//! Montages: the derivations drawn on screen, built from the identified leads.
//!
//! Every on-screen channel is a linear combination of file signals. Recorded leads form a
//! graph whose nodes are electrodes and references: the lead "Fp1-REF" is an edge from Fp1 to
//! REF carrying V(Fp1) - V(REF). The potential difference between any two electrodes in the
//! same connected component is the sum of the edges along a path between them. That single
//! rule derives bipolar montages from referential recordings, the transverse montage from a
//! longitudinal bipolar recording, and the average reference from either.
//!
//! Derivations name electrodes, so the order of signals in the file never matters.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::edf::Signal;
use crate::leads::{ElectrodeId, Kind, Lead, LeadSet, Reference, Side, Site, electrode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MontageId {
    AsRecorded,
    Banana,
    Transverse,
    Average,
    Tcp,
    /// Loaded from a montage file.
    Custom,
}

impl MontageId {
    pub const ALL: [MontageId; 5] =
        [MontageId::AsRecorded, MontageId::Banana, MontageId::Transverse, MontageId::Average, MontageId::Tcp];

    pub fn name(self) -> &'static str {
        match self {
            MontageId::AsRecorded => "As recorded",
            MontageId::Banana => "Double banana",
            MontageId::Transverse => "Transverse",
            MontageId::Average => "Average",
            MontageId::Tcp => "TCP",
            MontageId::Custom => "Custom",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            MontageId::AsRecorded => "Signals as stored in the file",
            MontageId::Banana => "Longitudinal bipolar \u{b7} ACNS LB-18.1",
            MontageId::Transverse => "Transverse bipolar \u{b7} ACNS TB-18.1",
            MontageId::Average => "Common average reference",
            MontageId::Tcp => "Temporal central parasagittal \u{b7} TUH",
            MontageId::Custom => "From a montage file",
        }
    }
}

type Chain = &'static [(&'static str, &'static str)];

/// Longitudinal bipolar ("double banana"), ACNS Guideline 3 LB-18.1: left temporal, left
/// parasagittal, midline, right parasagittal, right temporal.
const BANANA: [Chain; 5] = [
    &[("Fp1", "F7"), ("F7", "T7"), ("T7", "P7"), ("P7", "O1")],
    &[("Fp1", "F3"), ("F3", "C3"), ("C3", "P3"), ("P3", "O1")],
    &[("Fz", "Cz"), ("Cz", "Pz")],
    &[("Fp2", "F4"), ("F4", "C4"), ("C4", "P4"), ("P4", "O2")],
    &[("Fp2", "F8"), ("F8", "T8"), ("T8", "P8"), ("P8", "O2")],
];

/// Transverse bipolar, ACNS Guideline 3 TB-18.1: front to back, each row left to right.
const TRANSVERSE: [Chain; 4] = [
    &[("F7", "Fp1"), ("Fp1", "Fp2"), ("Fp2", "F8"), ("F7", "F3"), ("F3", "Fz"), ("Fz", "F4"), ("F4", "F8")],
    &[("T7", "C3"), ("C3", "Cz"), ("Cz", "C4"), ("C4", "T8")],
    &[("P7", "P3"), ("P3", "Pz"), ("Pz", "P4"), ("P4", "P8")],
    &[("P7", "O1"), ("O1", "O2"), ("O2", "P8")],
];

/// TUH EEG "temporal central parasagittal" montage, in the corpus' channel order.
const TCP: [Chain; 5] = [
    &[("Fp1", "F7"), ("F7", "T7"), ("T7", "P7"), ("P7", "O1")],
    &[("Fp2", "F8"), ("F8", "T8"), ("T8", "P8"), ("P8", "O2")],
    &[("A1", "T7"), ("T7", "C3"), ("C3", "Cz"), ("Cz", "C4"), ("C4", "T8"), ("T8", "A2")],
    &[("Fp1", "F3"), ("F3", "C3"), ("C3", "P3"), ("P3", "O1")],
    &[("Fp2", "F4"), ("F4", "C4"), ("C4", "P4"), ("P4", "O2")],
];

/// Display scaling for channels fitted to their lane rather than drawn at the global
/// sensitivity (ECG in mV, SpO2 in %, ...).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignalStats {
    /// Subtracted before drawing (for signals shown around a fixed level, like SpO2).
    pub center: f32,
    /// Half of the typical peak-to-peak range.
    pub half_range: f32,
}

impl Default for SignalStats {
    fn default() -> Self {
        SignalStats { center: 0.0, half_range: 1.0 }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub label: String,
    /// Which file signals make up this channel, for tooltips.
    pub detail: String,
    /// (signal index, coefficient). Coefficients include unit conversion to the display unit.
    pub terms: Vec<(u32, f32)>,
    /// Subtract the montage's mean signal (average reference).
    pub minus_mean: bool,
    pub kind: Kind,
    pub fs: f64,
    pub unit: String,
    pub sensitivity: bool,
    pub ac: bool,
    pub filtered: bool,
    pub center: f32,
    pub half_range: f32,
    pub side: Side,
    /// Channels in the same group are drawn together; groups are separated by a small gap.
    pub group: u16,
    /// Electrodes for the head map: `a - b`, or `a` against the montage reference.
    pub a: Option<ElectrodeId>,
    pub b: Option<ElectrodeId>,
    /// Hidden until the user shows it (e.g. a BDF status channel already decoded into events).
    pub hidden: bool,
    /// Starting gain relative to the global sensitivity (montage files can set their own).
    pub gain: f32,
}

#[derive(Clone, Debug)]
pub struct Montage {
    pub id: MontageId,
    pub channels: Vec<Channel>,
    /// For the average reference: the mean of the scalp electrodes, as signal terms.
    pub mean_terms: Vec<(u32, f32)>,
    pub mean_count: usize,
    /// What single-electrode channels are referred to ("REF", "average", ...).
    pub reference: Option<String>,
    /// Derivations of the template that this file cannot form.
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Node {
    E(ElectrodeId),
    R(String),
}

/// Potentials of every electrode/reference node relative to its component's root, as signal
/// terms. Nodes are keyed by sampling rate too: signals at different rates cannot be combined.
struct Graph {
    pot: HashMap<(u64, Node), (usize, Vec<(u32, f32)>)>,
    by_electrode: BTreeMap<ElectrodeId, Vec<u64>>,
}

impl Graph {
    fn new(leads: &[Lead], signals: &[Signal]) -> Graph {
        type Key = (u64, Node);
        let mut adj: BTreeMap<Key, Vec<(Key, u32, f32)>> = BTreeMap::new();
        for l in leads {
            if !matches!(l.kind, Kind::Eeg | Kind::Eog) {
                continue;
            }
            let (Some(a), Some(r)) = (l.electrode, &l.reference) else { continue };
            let fs = signals[l.signal].fs;
            if fs <= 0.0 {
                continue;
            }
            let b = match r {
                Reference::Electrode(e) => Node::E(*e),
                Reference::Named(n) => Node::R(n.clone()),
                Reference::Unstated => Node::R(String::new()),
            };
            let (na, nb) = ((fs.to_bits(), Node::E(a)), (fs.to_bits(), b));
            if na == nb {
                continue;
            }
            // signal = V(a) - V(b), so pot(b) = pot(a) - c*s and pot(a) = pot(b) + c*s.
            let c = l.display_scale();
            adj.entry(na.clone()).or_default().push((nb.clone(), l.signal as u32, -c));
            adj.entry(nb).or_default().push((na, l.signal as u32, c));
        }

        // Root each component at its best-connected node (the shared reference, if any), so
        // referential channels come out as a single term.
        let mut order: Vec<&Key> = adj.keys().collect();
        order.sort_by_key(|k| std::cmp::Reverse(adj[*k].len()));
        let mut pot: HashMap<Key, (usize, Vec<(u32, f32)>)> = HashMap::new();
        for (comp, root) in order.into_iter().enumerate() {
            if pot.contains_key(root) {
                continue;
            }
            pot.insert(root.clone(), (comp, Vec::new()));
            let mut queue = VecDeque::from([root.clone()]);
            while let Some(n) = queue.pop_front() {
                let pn = pot[&n].1.clone();
                for (m, s, c) in &adj[&n] {
                    if pot.contains_key(m) {
                        continue;
                    }
                    let mut pm = pn.clone();
                    add_term(&mut pm, *s, *c);
                    pot.insert(m.clone(), (comp, pm));
                    queue.push_back(m.clone());
                }
            }
        }
        let mut by_electrode: BTreeMap<ElectrodeId, Vec<u64>> = BTreeMap::new();
        for (fs, node) in pot.keys() {
            if let Node::E(e) = node {
                by_electrode.entry(*e).or_default().push(*fs);
            }
        }
        for v in by_electrode.values_mut() {
            v.sort_unstable_by(|a, b| f64::from_bits(*b).total_cmp(&f64::from_bits(*a)));
        }
        Graph { pot, by_electrode }
    }

    /// V(a) - V(b) as signal terms, with its sampling rate, if `a` and `b` are connected.
    fn derive(&self, a: ElectrodeId, b: ElectrodeId) -> Option<(f64, Vec<(u32, f32)>)> {
        for fs in self.by_electrode.get(&a)? {
            let (ca, pa) = &self.pot[&(*fs, Node::E(a))];
            if let Some((cb, pb)) = self.pot.get(&(*fs, Node::E(b))) {
                if ca == cb {
                    let mut terms = pa.clone();
                    for &(s, c) in pb {
                        add_term(&mut terms, s, -c);
                    }
                    return Some((f64::from_bits(*fs), terms));
                }
            }
        }
        None
    }
}

fn add_term(terms: &mut Vec<(u32, f32)>, s: u32, c: f32) {
    match terms.iter_mut().find(|t| t.0 == s) {
        Some(t) => t.1 += c,
        None => terms.push((s, c)),
    }
    terms.retain(|t| t.1.abs() > 1e-6);
}

/// All montages, each either built or with the reason it is not available for this file.
pub fn catalog(leads: &LeadSet, signals: &[Signal], stats: &[SignalStats]) -> Vec<(MontageId, Result<Montage, String>)> {
    let graph = Graph::new(&leads.leads, signals);
    MontageId::ALL.iter().map(|&id| (id, build_with(&graph, id, leads, signals, stats))).collect()
}

pub fn build(id: MontageId, leads: &LeadSet, signals: &[Signal], stats: &[SignalStats]) -> Result<Montage, String> {
    build_with(&Graph::new(&leads.leads, signals), id, leads, signals, stats)
}

fn build_with(
    graph: &Graph,
    id: MontageId,
    leads: &LeadSet,
    signals: &[Signal],
    stats: &[SignalStats],
) -> Result<Montage, String> {
    let mut m = Montage {
        id,
        channels: Vec::new(),
        mean_terms: Vec::new(),
        mean_count: 0,
        reference: None,
        missing: Vec::new(),
    };
    // Spell electrodes the way the file does (T3 rather than T7 in a T3 file).
    let mut spelled: HashMap<ElectrodeId, String> = HashMap::new();
    for l in &leads.leads {
        if let (Some(e), Some(n)) = (l.electrode, &l.electrode_name) {
            spelled.entry(e).or_insert_with(|| n.clone());
        }
    }
    let legacy = leads.summary.legacy_names;
    let spell = |e: ElectrodeId| -> String {
        if let Some(n) = spelled.get(&e) {
            return n.clone();
        }
        let name = &e.info().name;
        if legacy {
            if let Some(old) = [("T7", "T3"), ("T8", "T4"), ("P7", "T5"), ("P8", "T6")].iter().find(|(n, _)| n == name) {
                return old.1.to_string();
            }
        }
        name.clone()
    };
    let detail = |terms: &[(u32, f32)]| -> String {
        terms
            .iter()
            .enumerate()
            .map(|(i, (s, c))| {
                let sign = if *c < 0.0 { "\u{2212} " } else if i > 0 { "+ " } else { "" };
                format!("{sign}{}", signals[*s as usize].label)
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    match id {
        MontageId::AsRecorded => {
            m.reference = leads.summary.reference.clone();
            for l in &leads.leads {
                if l.kind == Kind::Annotation || signals[l.signal].samples_per_record == 0 {
                    continue;
                }
                let group = if l.kind == Kind::Eeg { 0 } else { 1 };
                m.channels.push(recorded(l, &signals[l.signal], stats[l.signal], group));
            }
        }
        MontageId::Banana | MontageId::Transverse | MontageId::Tcp => {
            let chains: &[Chain] = match id {
                MontageId::Banana => &BANANA,
                MontageId::Transverse => &TRANSVERSE,
                _ => &TCP,
            };
            let total: usize = chains.iter().map(|c| c.len()).sum();
            for (g, chain) in chains.iter().enumerate() {
                for &(x, y) in chain.iter() {
                    let (ex, ey) = (electrode(x).expect("template electrode").0, electrode(y).expect("template electrode").0);
                    match graph.derive(ex, ey) {
                        Some((fs, terms)) if !terms.is_empty() => m.channels.push(Channel {
                            label: format!("{}\u{2013}{}", spell(ex), spell(ey)),
                            detail: detail(&terms),
                            terms,
                            minus_mean: false,
                            kind: Kind::Eeg,
                            fs,
                            unit: "\u{b5}V".into(),
                            sensitivity: true,
                            ac: true,
                            filtered: true,
                            center: 0.0,
                            half_range: 1.0,
                            side: pair_side(ex, ey),
                            group: g as u16,
                            a: Some(ex),
                            b: Some(ey),
                            hidden: false,
                            gain: 1.0,
                        }),
                        _ => m.missing.push(format!("{}\u{2013}{}", spell(ex), spell(ey))),
                    }
                }
            }
            let have = m.channels.len();
            if have * 2 < total {
                return Err(format!("Needs the 10-20 electrodes; only {have} of {total} derivations can be formed."));
            }
            append_others(&mut m, leads, signals, stats, chains.len() as u16);
        }
        MontageId::Custom => return Err("Custom montages are loaded from a file.".into()),
        MontageId::Average => {
            // The largest connected set of scalp electrodes that share a sampling rate.
            let mut groups: BTreeMap<(u64, usize), Vec<ElectrodeId>> = BTreeMap::new();
            for ((fs, node), (comp, _)) in &graph.pot {
                if let Node::E(e) = node {
                    if e.info().site == Site::Scalp {
                        groups.entry((*fs, *comp)).or_default().push(*e);
                    }
                }
            }
            let Some(((fs, comp), mut members)) = groups.into_iter().max_by_key(|(_, v)| v.len()) else {
                return Err("Needs referential or bipolar scalp EEG.".into());
            };
            if members.len() < 6 {
                return Err(format!("Needs at least 6 scalp electrodes; found {}.", members.len()));
            }
            members.sort_by(|a, b| spatial_key(*a).partial_cmp(&spatial_key(*b)).unwrap());
            members.dedup();
            let n = members.len() as f32;
            for e in &members {
                for &(s, c) in &graph.pot[&(fs, Node::E(*e))].1 {
                    add_term(&mut m.mean_terms, s, c / n);
                }
            }
            m.mean_count = members.len();
            m.reference = Some("average".into());
            let _ = comp;
            for e in members {
                let terms = graph.pot[&(fs, Node::E(e))].1.clone();
                m.channels.push(Channel {
                    label: format!("{}\u{2013}Avg", spell(e)),
                    detail: format!("{} \u{2212} mean of {} electrodes", if terms.is_empty() { "reference".to_string() } else { detail(&terms) }, n),
                    terms,
                    minus_mean: true,
                    kind: Kind::Eeg,
                    fs: f64::from_bits(fs),
                    unit: "\u{b5}V".into(),
                    sensitivity: true,
                    ac: true,
                    filtered: true,
                    center: 0.0,
                    half_range: 1.0,
                    side: e.side(),
                    group: 0,
                    a: Some(e),
                    b: None,
                    hidden: false,
                    gain: 1.0,
                });
            }
            append_others(&mut m, leads, signals, stats, 1);
        }
    }
    if m.channels.is_empty() {
        return Err("No signals to show.".into());
    }
    Ok(m)
}

/// Builds a montage from an EDFbrowser montage file (`.mtg`, XML). Each `signalcomposition`
/// is a weighted sum of signals, which are matched by label (never by position). Per-channel
/// sensitivities (`voltpercm`) become starting gains relative to the file's most common
/// setting; per-channel filters are not used (the viewer's filters are global).
pub fn from_edfbrowser(xml: &str, leads: &LeadSet, signals: &[Signal], stats: &[SignalStats]) -> Result<Montage, String> {
    let compositions = elements(xml, "signalcomposition");
    if compositions.is_empty() {
        return Err("Not an EDFbrowser montage (no signalcomposition elements).".into());
    }
    let find = |label: &str| {
        let label = label.trim();
        signals
            .iter()
            .position(|s| !s.is_annotation && s.label.trim() == label)
            .or_else(|| signals.iter().position(|s| !s.is_annotation && s.label.trim().eq_ignore_ascii_case(label)))
    };
    let volts: Vec<f32> = compositions
        .iter()
        .filter_map(|c| elements(c, "voltpercm").first().and_then(|v| v.trim().parse::<f32>().ok()))
        .filter(|v| *v > 0.0)
        .collect();
    let typical = {
        let mut counts: Vec<(f32, usize)> = Vec::new();
        for v in &volts {
            match counts.iter_mut().find(|(x, _)| (x - v).abs() < 1e-3) {
                Some(c) => c.1 += 1,
                None => counts.push((*v, 1)),
            }
        }
        counts.into_iter().max_by_key(|c| c.1).map(|c| c.0)
    };

    let mut m = Montage {
        id: MontageId::Custom,
        channels: Vec::new(),
        mean_terms: Vec::new(),
        mean_count: 0,
        reference: leads.summary.reference.clone(),
        missing: Vec::new(),
    };
    let mut group = 0u16;
    for comp in compositions {
        let mut parts: Vec<(usize, f32)> = Vec::new();
        let mut unknown = Vec::new();
        for sig in elements(comp, "signal") {
            let label = unescape(elements(sig, "label").first().copied().unwrap_or(""));
            let factor = elements(sig, "factor").first().and_then(|f| f.trim().parse::<f32>().ok()).unwrap_or(1.0);
            match find(&label) {
                Some(i) => parts.push((i, factor)),
                None => unknown.push(label.trim().to_string()),
            }
        }
        if parts.is_empty() || !unknown.is_empty() {
            m.missing.push(unknown.join(" / "));
            continue;
        }
        let fs = signals[parts[0].0].fs;
        if parts.iter().any(|(i, _)| signals[*i].fs != fs) {
            m.missing.push("signals at different sampling rates".into());
            continue;
        }
        let lead = |i: usize| &leads.leads[i];
        let sensitivity = parts.iter().all(|(i, _)| lead(*i).sensitivity);
        let first = lead(parts[0].0);
        let (a, b) = match parts.as_slice() {
            [(i, f)] if *f > 0.0 => (lead(*i).electrode, first_reference(lead(*i))),
            [(i, fi), (j, fj)] if *fi > 0.0 && *fj < 0.0 => (lead(*i).electrode, lead(*j).electrode),
            [(i, fi), (j, fj)] if *fi < 0.0 && *fj > 0.0 => (lead(*j).electrode, lead(*i).electrode),
            _ => (None, None),
        };
        let label = match parts.as_slice() {
            [(i, f)] if (*f - 1.0).abs() < 1e-6 => lead(*i).display.clone(),
            [(i, fi), (j, fj)] if (*fi - 1.0).abs() < 1e-6 && (*fj + 1.0).abs() < 1e-6 => {
                format!("{}\u{2013}{}", lead(*i).display, lead(*j).display)
            }
            _ => parts
                .iter()
                .enumerate()
                .map(|(k, (i, f))| {
                    let sign = if *f < 0.0 { "\u{2212}" } else if k > 0 { "+" } else { "" };
                    let w = f.abs();
                    let w = if (w - 1.0).abs() < 1e-6 { String::new() } else { format!("{w}\u{b7}") };
                    format!("{sign}{w}{}", lead(*i).display)
                })
                .collect::<Vec<_>>()
                .join(" "),
        };
        let prev = m.channels.last().map(|c: &Channel| (c.a, c.b));
        if let Some((pa, pb)) = prev {
            let shared = [a, b].iter().flatten().any(|e| pa == Some(*e) || pb == Some(*e));
            if !shared {
                group += 1;
            }
        }
        let volt = elements(comp, "voltpercm").first().and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| *v > 0.0);
        let gain = match (typical, volt) {
            (Some(t), Some(v)) => (t / v).clamp(0.01, 100.0),
            _ => 1.0,
        };
        let st = stats[parts[0].0];
        m.channels.push(Channel {
            label,
            detail: parts
                .iter()
                .map(|(i, f)| format!("{f:+} \u{d7} {}", signals[*i].label))
                .collect::<Vec<_>>()
                .join("  "),
            terms: parts.iter().map(|(i, f)| (*i as u32, f * lead(*i).display_scale())).collect(),
            minus_mean: false,
            kind: first.kind,
            fs,
            unit: if sensitivity { "\u{b5}V".into() } else { first.display_unit().to_string() },
            sensitivity,
            ac: parts.iter().any(|(i, _)| lead(*i).ac),
            filtered: parts.iter().all(|(i, _)| lead(*i).filtered),
            center: if parts.len() == 1 { st.center } else { 0.0 },
            half_range: if parts.len() == 1 { st.half_range } else { st.half_range * 2.0 },
            side: match (a, b) {
                (Some(a), Some(b)) => pair_side(a, b),
                (Some(a), None) => a.side(),
                _ => Side::None,
            },
            group,
            a,
            b,
            hidden: false,
            gain,
        });
    }
    if m.channels.is_empty() {
        return Err(format!("None of its signals are in this file ({}).", m.missing.join(", ")));
    }
    Ok(m)
}

fn first_reference(l: &Lead) -> Option<ElectrodeId> {
    match l.reference {
        Some(Reference::Electrode(e)) => Some(e),
        _ => None,
    }
}

/// Contents of every `<tag>...</tag>` in `xml` (not nested in itself).
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let body = &rest[i + open.len()..];
        let Some(j) = body.find(&close) else { break };
        out.push(&body[..j]);
        rest = &body[j + close.len()..];
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn recorded(l: &Lead, sig: &Signal, st: SignalStats, group: u16) -> Channel {
    let b = match l.reference {
        Some(Reference::Electrode(e)) => Some(e),
        _ => None,
    };
    let status = l.kind == Kind::Trigger && l.label.eq_ignore_ascii_case("status");
    Channel {
        label: l.display.clone(),
        detail: l.label.clone(),
        terms: vec![(l.signal as u32, l.display_scale())],
        minus_mean: false,
        kind: l.kind,
        fs: sig.fs,
        unit: l.display_unit().to_string(),
        sensitivity: l.sensitivity,
        ac: l.ac,
        filtered: l.filtered,
        center: st.center,
        half_range: st.half_range,
        side: l.side(),
        group,
        a: l.electrode,
        b,
        hidden: status,
        gain: 1.0,
    }
}

/// Non-EEG signals (EOG, ECG, EMG, respiration, ...) go below a derived montage, as recorded.
fn append_others(m: &mut Montage, leads: &LeadSet, signals: &[Signal], stats: &[SignalStats], group: u16) {
    for l in &leads.leads {
        if matches!(l.kind, Kind::Eeg | Kind::Annotation) || signals[l.signal].samples_per_record == 0 {
            continue;
        }
        m.channels.push(recorded(l, &signals[l.signal], stats[l.signal], group));
    }
}

fn pair_side(a: ElectrodeId, b: ElectrodeId) -> Side {
    let (sa, sb) = (a.side(), b.side());
    let off_scalp = |e: ElectrodeId| e.info().site != Site::Scalp;
    if off_scalp(b) || sa == sb || sb == Side::Mid {
        sa
    } else if sa == Side::Mid || off_scalp(a) {
        sb
    } else {
        Side::Mid
    }
}

/// Front-to-back rows, then left to right: Fp1 Fpz Fp2 / AF7 ... / F7 F3 Fz F4 F8 / ...
fn spatial_key(e: ElectrodeId) -> (u8, f32) {
    let info = e.info();
    let prefix: String = info.name.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    let prefix = prefix.trim_end_matches('Z');
    let row = match prefix {
        "N" => 0,
        "FP" => 1,
        "AF" => 2,
        "F" => 3,
        "FC" | "FT" => 4,
        "C" | "T" => 5,
        "CP" | "TP" => 6,
        "P" => 7,
        "PO" => 8,
        "O" => 9,
        "I" => 10,
        _ => 11,
    };
    (row, info.x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edf::test_signal;

    fn set(labels: &[&str]) -> (LeadSet, Vec<Signal>, Vec<SignalStats>) {
        let sigs: Vec<Signal> = labels.iter().enumerate().map(|(i, l)| test_signal(i, l, "uV", 256)).collect();
        let stats = vec![SignalStats::default(); sigs.len()];
        (LeadSet::identify(&sigs), sigs, stats)
    }

    fn find<'a>(m: &'a Montage, label: &str) -> &'a Channel {
        m.channels.iter().find(|c| c.label == label).unwrap_or_else(|| panic!("no {label} in {:?}", m.channels.iter().map(|c| &c.label).collect::<Vec<_>>()))
    }

    fn sorted(mut t: Vec<(u32, f32)>) -> Vec<(u32, f32)> {
        t.sort_by_key(|x| x.0);
        t
    }

    const TUH: [&str; 21] = [
        "EEG FP1-REF", "EEG FP2-REF", "EEG F3-REF", "EEG F4-REF", "EEG C3-REF", "EEG C4-REF", "EEG P3-REF",
        "EEG P4-REF", "EEG O1-REF", "EEG O2-REF", "EEG F7-REF", "EEG F8-REF", "EEG T3-REF", "EEG T4-REF",
        "EEG T5-REF", "EEG T6-REF", "EEG A1-REF", "EEG A2-REF", "EEG FZ-REF", "EEG CZ-REF", "EEG PZ-REF",
    ];

    #[test]
    fn banana_from_referential() {
        let (leads, sigs, stats) = set(&TUH);
        let m = build(MontageId::Banana, &leads, &sigs, &stats).unwrap();
        assert_eq!(m.channels.len(), 18);
        assert!(m.missing.is_empty());
        let c = find(&m, "Fp1\u{2013}F7");
        assert_eq!(sorted(c.terms.clone()), vec![(0, 1.0), (10, -1.0)]);
        assert_eq!(c.side, Side::Left);
        // The file says T3, so the label does too.
        let c = find(&m, "F7\u{2013}T3");
        assert_eq!(sorted(c.terms.clone()), vec![(10, 1.0), (12, -1.0)]);
        assert_eq!(m.channels[0].label, "Fp1\u{2013}F7");
        assert_eq!(m.channels[17].label, "T6\u{2013}O2");
    }

    #[test]
    fn channel_order_in_the_file_does_not_matter() {
        let (leads, sigs, stats) = set(&TUH);
        let reference = build(MontageId::Banana, &leads, &sigs, &stats).unwrap();
        // Same recording, signals in a scrambled order.
        let order = [7, 19, 3, 0, 15, 11, 20, 1, 9, 13, 5, 17, 2, 8, 14, 10, 18, 6, 4, 12, 16];
        let shuffled: Vec<&str> = order.iter().map(|&i| TUH[i]).collect();
        let (leads2, sigs2, stats2) = set(&shuffled);
        let m = build(MontageId::Banana, &leads2, &sigs2, &stats2).unwrap();
        assert_eq!(m.channels.len(), reference.channels.len());
        for (a, b) in reference.channels.iter().zip(&m.channels) {
            assert_eq!(a.label, b.label);
            let labels = |c: &Channel, s: &[Signal]| {
                let mut v: Vec<(String, i32)> = c.terms.iter().map(|(i, k)| (s[*i as usize].label.clone(), *k as i32)).collect();
                v.sort();
                v
            };
            assert_eq!(labels(a, &sigs), labels(b, &sigs2), "{}", a.label);
        }
    }

    #[test]
    fn transverse_from_longitudinal_bipolar() {
        // CHB-MIT style: only bipolar chains are recorded.
        let chb = [
            "FP1-F7", "F7-T7", "T7-P7", "P7-O1", "FP1-F3", "F3-C3", "C3-P3", "P3-O1", "FP2-F4", "F4-C4", "C4-P4",
            "P4-O2", "FP2-F8", "F8-T8", "T8-P8", "P8-O2", "FZ-CZ", "CZ-PZ", "P7-T7", "T7-FT9", "FT9-FT10", "FT10-T8",
            "T8-P8",
        ];
        let (leads, sigs, stats) = set(&chb);
        let m = build(MontageId::Transverse, &leads, &sigs, &stats).unwrap();
        // Check derived channels against synthetic electrode potentials.
        let mut v: HashMap<String, f32> = HashMap::new();
        for (i, name) in ["FP1", "F7", "T7", "P7", "O1", "F3", "C3", "P3", "FP2", "F4", "C4", "P4", "O2", "F8", "T8", "P8", "FZ", "CZ", "PZ", "FT9", "FT10"].iter().enumerate() {
            v.insert(name.to_string(), (i as f32 * 7.3).sin() * 50.0 + i as f32);
        }
        let sample: Vec<f32> = chb.iter().map(|l| {
            let (a, b) = l.split_once('-').unwrap();
            v[a] - v[b]
        }).collect();
        let eval = |c: &Channel| c.terms.iter().map(|(s, k)| sample[*s as usize] * k).sum::<f32>();
        // Left and right chains meet at Fp1/Fp2 via the FT9-FT10 link.
        let c = find(&m, "Fp1\u{2013}Fp2");
        assert!((eval(c) - (v["FP1"] - v["FP2"])).abs() < 1e-3);
        let c = find(&m, "T7\u{2013}C3");
        assert!((eval(c) - (v["T7"] - v["C3"])).abs() < 1e-3);
        // The Fz-Cz-Pz chain is not connected to anything else, so C3-Cz cannot be formed and
        // must not be invented.
        assert!(m.missing.iter().any(|d| d == "C3\u{2013}Cz"));

        // The average reference is derivable from a bipolar recording too (over the connected
        // electrodes).
        let avg = build(MontageId::Average, &leads, &sigs, &stats).unwrap();
        assert_eq!(avg.mean_count, 18);
        let mean = avg.mean_terms.iter().map(|(s, k)| sample[*s as usize] * k).sum::<f32>();
        let connected: Vec<&str> = v.keys().map(|s| s.as_str()).filter(|n| !matches!(*n, "FZ" | "CZ" | "PZ")).collect();
        let true_mean = connected.iter().map(|n| v[*n]).sum::<f32>() / connected.len() as f32;
        let c = find(&avg, "O1\u{2013}Avg");
        assert!(((eval(c) - mean) - (v["O1"] - true_mean)).abs() < 1e-3);
    }

    #[test]
    fn average_excludes_ears_and_unplaced() {
        let mut labels = TUH.to_vec();
        labels.push("EEG EKG1-REF");
        labels.push("EEG X1-REF");
        let (leads, sigs, stats) = set(&labels);
        let m = build(MontageId::Average, &leads, &sigs, &stats).unwrap();
        assert_eq!(m.mean_count, 19, "A1/A2 are not scalp electrodes");
        assert_eq!(m.channels.iter().filter(|c| c.minus_mean).count(), 19);
        assert_eq!(m.channels[0].label, "Fp1\u{2013}Avg");
        assert!(m.channels.iter().any(|c| c.kind == Kind::Ecg));
    }

    #[test]
    fn edfbrowser_montage_file() {
        let labels = ["EEG FP2", "EEG F4", "EEG C4", "EEG X6", "EEG CZ"];
        let sigs: Vec<Signal> = labels.iter().enumerate().map(|(i, l)| test_signal(i, l, "uV", 200)).collect();
        let stats = vec![SignalStats::default(); sigs.len()];
        let leads = LeadSet::identify(&sigs);
        let xml = r#"<?xml version="1.0"?>
<EDFbrowser_montage>
  <signalcomposition>
    <num_of_signals>2</num_of_signals>
    <voltpercm>50.000000</voltpercm>
    <signal><label>EEG F4          </label><factor>1</factor></signal>
    <signal><label>EEG FP2         </label><factor>-1</factor></signal>
    <filter><LPF>0</LPF><frequency>0.1</frequency></filter>
  </signalcomposition>
  <signalcomposition>
    <num_of_signals>2</num_of_signals>
    <voltpercm>50.000000</voltpercm>
    <signal><label>EEG C4          </label><factor>1</factor></signal>
    <signal><label>EEG F4          </label><factor>-1</factor></signal>
  </signalcomposition>
  <signalcomposition>
    <num_of_signals>1</num_of_signals>
    <voltpercm>200.000000</voltpercm>
    <signal><label>EEG X6          </label><factor>1</factor></signal>
  </signalcomposition>
  <signalcomposition>
    <num_of_signals>1</num_of_signals>
    <voltpercm>50.000000</voltpercm>
    <signal><label>EEG O9          </label><factor>1</factor></signal>
  </signalcomposition>
</EDFbrowser_montage>"#;
        let m = from_edfbrowser(xml, &leads, &sigs, &stats).unwrap();
        assert_eq!(m.channels.len(), 3);
        assert_eq!(m.channels[0].label, "F4\u{2013}Fp2");
        assert_eq!(sorted(m.channels[0].terms.clone()), vec![(0, -1.0), (1, 1.0)]);
        assert_eq!(m.channels[0].side, Side::Right);
        assert_eq!((m.channels[0].a, m.channels[0].b), (electrode("F4").map(|e| e.0), electrode("Fp2").map(|e| e.0)));
        assert_eq!(m.channels[1].group, m.channels[0].group, "C4-F4 continues the chain");
        assert_eq!(m.channels[2].label, "X6");
        assert!((m.channels[2].gain - 0.25).abs() < 1e-6, "200 µV/cm against a typical 50");
        assert_eq!(m.missing, vec!["EEG O9".to_string()]);
        assert!(from_edfbrowser("<html/>", &leads, &sigs, &stats).is_err());
    }

    #[test]
    fn sleep_recording_offers_only_as_recorded() {
        let (leads, sigs, stats) = set(&["EEG Fpz-Cz", "EEG Pz-Oz", "EOG horizontal", "Resp oro-nasal", "EMG submental"]);
        let cat = catalog(&leads, &sigs, &stats);
        assert!(cat[0].1.is_ok());
        assert!(cat.iter().filter(|(_, m)| m.is_ok()).count() == 1);
    }
}
