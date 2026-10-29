//! Turns a time window of the file into trace polylines ready for the GPU.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::ops::Range;
use std::sync::Arc;

use rayon::prelude::*;

use crate::dsp::{self, FilterChain, FilterSettings, Point};
use crate::edf::{EdfFile, Window};
use crate::leads::{Kind, LeadSet};
use crate::montage::{Channel, Montage, SignalStats};

/// Decoded samples held at once while computing a window. Larger requests are split into
/// batches of channels, so memory stays bounded whatever the zoom level or channel count.
const BUDGET_SAMPLES: f64 = 12e6;

#[derive(Clone)]
pub struct TraceRequest {
    pub file: Arc<EdfFile>,
    pub montage: Arc<Montage>,
    pub visible: Arc<Vec<bool>>,
    /// Time span to compute, in seconds after the header start time.
    pub t0: f64,
    pub t1: f64,
    /// Horizontal resolution in physical pixels per second.
    pub px_per_sec: f64,
    pub filters: FilterSettings,
}

#[derive(Debug, Default)]
pub struct TraceData {
    pub t0: f64,
    pub t1: f64,
    pub px_per_sec: f64,
    /// Point x values are seconds relative to this time.
    pub anchor: f64,
    pub points: Vec<Point>,
    /// Range of `points` belonging to each montage channel (empty for hidden channels).
    pub ranges: Vec<Range<u32>>,
}

impl TraceData {
    /// Points of channel `ch` whose time falls in `[t0, t1]`.
    pub fn points_in(&self, ch: usize, t0: f64, t1: f64) -> &[Point] {
        let Some(r) = self.ranges.get(ch) else { return &[] };
        let pts = &self.points[r.start as usize..r.end as usize];
        let (a, b) = ((t0 - self.anchor) as f32, (t1 - self.anchor) as f32);
        let lo = pts.partition_point(|p| p.x < a);
        let hi = pts.partition_point(|p| p.x <= b);
        &pts[lo..hi.max(lo)]
    }
}

struct Mean {
    start: i64,
    data: Vec<f32>,
}

/// Computes the polylines of every visible channel. Returns `None` if `cancelled` turns true
/// part way (a newer request superseded this one).
pub fn compute(req: &TraceRequest, cancelled: &(dyn Fn() -> bool + Sync)) -> io::Result<Option<TraceData>> {
    let m = &req.montage;
    let visible: Vec<usize> = (0..m.channels.len())
        .filter(|&i| req.visible.get(i).copied().unwrap_or(!m.channels[i].hidden))
        .filter(|&i| m.channels[i].fs > 0.0)
        .collect();
    let any_filtered = visible.iter().any(|&i| m.channels[i].filtered);
    let pad = if any_filtered { dsp::padding_seconds(&req.filters) } else { 0.0 };
    let (r0, r1) = (req.t0 - pad, req.t1 + pad);
    let span = (r1 - r0).max(0.0);
    let fs_of = |s: u32| req.file.signals[s as usize].fs;

    let mean = if visible.iter().any(|&i| m.channels[i].minus_mean) && !m.mean_terms.is_empty() {
        Some(mean_signal(&req.file, &m.mean_terms, r0, r1)?)
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }

    // Group channels into batches whose signals fit the budget.
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut signals: BTreeSet<u32> = BTreeSet::new();
    let mut cost = 0.0;
    for &i in &visible {
        let new: Vec<u32> = m.channels[i].terms.iter().map(|t| t.0).filter(|s| !signals.contains(s)).collect();
        let extra: f64 = new.iter().map(|&s| span * fs_of(s)).sum();
        if !current.is_empty() && cost + extra > BUDGET_SAMPLES {
            batches.push(std::mem::take(&mut current));
            signals.clear();
            cost = 0.0;
        }
        for t in &m.channels[i].terms {
            if signals.insert(t.0) {
                cost += span * fs_of(t.0);
            }
        }
        current.push(i);
    }
    if !current.is_empty() {
        batches.push(current);
    }

    let mut chains: HashMap<u64, FilterChain> = HashMap::new();
    for &i in &visible {
        let fs = m.channels[i].fs;
        chains.entry(fs.to_bits()).or_insert_with(|| FilterChain::design(&req.filters, fs));
    }

    let anchor = req.t0;
    let mut per_channel: Vec<Vec<Point>> = vec![Vec::new(); m.channels.len()];
    for batch in batches {
        if cancelled() {
            return Ok(None);
        }
        let sigs: Vec<usize> = batch
            .iter()
            .flat_map(|&i| m.channels[i].terms.iter().map(|t| t.0 as usize))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let windows = req.file.read_window(&sigs, r0, r1)?;
        let by_signal: HashMap<u32, &Window> = windows.iter().map(|w| (w.signal as u32, w)).collect();
        let results: Vec<(usize, Vec<Point>)> = batch
            .par_iter()
            .map(|&i| {
                if cancelled() {
                    return (i, Vec::new());
                }
                let ch = &m.channels[i];
                let chain = chains.get(&ch.fs.to_bits()).filter(|c| ch.filtered && !c.is_empty());
                (i, process(ch, i as u32, &by_signal, mean.as_ref(), chain, req, anchor))
            })
            .collect();
        for (i, pts) in results {
            per_channel[i] = pts;
        }
    }
    if cancelled() {
        return Ok(None);
    }

    let total: usize = per_channel.iter().map(|p| p.len()).sum();
    let mut points = Vec::with_capacity(total);
    let mut ranges = Vec::with_capacity(per_channel.len());
    for pts in per_channel {
        let start = points.len() as u32;
        points.extend_from_slice(&pts);
        ranges.push(start..points.len() as u32);
    }
    Ok(Some(TraceData { t0: req.t0, t1: req.t1, px_per_sec: req.px_per_sec, anchor, points, ranges }))
}

fn process(
    ch: &Channel,
    idx: u32,
    windows: &HashMap<u32, &Window>,
    mean: Option<&Mean>,
    chain: Option<&FilterChain>,
    req: &TraceRequest,
    anchor: f64,
) -> Vec<Point> {
    let (k0, n) = match (ch.terms.first(), mean) {
        (Some(t), _) => {
            let w = windows[&t.0];
            (w.start, w.data.len())
        }
        (None, Some(m)) if ch.minus_mean => (m.start, m.data.len()),
        _ => return Vec::new(),
    };
    let mut buf = vec![0f32; n];
    for &(s, c) in &ch.terms {
        let w = windows[&s];
        for (b, v) in buf.iter_mut().zip(&w.data) {
            *b += c * v;
        }
    }
    if ch.minus_mean {
        if let Some(m) = mean {
            for (b, v) in buf.iter_mut().zip(&m.data) {
                *b -= v;
            }
        }
    }
    if let Some(chain) = chain {
        chain.filtfilt(&mut buf);
    }
    let fs = ch.fs;
    let keep = (req.t0 * fs).floor() as i64 - 1..(req.t1 * fs).ceil() as i64 + 2;
    let highpassed = chain.is_some() && req.filters.highpass.is_some();
    let level = if ch.ac && !highpassed {
        // Remove the offset over the visible part (amplifier DC, electrode drift).
        let a = (keep.start - k0).clamp(0, n as i64) as usize;
        let b = (keep.end - k0).clamp(0, n as i64) as usize;
        dsp::finite_mean(&buf[a..b]).unwrap_or(0.0)
    } else if !ch.ac {
        ch.center
    } else {
        0.0
    };
    if level != 0.0 {
        buf.iter_mut().for_each(|v| *v -= level);
    }
    let mut pts = Vec::new();
    dsp::decimate(&buf, k0, fs, anchor, keep, req.px_per_sec, idx, &mut pts);
    pts
}

fn mean_signal(file: &EdfFile, terms: &[(u32, f32)], r0: f64, r1: f64) -> io::Result<Mean> {
    let fs = file.signals[terms[0].0 as usize].fs;
    let start = (r0 * fs).floor() as i64;
    let n = ((r1 * fs).ceil() as i64 - start).max(0) as usize;
    let mut data = vec![0f32; n];
    let per_batch = ((BUDGET_SAMPLES / n.max(1) as f64) as usize).max(1);
    for chunk in terms.chunks(per_batch) {
        let sigs: Vec<usize> = chunk.iter().map(|t| t.0 as usize).collect();
        for (w, &(_, c)) in file.read_window(&sigs, r0, r1)?.iter().zip(chunk) {
            for (d, v) in data.iter_mut().zip(&w.data) {
                *d += c * v;
            }
        }
    }
    Ok(Mean { start, data })
}

/// Typical level and spread of every signal that is fitted to its lane (ECG, SpO2,
/// respiration, ...), from short windows spread across the recording. EEG-like signals use
/// the global sensitivity instead and keep the default.
pub fn signal_stats(file: &EdfFile, leads: &LeadSet) -> Vec<SignalStats> {
    let mut stats = vec![SignalStats::default(); file.signals.len()];
    let wanted: Vec<usize> = leads
        .leads
        .iter()
        .filter(|l| !l.sensitivity && l.kind != Kind::Annotation && file.signals[l.signal].fs > 0.0)
        .map(|l| l.signal)
        .collect();
    let (ta, tb) = file.time_range();
    if wanted.is_empty() || tb <= ta {
        return stats;
    }
    let win = 4.0f64.min(tb - ta);
    let n_windows = 12;
    let mut values: Vec<Vec<f32>> = vec![Vec::new(); wanted.len()];
    for w in 0..n_windows {
        let t = ta + (tb - ta - win) * (w as f64 + 0.5) / n_windows as f64;
        let Ok(windows) = file.read_window(&wanted, t, t + win) else { return stats };
        for (i, win) in windows.iter().enumerate() {
            let lead = &leads.leads[wanted[i]];
            let finite: Vec<f32> = win.data.iter().copied().filter(|v| v.is_finite()).collect();
            if finite.is_empty() {
                continue;
            }
            let level = if lead.ac { dsp::finite_mean(&finite).unwrap_or(0.0) } else { 0.0 };
            let step = (finite.len() / 2000).max(1);
            values[i].extend(finite.iter().step_by(step).map(|v| v - level));
        }
    }
    for (i, mut v) in values.into_iter().enumerate() {
        let s = wanted[i];
        let sig = &file.signals[s];
        let fallback = ((sig.phys_max - sig.phys_min).abs() / 2.0) as f32;
        if v.is_empty() {
            stats[s] = SignalStats { center: ((sig.phys_max + sig.phys_min) / 2.0) as f32, half_range: fallback.max(1e-6) };
            continue;
        }
        v.sort_by(f32::total_cmp);
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let (lo, hi) = (q(0.01), q(0.99));
        // Never fit tighter than 0.5% of the header range, so an unused input shows as a flat
        // line instead of amplified noise.
        let half = ((hi - lo) / 2.0).max(fallback * 0.005).max(1e-6);
        let ac = leads.leads[s].ac;
        stats[s] = SignalStats { center: if ac { 0.0 } else { (lo + hi) / 2.0 }, half_range: half };
    }
    stats
}
