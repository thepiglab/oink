//! The sidebar: montage on the head, events, and file details.

use eframe::egui::{self, Align, CornerRadius, Layout, RichText, ScrollArea, Sense, vec2};
use oink_eeg::edf::AnnotationSource;
use oink_eeg::leads::{Kind, Numbering};
use serde::{Deserialize, Serialize};

use super::doc::Doc;
use super::headmap::{self, HeadMap};
use super::theme::{self, Palette};
use super::traces::{fmt_clock, fmt_hz, fmt_seconds, trace_color};
use super::widgets;
use super::{Settings, focus_search_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tab {
    Montage,
    Events,
    File,
}

pub fn ui(ui: &mut egui::Ui, doc: &mut Doc, s: &mut Settings) {
    let pal = theme::palette(ui.ctx());
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let events = if doc.annotations_ready && !doc.annotations.is_empty() {
            format!("Events {}", doc.annotations.len())
        } else {
            "Events".to_string()
        };
        widgets::segmented(ui, pal, &mut s.tab, &[(Tab::Montage, "Montage"), (Tab::Events, &events), (Tab::File, "File")]);
    });
    ui.add_space(8.0);
    match s.tab {
        Tab::Montage => montage_tab(ui, doc, s, pal),
        Tab::Events => events_tab(ui, doc, s, pal),
        Tab::File => file_tab(ui, doc, pal),
    }
}

fn montage_tab(ui: &mut egui::Ui, doc: &mut Doc, s: &Settings, pal: &Palette) {
    ui.label(RichText::new(doc.montage_name()).font(theme::font(15.0)));
    ui.label(RichText::new(headmap::caption(&doc.montage)).color(pal.muted).small());

    let size = ui.available_width().min(300.0);
    let (rect, resp) = headmap::allocate(ui, size);
    let highlight: Vec<usize> = doc.hover_channel.into_iter().chain(doc.selected_channel).collect();
    let pointer = resp.hover_pos();
    let map = HeadMap {
        montage: &doc.montage,
        leads: &doc.leads,
        visible: Some(&doc.visible),
        highlight: &highlight,
        highlight_electrode: doc.hover_electrode,
        hemisphere_colors: s.hemisphere_colors,
        detailed: true,
    };
    let hovered = map.draw(ui.painter(), rect, pal, pointer);
    if let Some(e) = hovered {
        let names: Vec<String> = doc.channels_with(e).map(|i| doc.montage.channels[i].label.clone()).collect();
        let info = e.info();
        let lead = doc.leads.leads.iter().find(|l| l.electrode == Some(e));
        resp.on_hover_ui_at_pointer(|ui| {
            ui.label(RichText::new(lead.and_then(|l| l.electrode_name.clone()).unwrap_or_else(|| info.name.clone())).strong());
            if let Some(l) = lead {
                ui.label(RichText::new(format!("from \"{}\"", l.label)).color(pal.muted).small());
                if let Some(n) = &l.note {
                    ui.label(RichText::new(n).color(pal.muted).small());
                }
            }
            if !names.is_empty() {
                ui.label(RichText::new(names.join(", ")).small());
            }
        });
    }
    doc.hover_electrode = hovered;

    // What was identified.
    let sum = &doc.leads.summary;
    ui.add_space(6.0);
    widgets::heading(ui, pal, "Leads");
    let mut line = format!("{} EEG", sum.eeg);
    if sum.ten_twenty > 0 {
        line += &format!(" \u{b7} {}/19 standard 10-20 sites", sum.ten_twenty);
    }
    ui.label(line);
    let mut detail = Vec::new();
    if let Some(r) = &sum.reference {
        detail.push(format!("referenced to {r}"));
    } else if sum.bipolar {
        detail.push("recorded bipolar".to_string());
    }
    if sum.legacy_names {
        detail.push("T3/T4/T5/T6 naming".to_string());
    }
    match sum.numbering {
        Some(Numbering::BioSemi) => detail.push("BioSemi cap numbering".to_string()),
        Some(Numbering::Egi) => detail.push("EGI sensor numbering".to_string()),
        _ => {}
    }
    let others: Vec<String> = sum.kinds.iter().filter(|(k, _)| *k != Kind::Eeg).map(|(k, n)| format!("{n} {}", k.name())).collect();
    if !others.is_empty() {
        detail.push(others.join(", "));
    }
    if !detail.is_empty() {
        ui.label(RichText::new(detail.join(" \u{b7} ")).color(pal.muted).small());
    }
    if !sum.unplaced.is_empty() {
        let list: Vec<String> = sum.unplaced.iter().take(12).cloned().collect();
        let more = if sum.unplaced.len() > 12 { format!(" and {} more", sum.unplaced.len() - 12) } else { String::new() };
        ui.label(RichText::new(format!("No position for {}{more}", list.join(", "))).color(pal.faint).small());
    }
    if !doc.montage.missing.is_empty() {
        ui.label(RichText::new(format!("Cannot form {}", doc.montage.missing.join(", "))).color(pal.faint).small());
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        widgets::heading(ui, pal, "Channels");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if doc.visible.iter().any(|v| !v) && ui.small_button("Show all").clicked() {
                doc.show_all();
            }
        });
    });
    let mut toggle: Option<(usize, bool)> = None;
    let mut hovered = None;
    let mut select = None;
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (i, c) in doc.montage.channels.iter().enumerate() {
            let visible = doc.visible[i];
            let selected = doc.selected_channel == Some(i);
            let row = ui.horizontal(|ui| {
                let mut v = visible;
                if ui.checkbox(&mut v, "").changed() {
                    toggle = Some((i, v));
                }
                widgets::dot(ui, if visible { trace_color(c, s, pal) } else { pal.faint });
                let text = RichText::new(&c.label).color(if !visible { pal.faint } else if selected { pal.accent } else { pal.text });
                ui.label(text);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let kind = if c.kind == Kind::Eeg { String::new() } else { format!("{} \u{b7} ", c.kind.tag()) };
                    ui.label(RichText::new(format!("{kind}{} Hz", fmt_hz(c.fs))).color(pal.faint).small());
                });
            });
            let resp = ui.interact(row.response.rect, ui.id().with(("row", i)), Sense::click());
            if resp.hovered() || selected {
                ui.painter().rect_filled(
                    row.response.rect.expand2(vec2(2.0, 0.0)),
                    CornerRadius::same(4),
                    if selected { pal.accent_soft } else { pal.hover.gamma_multiply(0.6) },
                );
            }
            if resp.hovered() {
                hovered = Some(i);
            }
            if resp.clicked() {
                select = Some(i);
            }
            resp.on_hover_text(&c.detail);
        }
    });
    if let Some((i, v)) = toggle {
        doc.set_visible(i, v);
    }
    if hovered.is_some() {
        doc.hover_channel = hovered;
    }
    if let Some(i) = select {
        doc.selected_channel = if doc.selected_channel == Some(i) { None } else { Some(i) };
    }
}

fn events_tab(ui: &mut egui::Ui, doc: &mut Doc, s: &Settings, pal: &Palette) {
    if !doc.annotations_ready {
        ui.label(RichText::new("Reading annotations\u{2026}").color(pal.muted));
        return;
    }
    if doc.annotations.is_empty() {
        ui.label(RichText::new("No annotations or trigger events in this file.").color(pal.muted));
        return;
    }
    let search = ui.add(
        egui::TextEdit::singleline(&mut doc.event_filter)
            .hint_text("Filter events")
            .desired_width(f32::INFINITY)
            .margin(vec2(8.0, 5.0)),
    );
    if ui.ctx().memory_mut(|m| m.data.remove_temp::<bool>(focus_search_id()).unwrap_or(false)) {
        search.request_focus();
    }
    let needle = doc.event_filter.to_lowercase();
    let matches: Vec<usize> = (0..doc.annotations.len())
        .filter(|&i| needle.is_empty() || doc.annotations[i].text.to_lowercase().contains(&needle))
        .collect();
    ui.label(RichText::new(format!("{} of {}", matches.len(), doc.annotations.len())).color(pal.faint).small());
    ui.add_space(2.0);

    let row_h = 22.0;
    let (view0, view1) = (doc.t0, doc.t0 + s.page);
    let mut jump = None;
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, matches.len(), |ui, rows| {
        for &i in &matches[rows] {
            let a = &doc.annotations[i];
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let selected = doc.selected_event == Some(i);
            let in_view = a.onset >= view0 && a.onset <= view1;
            if selected {
                ui.painter().rect_filled(rect, CornerRadius::same(4), pal.accent_soft);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(4), pal.hover);
            }
            let color = if a.source == AnnotationSource::Trigger { pal.trigger } else { pal.event };
            ui.painter().circle_filled(rect.left_center() + vec2(8.0, 0.0), 3.0, color);
            let time = doc.file.clock(a.onset).time_string(false);
            ui.painter().text(rect.left_center() + vec2(18.0, 0.0), egui::Align2::LEFT_CENTER, time, theme::mono(11.5), pal.muted);
            let text_color = if in_view || selected { pal.text } else { pal.text.gamma_multiply(0.8) };
            let mut text = a.text.clone();
            if a.duration > 0.0 {
                text += &format!("  \u{b7} {}", fmt_seconds(a.duration));
            }
            let clip = egui::Rect::from_min_max(rect.left_top() + vec2(84.0, 0.0), rect.right_bottom());
            ui.painter().with_clip_rect(clip).text(
                clip.left_center(),
                egui::Align2::LEFT_CENTER,
                text,
                theme::font(12.5),
                text_color,
            );
            if resp.clicked() {
                jump = Some(i);
            }
            resp.on_hover_text(format!("{}\n+{}", a.text, fmt_clock(a.onset - doc.range.0)));
        }
    });
    if let Some(i) = jump {
        doc.selected_event = Some(i);
        doc.jump_to(doc.annotations[i].onset, s.page);
    }
}

fn file_tab(ui: &mut egui::Ui, doc: &mut Doc, pal: &Palette) {
    let f = &doc.file;
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if f.in_memory() {
            widgets::heading(ui, pal, "Sample recording");
            ui.label(super::sample::DESCRIPTION);
            ui.label(RichText::new(super::sample::CREDIT).color(pal.muted).small());
            ui.label(RichText::new(super::sample::LICENSE).color(pal.muted).small());
            ui.hyperlink_to(RichText::new(super::sample::DOI).small(), super::sample::DOI);
            ui.add_space(6.0);
        }
        widgets::heading(ui, pal, "Recording");
        let start = f.clock(doc.range.0);
        let date = start.date.map(|d| format!("{d} ")).unwrap_or_default();
        widgets::field(ui, pal, "Start", &format!("{date}{}", start.time_string(doc.range.0.fract() != 0.0)));
        widgets::field(ui, pal, "Duration", &fmt_clock(doc.range.1 - doc.range.0));
        let format = format!("{}{}", f.format.name(), if f.discontinuous { ", discontinuous" } else { "" });
        widgets::field(ui, pal, "Format", &format);
        widgets::field(ui, pal, "Records", &format!("{} \u{d7} {} s", f.n_records, fmt_hz(f.record_duration)));
        if !doc.gaps.is_empty() {
            widgets::field(ui, pal, "Gaps", &doc.gaps.len().to_string());
        }
        widgets::field(ui, pal, "Size", &fmt_bytes(f.file_size));
        let path = f.path.display().to_string();
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(88.0, 18.0), Sense::hover());
            ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, "File", theme::font(12.5), pal.muted);
            ui.add(egui::Label::new(f.file_name()).truncate()).on_hover_text(&path);
        });
        if let Some(r) = &f.recording {
            for (k, v) in [("Equipment", &r.equipment), ("Technician", &r.technician), ("Admin code", &r.admin_code)] {
                if let Some(v) = v {
                    widgets::field(ui, pal, k, v);
                }
            }
        } else if !f.recording_raw.is_empty() {
            widgets::field(ui, pal, "Recording", &f.recording_raw);
        }

        ui.add_space(6.0);
        widgets::heading(ui, pal, "Patient");
        match &f.patient {
            Some(p) if p.code.is_some() || p.name.is_some() || p.birthdate.is_some() || p.sex.is_some() => {
                for (k, v) in [("Name", &p.name), ("Code", &p.code), ("Sex", &p.sex), ("Born", &p.birthdate), ("Other", &p.additional)] {
                    if let Some(v) = v {
                        widgets::field(ui, pal, k, v);
                    }
                }
            }
            Some(_) => {
                ui.label(RichText::new("Anonymised").color(pal.muted));
            }
            None if f.patient_raw.is_empty() || f.patient_raw.eq_ignore_ascii_case("X") => {
                ui.label(RichText::new("Not given").color(pal.muted));
            }
            None => widgets::field(ui, pal, "Patient", &f.patient_raw),
        }

        if !f.warnings.is_empty() {
            ui.add_space(6.0);
            widgets::heading(ui, pal, "Header check");
            for w in &f.warnings {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("\u{25cf}").color(pal.warn).small());
                    ui.label(RichText::new(w).small());
                });
            }
        }

        ui.add_space(6.0);
        widgets::heading(ui, pal, "Signals");
        egui::Grid::new("signals").num_columns(4).spacing(vec2(10.0, 3.0)).striped(false).show(ui, |ui| {
            for h in ["Label", "Type", "Hz", "Unit"] {
                ui.label(RichText::new(h).color(pal.faint).small());
            }
            ui.end_row();
            for (l, sig) in doc.leads.leads.iter().zip(&f.signals) {
                if l.kind == Kind::Annotation {
                    continue;
                }
                let label = ui.label(RichText::new(&sig.label).font(theme::mono(11.5)));
                let mut tip = format!(
                    "Identified as {} ({})\nPhysical {} \u{2026} {} {}\nDigital {} \u{2026} {}",
                    l.display,
                    l.kind.name(),
                    sig.phys_min,
                    sig.phys_max,
                    sig.unit,
                    sig.dig_min,
                    sig.dig_max
                );
                if !sig.prefilter.is_empty() {
                    tip += &format!("\nPrefilter: {}", sig.prefilter);
                }
                if !sig.transducer.is_empty() {
                    tip += &format!("\nTransducer: {}", sig.transducer);
                }
                if let Some(n) = &l.note {
                    tip += &format!("\n{n}");
                }
                label.on_hover_text(tip);
                ui.label(RichText::new(l.kind.tag()).color(pal.muted).small());
                ui.label(RichText::new(fmt_hz(sig.fs)).color(pal.muted).small());
                ui.label(RichText::new(&l.unit.display).color(pal.muted).small());
                ui.end_row();
            }
        });
        ui.add_space(12.0);
    });
}

fn fmt_bytes(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.2} GB", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1} MB", n / 1e6)
    } else {
        format!("{:.0} kB", n / 1e3)
    }
}
