//! Whole-recording spectral overview (a density spectral array) for the navigator strip.
//! Computed in the background, coarse to fine, so a long recording shows a usable overview
//! within a moment and sharpens as the rest fills in.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use realfft::RealFftPlanner;

use crate::edf::EdfFile;
use crate::leads::{Kind, LeadSet};

#[derive(Clone, Debug)]
pub struct Overview {
    pub cols: usize,
    pub rows: usize,
    /// Time span of the columns, seconds after the header start time.
    pub t0: f64,
    pub t1: f64,
    /// Frequency of the first and last row (Hz).
    pub f_lo: f32,
    pub f_hi: f32,
    /// Mean power across the chosen channels in dB, row-major with row 0 at `f_lo`.
    /// NaN where not computed yet or where the recording has a gap.
    pub db: Vec<f32>,
    pub done: bool,
}

const MAX_COLS: usize = 2048;
const ROWS: usize = 48;
const MAX_SIGNALS: usize = 24;

/// EEG signals to summarise: the largest group sharing a sampling rate, thinned to at most
/// `MAX_SIGNALS`. Falls back to other electrophysiological signals.
pub fn pick_signals(file: &EdfFile, leads: &LeadSet) -> Vec<usize> {
    for kinds in [&[Kind::Eeg][..], &[Kind::Eog, Kind::Emg, Kind::Ecg][..]] {
        let mut by_fs: std::collections::BTreeMap<u64, Vec<usize>> = Default::default();
        for l in &leads.leads {
            let fs = file.signals[l.signal].fs;
            if kinds.contains(&l.kind) && fs >= 20.0 {
                by_fs.entry(fs.to_bits()).or_default().push(l.signal);
            }
        }
        if let Some((_, group)) = by_fs.into_iter().max_by_key(|(_, v)| v.len()) {
            let step = group.len().div_ceil(MAX_SIGNALS);
            return group.into_iter().step_by(step).collect();
        }
    }
    Vec::new()
}

/// Columns in coarse-to-fine order: every 1024th, then every 512th, ...
fn progressive_order(n: usize) -> Vec<usize> {
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut step = n.next_power_of_two();
    loop {
        for i in (0..n).step_by(step) {
            if !seen[i] {
                seen[i] = true;
                order.push(i);
            }
        }
        if step == 1 {
            break;
        }
        step /= 2;
    }
    order
}

/// Computes the overview, calling `publish` with progress every ~150 ms and once at the end.
pub fn compute(file: &EdfFile, signals: &[usize], cancel: &AtomicBool, mut publish: impl FnMut(&Overview)) {
    let (t0, t1) = file.time_range();
    let duration = t1 - t0;
    let Some(&first) = signals.first() else { return };
    let fs = file.signals[first].fs;
    let nfft = ((2.0 * fs).round() as usize).next_power_of_two().clamp(64, 16_384);
    let win = nfft as f64 / fs;
    if duration < win || fs <= 0.0 {
        return;
    }
    let cols = ((duration / 2.0).ceil() as usize).clamp(1, MAX_COLS);
    let col_span = duration / cols as f64;
    let mut per_col = ((col_span / win).floor() as usize).clamp(1, 3);
    let bytes = cols as f64 * per_col as f64 * win * fs * signals.len() as f64 * file.sample_bytes() as f64;
    if bytes > 1.5e9 {
        per_col = 1;
    }
    let f_lo = 0.5f32;
    let f_hi = (0.45 * fs).min(40.0) as f32;

    let mut ov = Overview { cols, rows: ROWS, t0, t1, f_lo, f_hi, db: vec![f32::NAN; ROWS * cols], done: false };
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(nfft);
    let hann: Vec<f32> =
        (0..nfft).map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / nfft as f64).cos()) as f32).collect();

    let column = |c: usize| -> Option<[f32; ROWS]> {
        let start = t0 + c as f64 * col_span;
        let mut power = vec![0f64; nfft / 2 + 1];
        let mut count = 0usize;
        let mut input = fft.make_input_vec();
        let mut spectrum = fft.make_output_vec();
        for w in 0..per_col {
            let center = start + (w as f64 + 0.5) * col_span / per_col as f64;
            let ta = (center - win / 2.0).clamp(t0, t1 - win);
            let windows = file.read_window(signals, ta, ta + win).ok()?;
            for window in &windows {
                if window.data.len() < nfft || window.data[..nfft].iter().any(|v| !v.is_finite()) {
                    continue;
                }
                let mean = window.data[..nfft].iter().map(|&v| v as f64).sum::<f64>() / nfft as f64;
                for ((x, &v), &h) in input.iter_mut().zip(&window.data[..nfft]).zip(&hann) {
                    *x = (v - mean as f32) * h;
                }
                if fft.process(&mut input, &mut spectrum).is_err() {
                    continue;
                }
                for (p, z) in power.iter_mut().zip(&spectrum) {
                    *p += z.norm_sqr() as f64;
                }
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        let mut out = [0f32; ROWS];
        for (r, o) in out.iter_mut().enumerate() {
            let f = f_lo as f64 + (r as f64 + 0.5) * (f_hi - f_lo) as f64 / ROWS as f64;
            let bin = f * nfft as f64 / fs;
            let (i, frac) = (bin.floor() as usize, bin.fract());
            let p = power[i.min(power.len() - 1)] * (1.0 - frac) + power[(i + 1).min(power.len() - 1)] * frac;
            *o = (10.0 * (p / count as f64 + 1e-20).log10()) as f32;
        }
        Some(out)
    };

    let mut last = Instant::now();
    for chunk in progressive_order(cols).chunks(48) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let results: Vec<(usize, Option<[f32; ROWS]>)> = chunk.par_iter().map(|&c| (c, column(c))).collect();
        for (c, col) in results {
            if let Some(col) = col {
                for (r, v) in col.iter().enumerate() {
                    ov.db[r * cols + c] = *v;
                }
            }
        }
        if last.elapsed() > Duration::from_millis(150) {
            publish(&ov);
            last = Instant::now();
        }
    }
    ov.done = true;
    publish(&ov);
}

#[cfg(test)]
mod tests {
    #[test]
    fn progressive_order_covers_everything_once() {
        for n in [1, 2, 7, 64, 1000] {
            let mut o = super::progressive_order(n);
            assert_eq!(o[0], 0);
            o.sort_unstable();
            assert_eq!(o, (0..n).collect::<Vec<_>>());
        }
    }
}
