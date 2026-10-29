//! Scalp maps: spectral measures of each scalp electrode against the average reference, and
//! spherical-spline interpolation (Perrin et al., 1989) to spread them over the head.
//!
//! Maps are drawn on the average reference whatever montage is on screen: band power belongs to
//! electrodes, while a bipolar channel's power describes the difference between two of them.

use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::io;

use rayon::prelude::*;
use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

use crate::edf::EdfFile;
use crate::leads::ElectrodeId;
use crate::montage::{Channel, Montage};

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub name: &'static str,
    pub symbol: &'static str,
    /// Edges in Hz; the lower edge is included, the upper one is not.
    pub lo: f64,
    pub hi: f64,
}

pub const BANDS: [Band; 5] = [
    Band { name: "Delta", symbol: "\u{3b4}", lo: 1.0, hi: 4.0 },
    Band { name: "Theta", symbol: "\u{3b8}", lo: 4.0, hi: 8.0 },
    Band { name: "Alpha", symbol: "\u{3b1}", lo: 8.0, hi: 13.0 },
    Band { name: "Beta", symbol: "\u{3b2}", lo: 13.0, hi: 30.0 },
    Band { name: "Gamma", symbol: "\u{3b3}", lo: 30.0, hi: 45.0 },
];

/// Relative powers are shares of this range, which leaves out slow drift and mains hum.
pub const TOTAL: (f64, f64) = (1.0, 45.0);

/// Shortest span a map is computed for.
pub const MIN_SPAN: f64 = 1.0;
/// Welch segment length: 2 s gives 0.5 Hz resolution.
const SEGMENT: f64 = 2.0;
/// Long spans are sampled with this many segments spread evenly across them, so a map of a
/// 30-minute page costs no more than one of two minutes.
const MAX_SEGMENTS: usize = 120;
/// Most samples read for one map. Caps with many channels at high rates get fewer segments
/// (but never fewer than `MIN_SEGMENTS` when the span has them).
const BUDGET_SAMPLES: usize = 24_000_000;
const MIN_SEGMENTS: usize = 16;

/// What a scalp map shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Measure {
    Delta,
    Theta,
    Alpha,
    Beta,
    Gamma,
    AlphaDelta,
    ThetaBeta,
    /// (delta + theta) / (alpha + beta).
    SlowFast,
}

impl Measure {
    pub const ALL: [Measure; 8] = [
        Measure::Delta,
        Measure::Theta,
        Measure::Alpha,
        Measure::Beta,
        Measure::Gamma,
        Measure::AlphaDelta,
        Measure::ThetaBeta,
        Measure::SlowFast,
    ];

    /// Index into `BANDS`, for band-power maps.
    pub fn band(self) -> Option<usize> {
        match self {
            Measure::Delta => Some(0),
            Measure::Theta => Some(1),
            Measure::Alpha => Some(2),
            Measure::Beta => Some(3),
            Measure::Gamma => Some(4),
            _ => None,
        }
    }

    pub fn is_ratio(self) -> bool {
        self.band().is_none()
    }

    pub fn label(self) -> &'static str {
        match self {
            Measure::AlphaDelta => "Alpha / delta",
            Measure::ThetaBeta => "Theta / beta",
            Measure::SlowFast => "Slow / fast",
            band => BANDS[band.band().unwrap_or(0)].name,
        }
    }

    /// The band's frequencies, or the ratio in symbols.
    pub fn detail(self) -> String {
        match self {
            Measure::AlphaDelta => "\u{3b1} / \u{3b4}".into(),
            Measure::ThetaBeta => "\u{3b8} / \u{3b2}".into(),
            Measure::SlowFast => "(\u{3b4} + \u{3b8}) / (\u{3b1} + \u{3b2})".into(),
            band => {
                let b = BANDS[band.band().unwrap_or(0)];
                format!("{}\u{2013}{} Hz", b.lo, b.hi)
            }
        }
    }

    /// Name for the command line.
    pub fn key(self) -> &'static str {
        match self {
            Measure::Delta => "delta",
            Measure::Theta => "theta",
            Measure::Alpha => "alpha",
            Measure::Beta => "beta",
            Measure::Gamma => "gamma",
            Measure::AlphaDelta => "alpha-delta",
            Measure::ThetaBeta => "theta-beta",
            Measure::SlowFast => "slow-fast",
        }
    }

    pub fn from_key(key: &str) -> Option<Measure> {
        Measure::ALL.into_iter().find(|m| m.key().eq_ignore_ascii_case(key))
    }

    /// Bands whose upper edge is below the Nyquist frequency `nyquist`.
    pub fn available(self, nyquist: f64) -> bool {
        let needs = |i: usize| BANDS[i].hi <= nyquist;
        match self {
            Measure::AlphaDelta => needs(2) && needs(0),
            Measure::ThetaBeta | Measure::SlowFast => needs(3),
            band => band.band().is_some_and(needs),
        }
    }

    /// The measure for one electrode: a percentage of 1-45 Hz power for bands, a power ratio
    /// otherwise. `None` where it is undefined.
    pub fn value(self, p: &BandPower) -> Option<f64> {
        let b = |i: usize| p.bands[i];
        let v = match self {
            Measure::AlphaDelta => b(2)? / b(0)?,
            Measure::ThetaBeta => b(1)? / b(3)?,
            Measure::SlowFast => (b(0)? + b(1)?) / (b(2)? + b(3)?),
            band => 100.0 * b(band.band()?)? / p.total,
        };
        v.is_finite().then_some(v)
    }
}

/// Power of one electrode in each band and in the 1-45 Hz total (consistent, arbitrary units).
#[derive(Clone, Debug)]
pub struct BandPower {
    pub electrode: ElectrodeId,
    /// `None` for bands beyond the Nyquist frequency.
    pub bands: [Option<f64>; 5],
    pub total: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Spectra {
    /// One per electrode of the average-reference montage, in its order.
    pub powers: Vec<BandPower>,
    /// Segments analysed, and those skipped because they touch a gap in the recording.
    pub segments: usize,
    pub skipped: usize,
    /// True when the span was too long to analyse whole and segments were spread across it.
    pub sampled: bool,
}

/// The electrodes of `avg` (the average-reference montage), in the order maps use.
pub fn electrodes(avg: &Montage) -> impl Iterator<Item = &Channel> {
    avg.channels.iter().filter(|c| c.minus_mean && c.a.is_some())
}

/// Band powers of every electrode of `avg` over `[t0, t1)`, by Welch's method: Hann-windowed
/// 2 s segments overlapping by half. Returns `None` if `cancelled` turns true.
pub fn band_powers(
    file: &EdfFile,
    avg: &Montage,
    t0: f64,
    t1: f64,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> io::Result<Option<Spectra>> {
    let chans: Vec<&Channel> = electrodes(avg).collect();
    let mut spectra = Spectra::default();
    let span = t1 - t0;
    let Some(fs) = chans.first().map(|c| c.fs) else { return Ok(Some(spectra)) };
    let seg = SEGMENT.min(span);
    let n = (seg * fs).round() as usize;
    if span < MIN_SPAN || n < 8 {
        return Ok(Some(spectra));
    }
    let mut signals: Vec<usize> =
        chans.iter().flat_map(|c| &c.terms).chain(&avg.mean_terms).map(|t| t.0 as usize).collect();
    signals.sort_unstable();
    signals.dedup();

    let hop = seg / 2.0;
    let count = ((span - seg) / hop + 1e-9).floor() as usize + 1;
    let most = (BUDGET_SAMPLES / (signals.len() * n).max(1)).clamp(MIN_SEGMENTS, MAX_SEGMENTS);
    spectra.sampled = count > most;
    let starts: Vec<f64> = if spectra.sampled {
        (0..most).map(|k| t0 + (span - seg) * k as f64 / (most - 1) as f64).collect()
    } else {
        (0..count).map(|k| t0 + k as f64 * hop).collect()
    };
    let slot = |s: u32| signals.binary_search(&(s as usize)).expect("signal was read");
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let hann: Vec<f32> = (0..n).map(|i| (0.5 - 0.5 * (TAU * i as f64 / n as f64).cos()) as f32).collect();
    let bins = n / 2 + 1;

    // Summed power spectra of every electrode over a run of segments, and how many were usable.
    type Run = io::Result<Option<(Vec<f64>, usize)>>;
    let run = |starts: &[f64]| -> Run {
        let mut acc = vec![0f64; chans.len() * bins];
        let mut used = 0;
        let (mut mean, mut x) = (vec![0f32; n], vec![0f32; n]);
        let (mut input, mut output) = (fft.make_input_vec(), fft.make_output_vec());
        for &ta in starts {
            if cancelled() {
                return Ok(None);
            }
            let windows = file.read_window(&signals, ta, ta + seg)?;
            if windows.iter().any(|w| w.data.len() < n || w.data[..n].iter().any(|v| !v.is_finite())) {
                continue;
            }
            mean.fill(0.0);
            for &(s, c) in &avg.mean_terms {
                for (m, v) in mean.iter_mut().zip(&windows[slot(s)].data) {
                    *m += c * v;
                }
            }
            for (e, ch) in chans.iter().enumerate() {
                for (xi, m) in x.iter_mut().zip(&mean) {
                    *xi = -m;
                }
                for &(s, c) in &ch.terms {
                    for (xi, v) in x.iter_mut().zip(&windows[slot(s)].data) {
                        *xi += c * v;
                    }
                }
                let level = (x.iter().map(|&v| v as f64).sum::<f64>() / n as f64) as f32;
                for ((i, &v), &h) in input.iter_mut().zip(&x).zip(&hann) {
                    *i = (v - level) * h;
                }
                fft.process(&mut input, &mut output).map_err(io::Error::other)?;
                for (a, z) in acc[e * bins..(e + 1) * bins].iter_mut().zip(&output) {
                    *a += z.norm_sqr() as f64;
                }
            }
            used += 1;
        }
        Ok(Some((acc, used)))
    };
    let parts: Vec<Run> = starts.par_chunks(4).map(run).collect();

    let mut acc = vec![0f64; chans.len() * bins];
    for part in parts {
        let Some((a, used)) = part? else { return Ok(None) };
        for (x, y) in acc.iter_mut().zip(a) {
            *x += y;
        }
        spectra.segments += used;
    }
    spectra.skipped = starts.len() - spectra.segments;
    if spectra.segments == 0 {
        return Ok(Some(spectra));
    }
    let nyquist = fs / 2.0;
    let df = fs / n as f64;
    let per = spectra.segments as f64;
    let sum = |p: &[f64], lo: f64, hi: f64| {
        p.iter().enumerate().filter(|(k, _)| (lo..hi).contains(&(*k as f64 * df))).map(|(_, v)| v).sum::<f64>() / per
    };
    for (e, ch) in chans.iter().enumerate() {
        let p = &acc[e * bins..(e + 1) * bins];
        spectra.powers.push(BandPower {
            electrode: ch.a.expect("electrode channel"),
            bands: BANDS.map(|b| (b.hi <= nyquist).then(|| sum(p, b.lo, b.hi))),
            total: sum(p, TOTAL.0, TOTAL.1.min(nyquist)),
        });
    }
    Ok(Some(spectra))
}

/// Spherical-spline interpolation of values at electrodes, evaluated on the head-map disc.
/// Head-map positions are an azimuthal equidistant projection of a spherical head (radius is
/// the angle from the vertex, 1 at the nasion level), so they map back onto the sphere exactly.
pub struct Interpolator {
    pos: Vec<[f64; 3]>,
    /// Inverse of the spline system [[G + λI, 1], [1ᵀ, 0]].
    inv: Vec<f64>,
    /// The spline kernel g sampled over cos(angle) in [-1, 1].
    kernel: Vec<f64>,
}

const KERNEL_SAMPLES: usize = 4096;
/// Added to the diagonal so that coincident electrodes cannot make the system singular. Small
/// enough that the map still passes through the electrode values: with many electrodes the
/// spline's smallest eigenvalues are around 1e-9.
const LAMBDA: f64 = 1e-11;

impl Interpolator {
    /// `points` are head-map positions. `None` for fewer than 4 points or a degenerate layout.
    pub fn new(points: &[(f32, f32)]) -> Option<Interpolator> {
        let n = points.len();
        if n < 4 {
            return None;
        }
        let pos: Vec<[f64; 3]> = points.iter().map(|&(x, y)| to_sphere(x as f64, y as f64)).collect();
        let kernel = (0..KERNEL_SAMPLES).map(|i| spline_kernel(-1.0 + 2.0 * i as f64 / (KERNEL_SAMPLES - 1) as f64)).collect();
        let mut it = Interpolator { pos, inv: Vec::new(), kernel };
        let m = n + 1;
        let mut a = vec![0f64; m * m];
        for i in 0..n {
            for j in 0..n {
                a[i * m + j] = it.g(dot(it.pos[i], it.pos[j])) + if i == j { LAMBDA } else { 0.0 };
            }
            a[i * m + n] = 1.0;
            a[n * m + i] = 1.0;
        }
        it.inv = invert(a, m)?;
        Some(it)
    }

    fn g(&self, cos: f64) -> f64 {
        let x = (cos.clamp(-1.0, 1.0) + 1.0) * 0.5 * (KERNEL_SAMPLES - 1) as f64;
        let i = (x as usize).min(KERNEL_SAMPLES - 2);
        let f = x - i as f64;
        self.kernel[i] * (1.0 - f) + self.kernel[i + 1] * f
    }

    /// Spline weights for `values` (one per point), with the constant term last.
    fn fit(&self, values: &[f64]) -> Vec<f64> {
        let m = self.pos.len() + 1;
        (0..m).map(|i| values.iter().enumerate().map(|(j, v)| self.inv[i * m + j] * v).sum()).collect()
    }

    fn eval(&self, weights: &[f64], p: [f64; 3]) -> f64 {
        let n = self.pos.len();
        weights[n] + self.pos.iter().zip(weights).map(|(q, w)| w * self.g(dot(p, *q))).sum::<f64>()
    }

    /// The interpolated value at head-map position `(x, y)`.
    pub fn value_at(&self, values: &[f64], x: f64, y: f64) -> f64 {
        self.eval(&self.fit(values), to_sphere(x, y))
    }

    /// Values on a `res` x `res` grid of cell centres spanning [-1, 1] in both axes, rows top
    /// (front of the head) to bottom; NaN beyond radius `limit`.
    pub fn grid(&self, values: &[f64], res: usize, limit: f64) -> Vec<f32> {
        let w = self.fit(values);
        let cell = 2.0 / res as f64;
        (0..res * res)
            .into_par_iter()
            .map(|k| {
                let (x, y) = (-1.0 + ((k % res) as f64 + 0.5) * cell, 1.0 - ((k / res) as f64 + 0.5) * cell);
                if x.hypot(y) > limit { f32::NAN } else { self.eval(&w, to_sphere(x, y)) as f32 }
            })
            .collect()
    }
}

fn to_sphere(x: f64, y: f64) -> [f64; 3] {
    let theta = x.hypot(y) * FRAC_PI_2;
    let phi = x.atan2(y);
    [theta.sin() * phi.sin(), theta.sin() * phi.cos(), theta.cos()]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// g(x) = 1/(4π) Σ (2n+1) / (n(n+1))^4 · P_n(x), the spline kernel of order 4, to n = 50.
fn spline_kernel(x: f64) -> f64 {
    let (mut p0, mut p1) = (1.0, x);
    let mut sum = 0.0;
    for n in 1..=50 {
        let n = n as f64;
        sum += (2.0 * n + 1.0) / (n * (n + 1.0)).powi(4) * p1;
        let p2 = ((2.0 * n + 1.0) * x * p1 - n * p0) / (n + 1.0);
        (p0, p1) = (p1, p2);
    }
    sum / (4.0 * PI)
}

/// Inverse of the `m` x `m` matrix `a` by Gauss-Jordan elimination with partial pivoting.
fn invert(mut a: Vec<f64>, m: usize) -> Option<Vec<f64>> {
    let mut inv = vec![0f64; m * m];
    for i in 0..m {
        inv[i * m + i] = 1.0;
    }
    for col in 0..m {
        let pivot = (col..m).max_by(|&i, &j| a[i * m + col].abs().total_cmp(&a[j * m + col].abs()))?;
        if a[pivot * m + col].abs() < 1e-14 {
            return None;
        }
        for k in 0..m {
            a.swap(col * m + k, pivot * m + k);
            inv.swap(col * m + k, pivot * m + k);
        }
        let d = a[col * m + col];
        for k in 0..m {
            a[col * m + k] /= d;
            inv[col * m + k] /= d;
        }
        for row in 0..m {
            let f = a[row * m + col];
            if row == col || f == 0.0 {
                continue;
            }
            for k in 0..m {
                a[row * m + k] -= f * a[col * m + k];
                inv[row * m + k] -= f * inv[col * m + k];
            }
        }
    }
    Some(inv)
}

/// Contour segments of `grid` (as from `Interpolator::grid`) at `level`, in head-map units, by
/// marching squares. Cells with a NaN corner are skipped.
pub fn isolines(grid: &[f32], res: usize, level: f32, out: &mut Vec<[[f32; 2]; 2]>) {
    let cell = 2.0 / res as f32;
    let at = |col: f32, row: f32| [-1.0 + (col + 0.5) * cell, 1.0 - (row + 0.5) * cell];
    for r in 0..res.saturating_sub(1) {
        for c in 0..res - 1 {
            // Corners clockwise from the top left; edge k joins corner k and k + 1.
            let v = [grid[r * res + c], grid[r * res + c + 1], grid[(r + 1) * res + c + 1], grid[(r + 1) * res + c]];
            if v.iter().any(|x| !x.is_finite()) {
                continue;
            }
            let corner = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
            let mut cross = [[0f32; 2]; 4];
            let mut found = 0;
            for k in 0..4 {
                let (a, b) = (v[k], v[(k + 1) % 4]);
                if (a >= level) != (b >= level) {
                    let t = (level - a) / (b - a);
                    let (p, q) = (corner[k], corner[(k + 1) % 4]);
                    cross[found] = at(c as f32 + p.0 + t * (q.0 - p.0), r as f32 + p.1 + t * (q.1 - p.1));
                    found += 1;
                }
            }
            match found {
                2 => out.push([cross[0], cross[1]]),
                4 => {
                    // A saddle: the centre decides which corners the contour separates.
                    let centre = (v[0] + v[1] + v[2] + v[3]) / 4.0;
                    if (centre >= level) == (v[0] >= level) {
                        out.push([cross[0], cross[1]]);
                        out.push([cross[2], cross[3]]);
                    } else {
                        out.push([cross[3], cross[0]]);
                        out.push([cross[1], cross[2]]);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::{TEN_TWENTY, electrode};

    fn ten_twenty() -> Vec<(f32, f32)> {
        TEN_TWENTY.iter().map(|n| electrode(n).unwrap().0.info()).map(|e| (e.x, e.y)).collect()
    }

    #[test]
    fn spline_passes_through_the_electrodes() {
        let pts = ten_twenty();
        let it = Interpolator::new(&pts).unwrap();
        let values: Vec<f64> = pts.iter().map(|&(x, y)| (3.0 * x as f64).sin() + 2.0 * y as f64 * y as f64).collect();
        for (&(x, y), v) in pts.iter().zip(&values) {
            let got = it.value_at(&values, x as f64, y as f64);
            assert!((got - v).abs() < 1e-3, "at ({x}, {y}): {got} vs {v}");
        }
    }

    #[test]
    fn dense_caps_stay_well_conditioned() {
        // Every scalp site of the 10-10 table, including T1/T2 right next to FT9/FT10.
        let pts: Vec<(f32, f32)> =
            crate::leads::electrodes().iter().filter(|e| e.site == crate::leads::Site::Scalp).map(|e| (e.x, e.y)).collect();
        assert!(pts.len() > 70);
        let it = Interpolator::new(&pts).unwrap();
        let values: Vec<f64> = pts.iter().map(|&(x, y)| (2.0 * x as f64).cos() * (1.0 + y as f64)).collect();
        for (&(x, y), v) in pts.iter().zip(&values) {
            assert!((it.value_at(&values, x as f64, y as f64) - v).abs() < 1e-3);
        }
        assert!(it.grid(&values, 48, 1.0).iter().all(|v| v.is_nan() || v.abs() < 10.0));
    }

    #[test]
    fn constant_values_give_a_flat_map() {
        let it = Interpolator::new(&ten_twenty()).unwrap();
        let grid = it.grid(&[7.5; 19], 32, 1.0);
        let inside: Vec<f32> = grid.iter().copied().filter(|v| v.is_finite()).collect();
        assert!(inside.len() > 600);
        assert!(inside.iter().all(|v| (v - 7.5).abs() < 1e-4), "{:?}", inside.iter().fold(f32::NAN, |a, &b| a.max(b)));
        assert!(grid[0].is_nan(), "the corner is outside the head");
    }

    #[test]
    fn isolines_of_a_ramp_follow_the_level() {
        let res = 16;
        let grid: Vec<f32> = (0..res * res).map(|k| -1.0 + ((k % res) as f32 + 0.5) * 2.0 / res as f32).collect();
        let mut segs = Vec::new();
        isolines(&grid, res, 0.3, &mut segs);
        assert_eq!(segs.len(), res - 1);
        assert!(segs.iter().flatten().all(|p| (p[0] - 0.3).abs() < 1e-5));
    }

    #[test]
    fn measures_from_band_powers() {
        let p = BandPower { electrode: ElectrodeId(0), bands: [Some(4.0), Some(2.0), Some(8.0), Some(4.0), None], total: 20.0 };
        assert_eq!(Measure::Alpha.value(&p), Some(40.0));
        assert_eq!(Measure::AlphaDelta.value(&p), Some(2.0));
        assert_eq!(Measure::ThetaBeta.value(&p), Some(0.5));
        assert_eq!(Measure::SlowFast.value(&p), Some(0.5));
        assert_eq!(Measure::Gamma.value(&p), None);
        assert!(!Measure::Gamma.available(32.0) && Measure::Beta.available(32.0));
        for m in Measure::ALL {
            assert_eq!(Measure::from_key(m.key()), Some(m));
        }
    }
}
