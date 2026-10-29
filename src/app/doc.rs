//! An open recording: the file, what was identified in it, the current view, and the
//! background work that feeds the view.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;
use oink_eeg::dsp::FilterSettings;
use oink_eeg::edf::{Annotation, EdfFile};
use oink_eeg::leads::{ElectrodeId, Kind, LeadSet};
use oink_eeg::maps;
use oink_eeg::montage::{self, Montage, MontageId, SignalStats};
use oink_eeg::overview::{self, Overview};
use oink_eeg::pipeline::{self, TraceData, TraceRequest};

use super::sample::Sample;
use super::scalp::{MapKey, MapResult, MapWorker};
use super::theme::LABEL_COLORS;

/// Smallest lane height in points; with more channels than fit, the trace view scrolls.
pub const MIN_LANE: f32 = 12.0;

/// One distinct annotation text: how often it occurs, its colour, and whether it is shown.
pub struct Label {
    pub text: String,
    /// What the label means, for the built-in samples' codes ("T1": "imagined left fist").
    pub meaning: Option<&'static str>,
    pub count: usize,
    /// Index into the palette's label colours. The most frequent labels get one each; the rest
    /// share the default event colour.
    pub color: Option<usize>,
    pub shown: bool,
}

pub struct MontageEntry {
    pub id: MontageId,
    pub name: String,
    pub description: String,
    pub montage: Result<Arc<Montage>, String>,
}

/// What a computed (or requested) trace window was computed for.
#[derive(Clone, PartialEq)]
struct Key {
    montage: usize,
    visible: usize,
    filters: FilterSettings,
    px_per_sec: f64,
}

struct Requested {
    generation: u64,
    key: Key,
    t0: f64,
    t1: f64,
}

pub struct Lanes {
    /// (channel index, lane centre in points from the top of the scrolled content)
    pub rows: Vec<(usize, f32)>,
    pub lane_h: f32,
    pub content_h: f32,
}

pub struct Doc {
    pub file: Arc<EdfFile>,
    pub leads: Arc<LeadSet>,
    pub montages: Vec<MontageEntry>,
    pub montage: Arc<Montage>,
    /// Index of the current montage in `montages`.
    pub current: Option<usize>,
    stats: Vec<SignalStats>,
    pub visible: Arc<Vec<bool>>,
    /// Per-channel gain multipliers set by the user.
    pub gains: Vec<f32>,
    /// Start of the view, seconds after the header start time.
    pub t0: f64,
    pub range: (f64, f64),
    pub gaps: Vec<(f64, f64)>,
    pub annotations: Vec<Annotation>,
    pub annotations_ready: bool,
    /// Distinct annotation texts, and each annotation's index into them.
    pub labels: Vec<Label>,
    label_of: Vec<usize>,
    /// The Events tab lists every label rather than the most frequent.
    pub labels_expanded: bool,
    /// The built-in sample this is, if it is one.
    sample: Option<&'static Sample>,
    pub overview: Option<Overview>,
    /// Bumped whenever `overview` changes, so the navigator rebuilds its texture.
    pub overview_version: u64,
    traces: Option<(Key, Arc<TraceData>)>,
    pub read_error: Option<String>,
    /// Channel under the pointer in the label gutter, channel list or head map (this frame).
    pub hover_channel: Option<usize>,
    pub hover_electrode: Option<ElectrodeId>,
    pub selected_channel: Option<usize>,
    /// Time range being measured (drag in the trace view).
    pub measure: Option<(f64, f64)>,
    pub v_scroll: f32,
    pub selected_event: Option<usize>,
    pub event_filter: String,
    /// The latest scalp map, or why it could not be made.
    pub scalp: Option<MapResult>,
    /// Started on the first map request.
    maps: Option<MapWorker>,
    worker: TraceWorker,
    requested: VecDeque<Requested>,
    annotations_rx: Receiver<Vec<Annotation>>,
    overview_slot: Arc<Mutex<Option<Overview>>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Doc {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Doc {
    pub fn open(path: &Path, preferred: MontageId, ctx: &egui::Context) -> Result<Doc, String> {
        let file = EdfFile::open(path).map_err(|e| e.to_string())?;
        Self::new(file, preferred, ctx)
    }

    /// A recording built into the app.
    pub fn open_sample(sample: &'static Sample, preferred: MontageId, ctx: &egui::Context) -> Result<Doc, String> {
        let file = EdfFile::from_static(sample.name, sample.bytes).map_err(|e| e.to_string())?;
        let mut doc = Self::new(file, preferred, ctx)?;
        doc.sample = Some(sample);
        Ok(doc)
    }

    fn new(file: EdfFile, preferred: MontageId, ctx: &egui::Context) -> Result<Doc, String> {
        let file = Arc::new(file);
        let leads = Arc::new(LeadSet::identify(&file.signals));
        let stats = pipeline::signal_stats(&file, &leads);
        let montages: Vec<MontageEntry> = montage::catalog(&leads, &file.signals, &stats)
            .into_iter()
            .map(|(id, m)| MontageEntry {
                id,
                name: id.name().to_string(),
                description: id.description().to_string(),
                montage: m.map(Arc::new),
            })
            .collect();
        let range = file.time_range();
        let cancel = Arc::new(AtomicBool::new(false));

        // Annotations and trigger channels are read in the background: both need a pass over
        // every data record.
        let (tx, annotations_rx) = mpsc::channel();
        {
            let file = file.clone();
            let ctx = ctx.clone();
            let triggers: Vec<usize> = leads
                .leads
                .iter()
                .filter(|l| l.kind == Kind::Trigger && is_trigger_channel(&l.label))
                .map(|l| l.signal)
                .collect();
            std::thread::Builder::new()
                .name("annotations".into())
                .spawn(move || {
                    let mut all = file.read_annotations().unwrap_or_default();
                    for s in triggers {
                        all.extend(file.read_trigger_events(s).unwrap_or_default());
                    }
                    all.sort_by(|a, b| a.onset.total_cmp(&b.onset));
                    let _ = tx.send(all);
                    ctx.request_repaint();
                })
                .map_err(|e| e.to_string())?;
        }

        let overview_slot = Arc::new(Mutex::new(None));
        {
            let file = file.clone();
            let signals = overview::pick_signals(&file, &leads);
            let slot = overview_slot.clone();
            let cancel = cancel.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("overview".into())
                .spawn(move || {
                    overview::compute(&file, &signals, &cancel, |ov| {
                        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(ov.clone());
                        ctx.request_repaint();
                    });
                })
                .map_err(|e| e.to_string())?;
        }

        let mut doc = Doc {
            gaps: file.gaps(),
            file,
            leads,
            montages,
            montage: Arc::new(empty_montage()),
            current: None,
            stats,
            visible: Arc::new(Vec::new()),
            gains: Vec::new(),
            t0: range.0,
            range,
            annotations: Vec::new(),
            annotations_ready: false,
            labels: Vec::new(),
            label_of: Vec::new(),
            labels_expanded: false,
            sample: None,
            overview: None,
            overview_version: 0,
            traces: None,
            read_error: None,
            hover_channel: None,
            hover_electrode: None,
            selected_channel: None,
            measure: None,
            v_scroll: 0.0,
            selected_event: None,
            event_filter: String::new(),
            scalp: None,
            maps: None,
            worker: TraceWorker::new(ctx.clone()),
            requested: VecDeque::new(),
            annotations_rx,
            overview_slot,
            cancel,
        };
        let available = |id: MontageId| doc.montages.iter().any(|m| m.id == id && m.montage.is_ok());
        let pick = if available(preferred) {
            Some(preferred)
        } else {
            MontageId::ALL.into_iter().find(|&id| available(id))
        };
        if let Some(id) = pick {
            doc.set_montage(id);
        }
        // EDFbrowser montage files kept next to the recording.
        if let Some(dir) = doc.file.path.parent().filter(|_| !doc.file.in_memory()).map(Path::to_path_buf) {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            files.retain(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mtg")));
            files.sort();
            for f in files.into_iter().take(12) {
                doc.load_montage_file(&f);
            }
        }
        Ok(doc)
    }

    /// Adds a montage from an EDFbrowser `.mtg` file. Returns its index; the entry records why
    /// it is unusable if it cannot be built for this file.
    pub fn load_montage_file(&mut self, path: &Path) -> usize {
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let montage = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|xml| montage::from_edfbrowser(&xml, &self.leads, &self.file.signals, &self.stats))
            .map(Arc::new);
        let description = match &montage {
            Ok(m) if !m.missing.is_empty() => format!("EDFbrowser montage \u{b7} {} not in this file", m.missing.len()),
            _ => "EDFbrowser montage".to_string(),
        };
        let entry = MontageEntry { id: MontageId::Custom, name, description, montage };
        match self.montages.iter().position(|m| m.id == MontageId::Custom && m.name == entry.name) {
            Some(i) => {
                self.montages[i] = entry;
                i
            }
            None => {
                self.montages.push(entry);
                self.montages.len() - 1
            }
        }
    }

    pub fn montage_name(&self) -> &str {
        self.current.map(|i| self.montages[i].name.as_str()).unwrap_or("None")
    }

    pub fn set_montage(&mut self, id: MontageId) -> bool {
        match self.montages.iter().position(|m| m.id == id) {
            Some(i) => self.set_montage_at(i),
            None => false,
        }
    }

    pub fn set_montage_at(&mut self, index: usize) -> bool {
        let Some(Ok(m)) = self.montages.get(index).map(|m| &m.montage) else { return false };
        self.montage = m.clone();
        self.current = Some(index);
        self.visible = Arc::new(self.montage.channels.iter().map(|c| !c.hidden).collect());
        self.gains = self.montage.channels.iter().map(|c| c.gain).collect();
        self.traces = None;
        self.selected_channel = None;
        self.hover_channel = None;
        self.measure = None;
        self.v_scroll = 0.0;
        true
    }

    pub fn set_visible(&mut self, ch: usize, on: bool) {
        if self.visible.get(ch) != Some(&on) {
            let mut v = (*self.visible).clone();
            v[ch] = on;
            self.visible = Arc::new(v);
        }
    }

    pub fn show_all(&mut self) {
        self.visible = Arc::new(vec![true; self.montage.channels.len()]);
    }

    /// Picks up results from background threads.
    pub fn poll(&mut self) {
        if let Ok(a) = self.annotations_rx.try_recv() {
            (self.labels, self.label_of) = group_labels(&a);
            for l in &mut self.labels {
                l.meaning = self.sample.and_then(|s| s.labels.iter().find(|(code, _)| *code == l.text)).map(|(_, m)| *m);
            }
            self.annotations = a;
            self.annotations_ready = true;
        }
        if let Some(ov) = self.overview_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.overview = Some(ov);
            self.overview_version += 1;
        }
        if let Some(result) = self.maps.as_ref().and_then(MapWorker::take) {
            self.scalp = Some(result);
        }
        if let Some(err) = self.worker.error.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.read_error = Some(err);
        }
        if let Some((generation, data)) = self.worker.take() {
            if let Some(i) = self.requested.iter().position(|r| r.generation == generation) {
                self.traces = Some((self.requested[i].key.clone(), data));
                self.requested.drain(..=i);
                self.read_error = None;
            }
        }
    }

    fn key(&self, filters: FilterSettings, px_per_sec: f64) -> Key {
        Key {
            montage: Arc::as_ptr(&self.montage) as usize,
            visible: Arc::as_ptr(&self.visible) as usize,
            filters,
            px_per_sec,
        }
    }

    /// Makes sure trace data for the current view is computed or on its way. Data is computed
    /// with a margin on both sides, so scrolling within it needs no new work.
    pub fn ensure_traces(&mut self, px_per_sec: f64, filters: FilterSettings, page: f64) {
        if self.montage.channels.is_empty() || px_per_sec <= 0.0 {
            return;
        }
        let key = self.key(filters, px_per_sec);
        let (lo, hi) = self.range;
        let need = ((self.t0 - page * 0.2).max(lo), (self.t0 + page * 1.2).min(hi));
        let covers = |k: &Key, a: f64, b: f64| *k == key && a <= need.0 + 1e-9 && b >= need.1 - 1e-9;
        if let Some((k, d)) = &self.traces {
            if covers(k, d.t0, d.t1) {
                return;
            }
        }
        if self.requested.back().is_some_and(|r| covers(&r.key, r.t0, r.t1)) {
            return;
        }
        let margin = page * 0.5;
        let (t0, t1) = (self.t0 - margin, self.t0 + page + margin);
        let req = TraceRequest {
            file: self.file.clone(),
            montage: self.montage.clone(),
            visible: self.visible.clone(),
            t0,
            t1,
            px_per_sec,
            filters,
        };
        // Panning keeps the running job (its data is still useful); anything that changes what
        // is drawn cancels it.
        let last_key = self.requested.back().map(|r| &r.key).or(self.traces.as_ref().map(|(k, _)| k));
        let changed = last_key != Some(&key);
        let generation = self.worker.request(req, changed);
        self.requested.push_back(Requested { generation, key, t0, t1 });
        while self.requested.len() > 16 {
            self.requested.pop_front();
        }
    }

    /// The latest computed traces if they belong to the current montage and visibility.
    pub fn traces(&self) -> Option<&Arc<TraceData>> {
        let (k, d) = self.traces.as_ref()?;
        (k.montage == Arc::as_ptr(&self.montage) as usize).then_some(d)
    }

    /// True while the view is not yet covered by computed data.
    pub fn loading(&self, page: f64) -> bool {
        match &self.traces {
            Some((_, d)) => d.t0 > self.t0 + 1e-6 || d.t1 < (self.t0 + page).min(self.range.1) - 1e-6,
            None => !self.montage.channels.is_empty(),
        }
    }

    pub fn lanes(&self, height: f32) -> Lanes {
        let vis: Vec<usize> = (0..self.montage.channels.len()).filter(|&i| self.visible[i]).collect();
        let groups = vis.windows(2).filter(|w| self.montage.channels[w[0]].group != self.montage.channels[w[1]].group).count();
        let units = vis.len() as f32 + 0.6 * groups as f32;
        let lane_h = (height / units.max(1.0)).max(MIN_LANE);
        let mut rows = Vec::with_capacity(vis.len());
        let mut y = 0.0;
        for (k, &i) in vis.iter().enumerate() {
            if k > 0 && self.montage.channels[vis[k - 1]].group != self.montage.channels[i].group {
                y += 0.6 * lane_h;
            }
            rows.push((i, y + lane_h / 2.0));
            y += lane_h;
        }
        Lanes { rows, lane_h, content_h: y }
    }

    pub fn clamp_view(&mut self, page: f64) {
        let (lo, hi) = self.range;
        self.t0 = self.t0.clamp(lo, (hi - page).max(lo));
    }

    /// Moves the view so `t` sits a quarter of the way into the page.
    pub fn jump_to(&mut self, t: f64, page: f64) {
        self.t0 = t - page * 0.25;
        self.clamp_view(page);
    }

    /// Index of the next/previous annotation relative to the view.
    pub fn step_event(&mut self, forward: bool, page: f64) {
        if self.annotations.is_empty() {
            return;
        }
        let anchor = self.t0 + page * 0.25;
        let idx = if forward {
            self.annotations.iter().position(|a| a.onset > anchor + 1e-3)
        } else {
            self.annotations.iter().rposition(|a| a.onset < anchor - 1e-3)
        };
        if let Some(i) = idx {
            self.selected_event = Some(i);
            self.jump_to(self.annotations[i].onset, page);
        }
    }

    /// The average-reference montage that scalp maps are computed from.
    fn map_montage(&self) -> Result<&Arc<Montage>, &str> {
        match self.montages.iter().find(|m| m.id == MontageId::Average).map(|m| &m.montage) {
            Some(Ok(m)) => Ok(m),
            Some(Err(reason)) => Err(reason),
            None => Err("No scalp EEG."),
        }
    }

    /// The highest frequency the mapped electrodes carry, or why this file cannot be mapped.
    pub fn map_nyquist(&self) -> Result<f64, &str> {
        let m = self.map_montage()?;
        maps::electrodes(m).next().map(|c| c.fs / 2.0).ok_or("No scalp EEG.")
    }

    /// Asks for a scalp map; it arrives through `poll`.
    pub fn request_map(&mut self, key: MapKey, ctx: &egui::Context) {
        if self.maps.is_none() {
            let Ok(avg) = self.map_montage() else { return };
            self.maps = Some(MapWorker::new(self.file.clone(), avg.clone(), ctx.clone(), self.cancel.clone()));
        }
        if let Some(w) = &mut self.maps {
            w.request(key);
        }
    }

    /// True while a requested map has not arrived.
    pub fn map_pending(&self) -> bool {
        self.maps.as_ref().and_then(MapWorker::requested).is_some_and(|k| self.scalp.as_ref().is_none_or(|(have, _)| *have != k))
    }

    /// The label of annotation `i`.
    pub fn label(&self, i: usize) -> Option<&Label> {
        self.label_of.get(i).map(|&l| &self.labels[l])
    }

    /// Whether annotation `i` is shown: its label is ticked in the Events tab.
    pub fn event_shown(&self, i: usize) -> bool {
        self.label(i).is_none_or(|l| l.shown)
    }

    /// Annotation `i`'s text, with what it means and the channel it is about, where known.
    pub fn event_text(&self, i: usize) -> String {
        let a = &self.annotations[i];
        let mut text = a.text.clone();
        if let Some(m) = self.label(i).and_then(|l| l.meaning) {
            text = format!("{text} \u{b7} {m}");
        }
        if let Some(lead) = a.signal.and_then(|s| self.leads.leads.iter().find(|l| l.signal == s)) {
            text = format!("{text} \u{b7} {}", lead.display);
        }
        text
    }

    /// Selects annotation `i`. One with a duration also becomes the measured span, which the
    /// scalp map follows.
    pub fn select_event(&mut self, i: usize) {
        self.selected_event = Some(i);
        let a = &self.annotations[i];
        if a.duration > 0.0 {
            self.measure = Some((a.onset, a.onset + a.duration));
        }
    }

    /// Channels whose derivation involves `e`.
    pub fn channels_with(&self, e: ElectrodeId) -> impl Iterator<Item = usize> + '_ {
        self.montage.channels.iter().enumerate().filter(move |(_, c)| c.a == Some(e) || c.b == Some(e)).map(|(i, _)| i)
    }
}

/// Groups annotations by text. The most frequent labels get a colour each (ties keep the order
/// they first appear in); the rest share the default event colour.
fn group_labels(annotations: &[Annotation]) -> (Vec<Label>, Vec<usize>) {
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut labels: Vec<Label> = Vec::new();
    let label_of = annotations
        .iter()
        .map(|a| {
            let l = *index.entry(a.text.as_str()).or_insert_with(|| {
                labels.push(Label { text: a.text.clone(), meaning: None, count: 0, color: None, shown: true });
                labels.len() - 1
            });
            labels[l].count += 1;
            l
        })
        .collect();
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by_key(|&l| std::cmp::Reverse(labels[l].count));
    for (rank, &l) in order.iter().take(LABEL_COLORS).enumerate() {
        labels[l].color = Some(rank);
    }
    (labels, label_of)
}

fn is_trigger_channel(label: &str) -> bool {
    matches!(label.trim().to_ascii_lowercase().as_str(), "status" | "trigger" | "trig" | "sti 014" | "stim" | "stimulus" | "markers")
}

fn empty_montage() -> Montage {
    Montage { id: MontageId::AsRecorded, channels: Vec::new(), mean_terms: Vec::new(), mean_count: 0, reference: None, missing: Vec::new() }
}

/// Computes trace windows on a background thread. Queued requests are skipped in favour of the
/// newest. A running one is abandoned only when the newest changes what is drawn (montage,
/// filters, zoom), not when it merely pans, so fast scrolling still gets fresh data.
struct TraceWorker {
    tx: Sender<(u64, u64, TraceRequest)>,
    generation: AtomicU64,
    epoch: Arc<AtomicU64>,
    done: Arc<Mutex<Option<(u64, Arc<TraceData>)>>>,
    error: Arc<Mutex<Option<String>>>,
}

impl TraceWorker {
    fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel::<(u64, u64, TraceRequest)>();
        let epoch = Arc::new(AtomicU64::new(0));
        let done = Arc::new(Mutex::new(None));
        let error = Arc::new(Mutex::new(None));
        let (ep, d, e) = (epoch.clone(), done.clone(), error.clone());
        std::thread::Builder::new()
            .name("traces".into())
            .spawn(move || {
                while let Ok(mut job) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        job = newer;
                    }
                    let (generation, job_epoch, req) = job;
                    let current = ep.clone();
                    let cancelled = move || current.load(Ordering::Relaxed) != job_epoch;
                    match pipeline::compute(&req, &cancelled) {
                        Ok(Some(data)) => {
                            *d.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, Arc::new(data)));
                            ctx.request_repaint();
                        }
                        Ok(None) => {}
                        Err(err) => {
                            *e.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("Could not read the file: {err}"));
                            ctx.request_repaint();
                        }
                    }
                }
            })
            .expect("spawn trace worker");
        TraceWorker { tx, generation: AtomicU64::new(0), epoch, done, error }
    }

    fn request(&self, req: TraceRequest, changes_drawing: bool) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let epoch = if changes_drawing {
            self.epoch.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.epoch.load(Ordering::SeqCst)
        };
        let _ = self.tx.send((generation, epoch, req));
        generation
    }

    fn take(&self) -> Option<(u64, Arc<TraceData>)> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oink_eeg::edf::AnnotationSource;

    #[test]
    fn labels_are_grouped_and_coloured_by_frequency() {
        let event = |text: &str| Annotation { onset: 0.0, duration: 0.0, text: text.into(), source: AnnotationSource::Annotation, signal: None };
        let mut events: Vec<Annotation> = ["T0", "T2", "T0", "T1", "T0", "T1"].into_iter().map(event).collect();
        events.extend((0..10).map(|k| event(&format!("note {k}"))));
        let (labels, of) = group_labels(&events);
        assert_eq!(labels.len(), 13);
        assert_eq!((labels[of[0]].text.as_str(), labels[of[0]].count, labels[of[0]].color), ("T0", 3, Some(0)));
        assert_eq!(labels[of[3]].color, Some(1), "T1, twice");
        assert_eq!(labels[of[1]].color, Some(2), "T2, once but before the notes");
        assert_eq!(labels.iter().filter(|l| l.color.is_some()).count(), LABEL_COLORS);
        assert!(labels.iter().all(|l| l.shown));
    }
}
