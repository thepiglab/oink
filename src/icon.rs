//! The app icon, drawn in code so the repository needs no image assets: three EEG traces
//! (left blue, midline white, right red) on a rounded indigo square.

use eframe::egui;

/// RGBA pixels of the icon at `size` x `size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let n = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let radius = n * 0.225;
    let inset = n * 0.06;
    let traces: [(f32, [f32; 3], f32); 3] = [
        (0.36, [0.62, 0.76, 1.0], 0.0),
        (0.52, [1.0, 1.0, 1.0], 1.7),
        (0.68, [1.0, 0.62, 0.56], 3.1),
    ];
    let wave = |x: f32, phase: f32, spike: bool| -> f32 {
        let mut v = (x * 23.0 + phase).sin() * 0.022 + (x * 51.0 + phase * 2.0).sin() * 0.011;
        if spike {
            let d = (x - 0.56) / 0.025;
            v -= 0.07 * (-d * d).exp();
        }
        v
    };
    const SS: usize = 3;
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = px as f32 + (sx as f32 + 0.5) / SS as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SS as f32;
                    // Rounded square.
                    let qx = (x - n / 2.0).abs() - (n / 2.0 - inset - radius);
                    let qy = (y - n / 2.0).abs() - (n / 2.0 - inset - radius);
                    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
                    if outside > 0.0 {
                        continue;
                    }
                    let t = y / n;
                    let mut c = [0.25 - 0.09 * t, 0.24 - 0.09 * t, 0.62 - 0.22 * t];
                    let u = x / n;
                    if (0.17..=0.83).contains(&u) {
                        for (k, &(row, color, phase)) in traces.iter().enumerate() {
                            let yc = row + wave(u, phase, k == 1);
                            let slope = (wave(u + 0.002, phase, k == 1) - wave(u - 0.002, phase, k == 1)) / 0.004;
                            let d = ((t - yc) / (1.0 + slope * slope).sqrt()).abs() * n;
                            let w = (n * 0.021).max(1.0);
                            let a = (w - d + 0.5).clamp(0.0, 1.0);
                            for i in 0..3 {
                                c[i] = c[i] * (1.0 - a) + color[i] * a;
                            }
                        }
                    }
                    acc[0] += c[0];
                    acc[1] += c[1];
                    acc[2] += c[2];
                    acc[3] += 1.0;
                }
            }
            let i = ((py * size + px) * 4) as usize;
            let cov = acc[3] / (SS * SS) as f32;
            if acc[3] > 0.0 {
                for k in 0..3 {
                    out[i + k] = ((acc[k] / acc[3]).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                out[i + 3] = (cov * 255.0).round() as u8;
            }
        }
    }
    out
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
