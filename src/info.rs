//! `oink --info FILE`: what the viewer makes of a file, as text.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use oink_eeg::edf::EdfFile;
use oink_eeg::leads::{Kind, LeadSet, Reference};
use oink_eeg::montage;
use oink_eeg::pipeline;

pub fn print(path: &Path) -> Result<(), String> {
    let text = describe(path)?;
    // Ignore a closed pipe (`oink --info f.edf | head`).
    let _ = std::io::stdout().write_all(text.as_bytes());
    Ok(())
}

fn describe(path: &Path) -> Result<String, String> {
    let mut out = String::new();
    let file = EdfFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let leads = LeadSet::identify(&file.signals);
    let (t0, t1) = file.time_range();
    let start = file.clock(0.0);

    let _ = writeln!(out, "{}", file.file_name());
    let _ = writeln!(out, 
        "  {}{} · {} signals · {} records × {} s · {}",
        file.format.name(),
        if file.discontinuous { " (discontinuous)" } else { "" },
        file.signals.len(),
        file.n_records,
        file.record_duration,
        duration(t1 - t0)
    );
    let _ = writeln!(out, 
        "  starts {} {}",
        start.date.map(|d| d.to_string()).unwrap_or_else(|| "(no date)".into()),
        file.clock(t0).time_string(t0.fract() != 0.0)
    );
    if let Some(p) = &file.patient {
        let fields: Vec<String> = [("code", &p.code), ("sex", &p.sex), ("born", &p.birthdate), ("name", &p.name)]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}")))
            .collect();
        if !fields.is_empty() {
            let _ = writeln!(out, "  patient: {}", fields.join(", "));
        }
    }
    if let Some(eq) = file.recording.as_ref().and_then(|r| r.equipment.as_ref()) {
        let _ = writeln!(out, "  equipment: {eq}");
    }

    let s = &leads.summary;
    let _ = writeln!(out);
    let _ = writeln!(out, 
        "Leads: {} EEG ({} placed on the 10-20/10-10 map, {} of the 19 classic 10-20 sites){}{}",
        s.eeg,
        s.positioned,
        s.ten_twenty,
        s.reference.as_ref().map(|r| format!(", referenced to {r}")).unwrap_or_default(),
        if s.bipolar { ", recorded bipolar" } else { "" }
    );
    let others: Vec<String> =
        s.kinds.iter().filter(|(k, _)| *k != Kind::Eeg).map(|(k, n)| format!("{n} {}", k.name())).collect();
    if !others.is_empty() {
        let _ = writeln!(out, "  also {}", others.join(", "));
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "  {:>3}  {:<18} {:<10} {:<8} {:<10} {:>8}  unit / note", "#", "label", "shown as", "type", "reference", "Hz");
    for (l, sig) in leads.leads.iter().zip(&file.signals) {
        let reference = match &l.reference {
            Some(Reference::Electrode(_)) | Some(Reference::Named(_)) => l.reference_name.clone().unwrap_or_default(),
            Some(Reference::Unstated) => "(common)".into(),
            None => String::new(),
        };
        let fs = if sig.is_annotation { String::new() } else { format!("{:.6}", sig.fs).trim_end_matches('0').trim_end_matches('.').to_string() };
        let mut note = l.unit.display.clone();
        if let Some(n) = &l.note {
            note = format!("{note}  {n}");
        }
        let _ = writeln!(out, 
            "  {:>3}  {:<18} {:<10} {:<8} {:<10} {:>8}  {}",
            sig.index + 1,
            format!("{:?}", sig.label),
            l.display,
            l.kind.tag(),
            reference,
            fs,
            note
        );
    }

    let stats = pipeline::signal_stats(&file, &leads);
    let _ = writeln!(out);
    let _ = writeln!(out, "Montages:");
    for (id, m) in montage::catalog(&leads, &file.signals, &stats) {
        match m {
            Ok(m) => {
                let eeg = m.channels.iter().filter(|c| c.kind == Kind::Eeg).count();
                let missing = if m.missing.is_empty() { String::new() } else { format!(" (cannot form {})", m.missing.join(", ")) };
                let _ = writeln!(out, "  {:<14} {} channels, {eeg} EEG{missing}", id.name(), m.channels.len());
            }
            Err(reason) => {
                let _ = writeln!(out, "  {:<14} unavailable: {reason}", id.name());
            }
        }
    }

    let annotations = file.read_annotations().map_err(|e| e.to_string())?;
    if !annotations.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Annotations: {}", annotations.len());
        for a in annotations.iter().take(8) {
            let _ = writeln!(out, "  {}  {}", file.clock(a.onset).time_string(true), a.text);
        }
        if annotations.len() > 8 {
            let _ = writeln!(out, "  …");
        }
    }
    let gaps = file.gaps();
    if !gaps.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Gaps: {}", gaps.len());
    }
    if !file.warnings.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Warnings:");
        for w in &file.warnings {
            let _ = writeln!(out, "  - {w}");
        }
    }
    Ok(out)
}

fn duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
