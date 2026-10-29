//! Display filters and decimation.

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

/// The global EEG display filters (Hz). `None` switches a filter off.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterSettings {
    pub highpass: Option<f32>,
    pub lowpass: Option<f32>,
    pub notch: Option<f32>,
}

impl Default for FilterSettings {
    fn default() -> Self {
        FilterSettings { highpass: Some(0.5), lowpass: Some(70.0), notch: None }
    }
}

const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
const NOTCH_Q: f64 = 30.0;
/// Each second-order section runs forwards and backwards (zero phase), which squares its
/// magnitude response. Shifting the design cutoff by this factor (in the bilinear transform's
/// warped frequency) puts the -3 dB point of the combined response at the nominal frequency:
/// (sqrt(2) - 1)^(1/4).
const FILTFILT_CORRECTION: f64 = 0.802_255_4;

/// Design frequency for a Butterworth section whose forward-backward response is -3 dB at `f`.
fn corrected_cutoff(f: f64, fs: f64, highpass: bool) -> f64 {
    let warped = (std::f64::consts::PI * f / fs).tan();
    let warped = if highpass { warped * FILTFILT_CORRECTION } else { warped / FILTFILT_CORRECTION };
    warped.atan() * fs / std::f64::consts::PI
}

/// Second-order IIR section (RBJ cookbook), normalised so a0 = 1.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 3]) -> Self {
        Biquad { b0: b[0] / a[0], b1: b[1] / a[0], b2: b[2] / a[0], a1: a[1] / a[0], a2: a[2] / a[0] }
    }

    pub fn lowpass(fs: f64, fc: f64, q: f64) -> Self {
        let w = std::f64::consts::TAU * fc / fs;
        let (cw, alpha) = (w.cos(), w.sin() / (2.0 * q));
        Self::new([(1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0], [1.0 + alpha, -2.0 * cw, 1.0 - alpha])
    }

    pub fn highpass(fs: f64, fc: f64, q: f64) -> Self {
        let w = std::f64::consts::TAU * fc / fs;
        let (cw, alpha) = (w.cos(), w.sin() / (2.0 * q));
        Self::new([(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0], [1.0 + alpha, -2.0 * cw, 1.0 - alpha])
    }

    pub fn notch(fs: f64, f0: f64, q: f64) -> Self {
        let w = std::f64::consts::TAU * f0 / fs;
        let (cw, alpha) = (w.cos(), w.sin() / (2.0 * q));
        Self::new([1.0, -2.0 * cw, 1.0], [1.0 + alpha, -2.0 * cw, 1.0 - alpha])
    }

    fn dc_gain(&self) -> f64 {
        let den = 1.0 + self.a1 + self.a2;
        if den.abs() < 1e-300 { 0.0 } else { (self.b0 + self.b1 + self.b2) / den }
    }

    /// Single-pass magnitude response at `f` Hz.
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        let w = std::f64::consts::TAU * f / fs;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let num = (self.b0 + self.b1 * c1 + self.b2 * c2, -(self.b1 * s1 + self.b2 * s2));
        let den = (1.0 + self.a1 * c1 + self.a2 * c2, -(self.a1 * s1 + self.a2 * s2));
        (num.0.hypot(num.1)) / den.0.hypot(den.1)
    }

    /// Filters in place (transposed direct form II). The state starts as if the input had
    /// always been at its first value, so a DC offset does not ring at the start.
    fn run(&self, x: &mut [f32], reverse: bool) {
        let Some(&first) = (if reverse { x.last() } else { x.first() }) else { return };
        let x0 = first as f64;
        let y0 = self.dc_gain() * x0;
        let mut s2 = self.b2 * x0 - self.a2 * y0;
        let mut s1 = self.b1 * x0 - self.a1 * y0 + s2;
        let mut step = |v: &mut f32| {
            let xi = *v as f64;
            let yi = self.b0 * xi + s1;
            s1 = self.b1 * xi - self.a1 * yi + s2;
            s2 = self.b2 * xi - self.a2 * yi;
            *v = yi as f32;
        };
        if reverse {
            x.iter_mut().rev().for_each(&mut step);
        } else {
            x.iter_mut().for_each(&mut step);
        }
    }
}

/// A cascade of sections applied forwards then backwards (zero phase).
#[derive(Clone, Debug, Default)]
pub struct FilterChain {
    sections: Vec<Biquad>,
}

impl FilterChain {
    /// Butterworth high-pass and low-pass plus a notch, with cutoffs corrected for the
    /// forward-backward pass. Filters too close to Nyquist for this rate are skipped.
    pub fn design(settings: &FilterSettings, fs: f64) -> Self {
        let mut sections = Vec::new();
        let nyquist = fs / 2.0;
        if let Some(f) = settings.highpass.filter(|f| (*f as f64) < 0.45 * fs) {
            let fc = corrected_cutoff(f as f64, fs, true);
            if fc > 0.0 {
                sections.push(Biquad::highpass(fs, fc, BUTTERWORTH_Q));
            }
        }
        if let Some(f) = settings.lowpass.filter(|f| (*f as f64) < 0.45 * fs) {
            let fc = corrected_cutoff(f as f64, fs, false);
            if fc < 0.49 * fs {
                sections.push(Biquad::lowpass(fs, fc, BUTTERWORTH_Q));
            }
        }
        if let Some(f) = settings.notch {
            let f = f as f64;
            if f < 0.95 * nyquist {
                sections.push(Biquad::notch(fs, f, NOTCH_Q));
            }
        }
        FilterChain { sections }
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Zero-phase filtering. NaN marks gaps; each stretch between gaps is filtered on its own.
    pub fn filtfilt(&self, x: &mut [f32]) {
        if self.sections.is_empty() {
            return;
        }
        let mut i = 0;
        while i < x.len() {
            if !x[i].is_finite() {
                i += 1;
                continue;
            }
            let start = i;
            while i < x.len() && x[i].is_finite() {
                i += 1;
            }
            let run = &mut x[start..i];
            for s in &self.sections {
                s.run(run, false);
            }
            for s in self.sections.iter().rev() {
                s.run(run, true);
            }
        }
    }

    /// Magnitude of the combined zero-phase response at `f` Hz.
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        self.sections.iter().map(|s| s.magnitude(f, fs).powi(2)).product()
    }
}

/// Extra data to read on each side of a window so filter start-up transients fall outside it.
pub fn padding_seconds(settings: &FilterSettings) -> f64 {
    let mut pad: f64 = 0.0;
    if let Some(f) = settings.highpass {
        pad = pad.max(3.0 / f.max(0.01) as f64);
    }
    if let Some(f) = settings.lowpass {
        pad = pad.max(4.0 / f.max(0.1) as f64);
    }
    if let Some(f) = settings.notch {
        pad = pad.max(3.0 * NOTCH_Q / (std::f64::consts::PI * f.max(1.0) as f64));
    }
    pad.clamp(0.0, 30.0)
}

/// One vertex of a trace polyline, as uploaded to the GPU.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Point {
    /// Seconds relative to the trace data's anchor time.
    pub x: f32,
    /// Value in display units.
    pub y: f32,
    /// Montage channel index, or [`BREAK`].
    pub ch: u32,
}

/// Marks a gap: no segment is drawn to or from this point. It keeps the previous point's x so
/// x stays sorted within a channel.
pub const BREAK: u32 = u32::MAX;

/// Turns samples into a polyline for drawing. When samples are denser than pixels, each pixel
/// column keeps only its first, minimum, maximum and last sample ("M4"), which rasterises the
/// same as drawing every sample. Columns sit on a grid fixed to absolute time, so traces do not
/// shimmer when the window is recomputed at a different offset.
///
/// `y` holds samples `k0..k0 + y.len()` of a signal sampled at `fs` (sample `k` is at `k / fs`
/// seconds); only samples in `keep` are emitted.
pub fn decimate(y: &[f32], k0: i64, fs: f64, t_anchor: f64, keep: Range<i64>, px_per_sec: f64, ch: u32, out: &mut Vec<Point>) {
    let lo = keep.start.max(k0);
    let hi = keep.end.min(k0 + y.len() as i64);
    if lo >= hi {
        return;
    }
    let x_of = |k: i64| (k as f64 / fs - t_anchor) as f32;
    let px_per_sample = px_per_sec / fs;
    let gap = |out: &mut Vec<Point>| {
        if let Some(last) = out.last().copied().filter(|p| p.ch != BREAK) {
            out.push(Point { x: last.x, y: 0.0, ch: BREAK });
        }
    };

    if px_per_sample >= 0.6 {
        for k in lo..hi {
            let v = y[(k - k0) as usize];
            if v.is_finite() {
                out.push(Point { x: x_of(k), y: v, ch });
            } else {
                gap(out);
            }
        }
        gap(out);
        return;
    }

    struct Column {
        id: i64,
        first: (i64, f32),
        last: (i64, f32),
        min: (i64, f32),
        max: (i64, f32),
    }
    let flush = |c: &Column, out: &mut Vec<Point>| {
        let mut pts = [c.first, c.min, c.max, c.last];
        pts.sort_by_key(|p| p.0);
        let mut prev = i64::MIN;
        for (k, v) in pts {
            if k != prev {
                out.push(Point { x: x_of(k), y: v, ch });
                prev = k;
            }
        }
    };
    let mut col: Option<Column> = None;
    for k in lo..hi {
        let v = y[(k - k0) as usize];
        if !v.is_finite() {
            if let Some(c) = col.take() {
                flush(&c, out);
            }
            gap(out);
            continue;
        }
        let id = (k as f64 * px_per_sample).floor() as i64;
        match &mut col {
            Some(c) if c.id == id => {
                c.last = (k, v);
                if v < c.min.1 {
                    c.min = (k, v);
                }
                if v > c.max.1 {
                    c.max = (k, v);
                }
            }
            _ => {
                if let Some(c) = col.take() {
                    flush(&c, out);
                }
                col = Some(Column { id, first: (k, v), last: (k, v), min: (k, v), max: (k, v) });
            }
        }
    }
    if let Some(c) = col.take() {
        flush(&c, out);
    }
    gap(out);
}

/// Mean of the finite values in `x`.
pub fn finite_mean(x: &[f32]) -> Option<f32> {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for v in x.iter().filter(|v| v.is_finite()) {
        sum += *v as f64;
        n += 1;
    }
    (n > 0).then(|| (sum / n as f64) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoffs_are_minus_3db() {
        let fs = 256.0;
        let s = FilterSettings { highpass: Some(1.0), lowpass: Some(30.0), notch: None };
        let chain = FilterChain::design(&s, fs);
        let db = |f: f64| 20.0 * chain.magnitude(f, fs).log10();
        assert!((db(1.0) + 3.0).abs() < 0.2, "high-pass at 1 Hz: {} dB", db(1.0));
        assert!((db(30.0) + 3.0).abs() < 0.2, "low-pass at 30 Hz: {} dB", db(30.0));
        assert!(db(10.0).abs() < 0.1);
        let notch = FilterChain::design(&FilterSettings { highpass: None, lowpass: None, notch: Some(60.0) }, fs);
        assert!(notch.magnitude(60.0, fs) < 1e-3);
        assert!(notch.magnitude(50.0, fs) > 0.95);
    }

    #[test]
    fn filtfilt_removes_dc_without_phase_shift() {
        let fs = 200.0;
        let chain = FilterChain::design(&FilterSettings { highpass: Some(0.5), lowpass: None, notch: None }, fs);
        let n = 4000;
        let mut x: Vec<f32> = (0..n).map(|i| 500.0 + (std::f64::consts::TAU * 10.0 * i as f64 / fs).sin() as f32).collect();
        chain.filtfilt(&mut x);
        // Away from the edges: the offset is gone and the 10 Hz sine is in phase.
        for i in 1000..3000 {
            let expect = (std::f64::consts::TAU * 10.0 * i as f64 / fs).sin() as f32;
            assert!((x[i] - expect).abs() < 0.02, "sample {i}: {} vs {expect}", x[i]);
        }
    }

    #[test]
    fn gaps_split_filtering() {
        let chain = FilterChain::design(&FilterSettings::default(), 100.0);
        let mut x = vec![1.0f32; 300];
        x[150] = f32::NAN;
        chain.filtfilt(&mut x);
        assert!(x[150].is_nan());
        assert!(x.iter().enumerate().all(|(i, v)| i == 150 || v.is_finite()));
    }

    #[test]
    fn decimation_keeps_extremes_and_breaks_at_gaps() {
        let fs = 1000.0;
        let mut y: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.01).sin()).collect();
        y[5000] = 7.0;
        y[6000] = -9.0;
        y[8000] = f32::NAN;
        let mut out = Vec::new();
        decimate(&y, 0, fs, 0.0, 0..10_000, 100.0, 3, &mut out); // 10 samples per pixel
        assert!(out.len() < 4200);
        let ys: Vec<f32> = out.iter().filter(|p| p.ch == 3).map(|p| p.y).collect();
        assert!(ys.contains(&7.0) && ys.contains(&-9.0));
        assert!(out.iter().any(|p| p.ch == BREAK));
        assert!(out.windows(2).all(|w| w[0].x <= w[1].x));

        // Sparse: one point per sample.
        let mut out = Vec::new();
        decimate(&y[..100], 0, fs, 0.0, 10..20, 5000.0, 0, &mut out);
        assert_eq!(out.iter().filter(|p| p.ch == 0).count(), 10);
        assert!((out[0].x - 0.010).abs() < 1e-6);
    }
}
