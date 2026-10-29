//! The app icon, drawn in code so the repository needs no image assets: a pink piglet wearing three
//! EEG electrodes (left blue, midline white, right red) in front of its traces, on a rounded indigo
//! square.

use eframe::egui;

type Rgb = [f32; 3];

const BLUE: Rgb = [0.62, 0.76, 1.0];
const WHITE: Rgb = [1.0, 1.0, 1.0];
const RED: Rgb = [1.0, 0.62, 0.56];
const INK: Rgb = [0.18, 0.16, 0.42];

/// RGBA pixels of the icon at `size` x `size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let n = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let (c, a) = shade([(px as f32 + 0.5) / n, (py as f32 + 0.5) / n], 1.0 / n);
            let i = ((py * size + px) * 4) as usize;
            for k in 0..3 {
                out[i + k] = (c[k].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

/// Colour and alpha at `p` (icon units, 0..1, y down); `px` is one pixel in icon units.
fn shade(p: [f32; 2], px: f32) -> (Rgb, f32) {
    // Coverage of a shape from its signed distance, anti-aliased over one pixel.
    let cover = |d: f32| (0.5 - d / px).clamp(0.0, 1.0);
    let (radius, inset) = (0.225, 0.06);
    let q = [(p[0] - 0.5).abs() - (0.5 - inset - radius), (p[1] - 0.5).abs() - (0.5 - inset - radius)];
    let alpha = cover(len(q[0].max(0.0), q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - radius);
    if alpha == 0.0 {
        return ([0.0; 3], 0.0);
    }
    let mut c = mix([0.25, 0.24, 0.62], [0.16, 0.15, 0.40], p[1]);

    // Traces behind the piglet.
    for (row, color, phase) in [(0.27, BLUE, 0.0), (0.52, WHITE, 1.7), (0.77, RED, 3.1)] {
        let y = |x: f32| {
            row + (x * 19.0 + phase).sin() * 0.016 + (x * 47.0 + phase * 2.0).sin() * 0.009 + (x * 83.0 + phase).sin() * 0.004
        };
        let slope = (y(p[0] + 0.002) - y(p[0] - 0.002)) / 0.004;
        let d = (p[1] - y(p[0])).abs() / (1.0 + slope * slope).sqrt() - 0.009;
        over(&mut c, color, 0.75 * cover(d));
    }

    // Soft shadow, ears, head.
    let head = ellipse(p, [0.5, 0.57], [0.315, 0.265]);
    over(&mut c, [0.08, 0.06, 0.22], 0.35 * smooth(ellipse(p, [0.5, 0.60], [0.32, 0.27]), 0.03));
    for s in [-1.0, 1.0] {
        let ear = |a: [f32; 2], b: [f32; 2], t: [f32; 2]| {
            let m = |v: [f32; 2]| [0.5 + s * (v[0] - 0.5), v[1]];
            triangle(p, m(a), m(b), m(t))
        };
        over(&mut c, [0.96, 0.58, 0.69], cover(ear([0.27, 0.45], [0.42, 0.36], [0.235, 0.27]) - 0.045));
        over(&mut c, [0.88, 0.42, 0.57], cover(ear([0.29, 0.42], [0.39, 0.37], [0.26, 0.315]) - 0.025));
    }
    over(&mut c, mix([1.0, 0.78, 0.84], [0.97, 0.62, 0.72], (p[1] - 0.3) / 0.54), cover(head));
    over(&mut c, WHITE, 0.18 * smooth(ellipse(p, [0.4, 0.4], [0.14, 0.07]), 0.04) * cover(head));

    // Electrodes on the forehead, chained like the head map's montage lines.
    let band = ellipse(p, [0.5, 0.57], [0.238, 0.21]).abs() - 0.004;
    if p[1] < 0.41 {
        over(&mut c, INK, 0.7 * cover(band));
    }
    for (x, y, color) in [(0.36, 0.40, BLUE), (0.5, 0.36, WHITE), (0.64, 0.40, RED)] {
        let r = len(p[0] - x, p[1] - y);
        over(&mut c, INK, cover(r - 0.036));
        over(&mut c, color, cover(r - 0.026));
        over(&mut c, INK, 0.6 * cover(r - 0.010));
    }

    // Face: cheeks, eyes, snout.
    for s in [-1.0, 1.0] {
        over(&mut c, [1.0, 0.45, 0.58], 0.4 * smooth(ellipse(p, [0.5 + s * 0.2, 0.64], [0.055, 0.035]), 0.01));
        let eye = [0.5 + s * 0.115, 0.535];
        over(&mut c, [0.17, 0.11, 0.22], cover(ellipse(p, eye, [0.031, 0.037])));
        over(&mut c, WHITE, cover(len(p[0] - eye[0] - 0.01, p[1] - eye[1] + 0.013) - 0.011));
    }
    over(&mut c, mix([0.98, 0.58, 0.69], [0.93, 0.48, 0.61], (p[1] - 0.56) / 0.18), cover(ellipse(p, [0.5, 0.655], [0.125, 0.09])));
    over(&mut c, WHITE, 0.25 * smooth(ellipse(p, [0.5, 0.6], [0.07, 0.02]), 0.012));
    for s in [-1.0, 1.0] {
        over(&mut c, [0.55, 0.2, 0.34], cover(ellipse(p, [0.5 + s * 0.045, 0.66], [0.02, 0.032])));
    }
    (c, alpha)
}

fn len(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn over(c: &mut Rgb, color: Rgb, a: f32) {
    *c = mix(*c, color, a);
}

/// Coverage that fades over `width` either side of the edge (soft shapes).
fn smooth(d: f32, width: f32) -> f32 {
    let t = (0.5 - d / (2.0 * width)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Approximate signed distance to an axis-aligned ellipse; exact on the edge, which is all
/// anti-aliasing needs.
fn ellipse(p: [f32; 2], c: [f32; 2], r: [f32; 2]) -> f32 {
    let (x, y) = ((p[0] - c[0]) / r[0], (p[1] - c[1]) / r[1]);
    let k0 = len(x, y);
    let k1 = len(x / r[0], y / r[1]);
    if k1 == 0.0 { -r[0].min(r[1]) } else { k0 * (k0 - 1.0) / k1 }
}

/// Signed distance to a triangle.
fn triangle(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    let sub = |u: [f32; 2], v: [f32; 2]| [u[0] - v[0], u[1] - v[1]];
    let dot = |u: [f32; 2], v: [f32; 2]| u[0] * v[0] + u[1] * v[1];
    let cross = |u: [f32; 2], v: [f32; 2]| u[0] * v[1] - u[1] * v[0];
    let (e0, e1, e2) = (sub(b, a), sub(c, b), sub(a, c));
    let s = cross(e0, e2).signum();
    let mut dist = f32::MAX;
    let mut side = f32::MAX;
    for (v, e) in [(sub(p, a), e0), (sub(p, b), e1), (sub(p, c), e2)] {
        let h = (dot(v, e) / dot(e, e)).clamp(0.0, 1.0);
        let q = [v[0] - e[0] * h, v[1] - e[1] * h];
        dist = dist.min(dot(q, q));
        side = side.min(s * cross(v, e));
    }
    -dist.sqrt() * side.signum()
}

pub fn icon_data(size: u32) -> egui::IconData {
    egui::IconData { rgba: rgba(size), width: size, height: size }
}

pub fn write_png(path: &std::path::Path, size: u32) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba(size))?;
    Ok(())
}
