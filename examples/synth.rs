//! Writes a synthetic EDF+ recording for benchmarking and demos:
//! `cargo run --release --example synth -- OUT.edf MINUTES FS CHANNELS`
//!
//! Channels use 10-10 names with a common reference, plus an ECG. The EEG has a posterior
//! alpha rhythm, some slow waves, noise and occasional spikes; annotations every few minutes.

use std::io::{BufWriter, Write};

const NAMES: [&str; 64] = [
    "Fp1", "Fpz", "Fp2", "AF7", "AF3", "AFz", "AF4", "AF8", "F7", "F5", "F3", "F1", "Fz", "F2", "F4", "F6", "F8", "FT7",
    "FC5", "FC3", "FC1", "FCz", "FC2", "FC4", "FC6", "FT8", "T7", "C5", "C3", "C1", "Cz", "C2", "C4", "C6", "T8", "TP7",
    "CP5", "CP3", "CP1", "CPz", "CP2", "CP4", "CP6", "TP8", "P7", "P5", "P3", "P1", "Pz", "P2", "P4", "P6", "P8", "PO7",
    "PO3", "POz", "PO4", "PO8", "O1", "Oz", "O2", "Iz", "M1", "M2",
];

fn put(out: &mut Vec<u8>, s: &str, w: usize) {
    let mut b = s.as_bytes().to_vec();
    b.resize(w, b' ');
    out.extend_from_slice(&b[..w]);
}

fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let path = a.first().cloned().unwrap_or_else(|| "synthetic.edf".into());
    let minutes: usize = a.get(1).and_then(|v| v.parse().ok()).unwrap_or(10);
    let fs: usize = a.get(2).and_then(|v| v.parse().ok()).unwrap_or(256);
    let n_eeg: usize = a.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(32).min(NAMES.len());
    let records = minutes * 60;
    let ns = n_eeg + 2; // + ECG + annotations
    let ann_spr = 60;

    let mut h = Vec::new();
    put(&mut h, "0", 8);
    put(&mut h, "X X X X", 80);
    put(&mut h, "Startdate 29-OCT-2026 X X synthetic", 80);
    put(&mut h, "29.10.26", 8);
    put(&mut h, "22.00.00", 8);
    put(&mut h, &(256 * (ns + 1)).to_string(), 8);
    put(&mut h, "EDF+C", 44);
    put(&mut h, &records.to_string(), 8);
    put(&mut h, "1", 8);
    put(&mut h, &ns.to_string(), 4);
    let labels: Vec<String> =
        NAMES[..n_eeg].iter().map(|n| format!("EEG {n}-Ref")).chain(["ECG II".to_string(), "EDF Annotations".to_string()]).collect();
    for l in &labels {
        put(&mut h, l, 16);
    }
    for _ in 0..ns {
        put(&mut h, "AgAgCl electrode", 80);
    }
    for i in 0..ns {
        put(&mut h, if i < n_eeg { "uV" } else if i == n_eeg { "mV" } else { "" }, 8);
    }
    for i in 0..ns {
        put(&mut h, if i < n_eeg { "-3276.8" } else if i == n_eeg { "-5" } else { "-1" }, 8);
    }
    for i in 0..ns {
        put(&mut h, if i < n_eeg { "3276.7" } else if i == n_eeg { "5" } else { "1" }, 8);
    }
    for _ in 0..ns {
        put(&mut h, "-32768", 8);
    }
    for _ in 0..ns {
        put(&mut h, "32767", 8);
    }
    for _ in 0..ns {
        put(&mut h, "HP:0.1Hz LP:100Hz", 80);
    }
    for i in 0..ns {
        put(&mut h, &if i <= n_eeg { fs } else { ann_spr }.to_string(), 8);
    }
    for _ in 0..ns {
        put(&mut h, "", 32);
    }

    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path)?);
    out.write_all(&h)?;
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut noise = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let tau = std::f64::consts::TAU;
    let mut record = Vec::with_capacity(ns * fs * 2);
    for r in 0..records {
        record.clear();
        for c in 0..n_eeg {
            let name = NAMES[c];
            let posterior = name.starts_with('O') || name.starts_with("PO") || name.starts_with('P');
            let left = name.chars().last().is_some_and(|d| d.is_ascii_digit() && (d as u8 - b'0') % 2 == 1);
            for j in 0..fs {
                let t = r as f64 + j as f64 / fs as f64;
                // Drowsy stretch every 20 minutes: more theta, less alpha.
                let drowsy = ((t / 1200.0).fract() > 0.7) as i32 as f64;
                let alpha = if posterior { 28.0 } else { 9.0 } * (1.0 - 0.7 * drowsy) * (tau * 10.2 * t + c as f64 * 0.3).sin();
                let theta = (6.0 + 22.0 * drowsy) * (tau * 5.5 * t + c as f64).sin();
                let slow = 14.0 * (tau * 0.7 * t + c as f64 * 0.11).sin();
                let spike = if left && (t % 37.0) < 0.07 { -90.0 * (std::f64::consts::PI * (t % 37.0) / 0.07).sin() } else { 0.0 };
                let v = alpha + theta + slow + spike + 6.0 * noise() + 40.0;
                record.extend_from_slice(&((v * 10.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
            }
        }
        for j in 0..fs {
            let t = r as f64 + j as f64 / fs as f64;
            let phase = (t * 1.15).fract();
            let qrs = (-((phase - 0.2) / 0.012).powi(2)).exp() - 0.15 * (-((phase - 0.45) / 0.05).powi(2)).exp();
            let v = qrs * 1.2 + 0.02 * noise();
            record.extend_from_slice(&((v / 5.0 * 32767.0) as i16).to_le_bytes());
        }
        let mut tal = format!("+{r}\x14\x14\x00").into_bytes();
        if r > 0 && r % 300 == 0 {
            tal.extend_from_slice(format!("+{r}\x14Marker {}\x14\x00", r / 300).as_bytes());
        }
        tal.resize(ann_spr * 2, 0);
        record.extend_from_slice(&tal);
        out.write_all(&record)?;
    }
    out.flush()?;
    println!("wrote {path}: {n_eeg} EEG channels at {fs} Hz, {minutes} min");
    Ok(())
}
