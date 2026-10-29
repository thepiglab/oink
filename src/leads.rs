//! Works out what every signal in a file is: its type (EEG, EOG, ECG, ...), the scalp
//! electrode it was recorded from, and its reference.
//!
//! Signals are identified by their label (plus type prefix and unit), never by their position
//! in the file: "EEG FP1-REF" can be signal 0 in one recording and signal 17 in the next.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::edf::Signal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Eeg,
    Eog,
    Ecg,
    Emg,
    Resp,
    SpO2,
    Pleth,
    Pulse,
    Temp,
    Position,
    Light,
    Sound,
    Trigger,
    Annotation,
    Other,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Eeg => "EEG",
            Kind::Eog => "EOG",
            Kind::Ecg => "ECG",
            Kind::Emg => "EMG",
            Kind::Resp => "Respiration",
            Kind::SpO2 => "SpO\u{2082}",
            Kind::Pleth => "Pleth",
            Kind::Pulse => "Heart rate",
            Kind::Temp => "Temperature",
            Kind::Position => "Position",
            Kind::Light => "Light",
            Kind::Sound => "Sound",
            Kind::Trigger => "Trigger",
            Kind::Annotation => "Annotations",
            Kind::Other => "Other",
        }
    }

    /// Short tag shown next to non-EEG channels.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Eeg => "EEG",
            Kind::Eog => "EOG",
            Kind::Ecg => "ECG",
            Kind::Emg => "EMG",
            Kind::Resp => "RESP",
            Kind::SpO2 => "SpO\u{2082}",
            Kind::Pleth => "PLETH",
            Kind::Pulse => "HR",
            Kind::Temp => "TEMP",
            Kind::Position => "POS",
            Kind::Light => "LIGHT",
            Kind::Sound => "SOUND",
            Kind::Trigger => "TRIG",
            Kind::Annotation => "ANN",
            Kind::Other => "AUX",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Mid,
    None,
}

/// Where an electrode sits. Only scalp electrodes take part in the average reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    Scalp,
    Ear,
    Mastoid,
    Eye,
    Nasopharyngeal,
    Sphenoidal,
}

#[derive(Clone, Debug)]
pub struct Electrode {
    /// Modern (10-10) name with standard capitalisation, e.g. `Fp1`, `T7`, `AFz`.
    pub name: String,
    /// Head-map position: nose up, unit circle at the nasion–inion level, Cz at the origin.
    pub x: f32,
    pub y: f32,
    pub site: Site,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElectrodeId(pub u16);

impl ElectrodeId {
    pub fn info(self) -> &'static Electrode {
        &table().electrodes[self.0 as usize]
    }

    pub fn side(self) -> Side {
        let x = self.info().x;
        if x < -0.02 {
            Side::Left
        } else if x > 0.02 {
            Side::Right
        } else {
            Side::Mid
        }
    }
}

/// The 19 sites of the classic 10-20 system, in modern names.
pub const TEN_TWENTY: [&str; 19] = [
    "Fp1", "Fp2", "F7", "F3", "Fz", "F4", "F8", "T7", "C3", "Cz", "C4", "T8", "P7", "P3", "Pz", "P4", "P8", "O1", "O2",
];

struct Table {
    electrodes: Vec<Electrode>,
    /// Upper-case name (canonical or alias) -> electrode and the name to show for it.
    names: HashMap<String, (ElectrodeId, String)>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

pub fn electrodes() -> &'static [Electrode] {
    &table().electrodes
}

/// Looks up an electrode by its modern or legacy name, case-insensitively. Returns the
/// electrode and the name to display (legacy names are kept as written, e.g. `T3`).
pub fn electrode(name: &str) -> Option<(ElectrodeId, &'static str)> {
    table().names.get(&name.trim().to_ascii_uppercase()).map(|(id, n)| (*id, n.as_str()))
}

fn sph(theta: f32, phi: f32) -> [f32; 3] {
    let (t, p) = (theta.to_radians(), phi.to_radians());
    [t.sin() * p.sin(), t.sin() * p.cos(), t.cos()]
}

fn slerp(a: [f32; 3], b: [f32; 3], f: f32) -> [f32; 3] {
    let d = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0);
    let om = d.acos();
    if om < 1e-6 {
        return a;
    }
    let s = om.sin();
    let (wa, wb) = (((1.0 - f) * om).sin() / s, (f * om).sin() / s);
    [a[0] * wa + b[0] * wb, a[1] * wa + b[1] * wb, a[2] * wa + b[2] * wb]
}

/// Azimuthal equidistant projection: distance from Cz is proportional to the angle from the
/// vertex, so the 10-20 outer ring (72 deg) lands at r = 0.8 and the nasion level at r = 1.
fn project(v: [f32; 3]) -> (f32, f32) {
    let r = v[2].clamp(-1.0, 1.0).acos().to_degrees() / 90.0;
    let phi = v[0].atan2(v[1]);
    (r * phi.sin(), r * phi.cos())
}

/// Builds 10-10 positions on an idealised spherical head: Cz at the pole, the 10-20 outer ring
/// (Fpz, T7, Oz, T8) 72 deg below it, rows 18 deg apart along the midline, and each row's
/// lateral sites spaced evenly along the arc from the midline site to the outer ring.
fn build_table() -> Table {
    let mut electrodes = Vec::new();
    let mut add = |name: String, (x, y): (f32, f32), site: Site| electrodes.push(Electrode { name, x, y, site });

    let midline = [
        ("Nz", 90.0, false),
        ("Fpz", 72.0, false),
        ("AFz", 54.0, false),
        ("Fz", 36.0, false),
        ("FCz", 18.0, false),
        ("Cz", 0.0, false),
        ("CPz", 18.0, true),
        ("Pz", 36.0, true),
        ("POz", 54.0, true),
        ("Oz", 72.0, true),
        ("Iz", 90.0, true),
    ];
    for (name, theta, back) in midline {
        add(name.into(), project(sph(theta, if back { 180.0 } else { 0.0 })), Site::Scalp);
    }
    for (name, phi) in [("Fp1", -18.0), ("Fp2", 18.0), ("O1", -162.0), ("O2", 162.0)] {
        add(name.into(), project(sph(72.0, phi)), Site::Scalp);
    }
    // (row, polar angle of its midline site, behind Cz?, azimuth of its outer-ring site, outer name)
    let rows = [
        ("AF", 54.0, false, 36.0, "AF"),
        ("F", 36.0, false, 54.0, "F"),
        ("FC", 18.0, false, 72.0, "FT"),
        ("C", 0.0, false, 90.0, "T"),
        ("CP", 18.0, true, 108.0, "TP"),
        ("P", 36.0, true, 126.0, "P"),
        ("PO", 54.0, true, 144.0, "PO"),
    ];
    for (row, theta, back, phi_outer, outer) in rows {
        let mid = sph(theta, if back { 180.0 } else { 0.0 });
        for side in [-1.0f32, 1.0] {
            let outer_v = sph(72.0, side * phi_outer);
            let odd = side < 0.0;
            for (k, f) in [(1, 0.25), (3, 0.5), (5, 0.75)] {
                let num = if odd { k } else { k + 1 };
                add(format!("{row}{num}"), project(slerp(mid, outer_v, f)), Site::Scalp);
            }
            add(format!("{outer}{}", if odd { 7 } else { 8 }), project(outer_v), Site::Scalp);
            add(format!("{outer}{}", if odd { 9 } else { 10 }), project(sph(90.0, side * phi_outer)), Site::Scalp);
        }
    }
    let specials = [
        ("A1", -1.12, 0.0, Site::Ear),
        ("A2", 1.12, 0.0, Site::Ear),
        ("M1", -1.06, -0.24, Site::Mastoid),
        ("M2", 1.06, -0.24, Site::Mastoid),
        ("T1", -0.95, 0.38, Site::Scalp),
        ("T2", 0.95, 0.38, Site::Scalp),
        ("Sp1", -1.04, 0.53, Site::Sphenoidal),
        ("Sp2", 1.04, 0.53, Site::Sphenoidal),
        ("PG1", -0.15, 1.13, Site::Nasopharyngeal),
        ("PG2", 0.15, 1.13, Site::Nasopharyngeal),
        ("LOC", -0.42, 1.04, Site::Eye),
        ("ROC", 0.42, 1.04, Site::Eye),
    ];
    for (name, x, y, site) in specials {
        add(name.into(), (x, y), site);
    }

    let mut names = HashMap::new();
    for (i, e) in electrodes.iter().enumerate() {
        names.insert(e.name.to_ascii_uppercase(), (ElectrodeId(i as u16), e.name.clone()));
    }
    // Legacy 10-20 temporal names, and the AASM names for the eye electrodes.
    for (alias, target) in [("T3", "T7"), ("T4", "T8"), ("T5", "P7"), ("T6", "P8"), ("E1", "LOC"), ("E2", "ROC")] {
        let id = names[&target.to_ascii_uppercase()].0;
        names.insert(alias.to_string(), (id, alias.to_string()));
    }
    Table { electrodes, names }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// Normalised for display (`uV` becomes `µV`).
    pub display: String,
    /// Factor to convert to microvolts, for voltage units.
    pub to_uv: Option<f32>,
}

pub fn parse_unit(raw: &str) -> Unit {
    let t = raw.trim();
    let norm = t.replace(['\u{b5}', '\u{3bc}'], "u").to_ascii_lowercase();
    let (display, to_uv) = match norm.as_str() {
        "uv" | "microv" | "microvolt" | "microvolts" => ("\u{b5}V", 1.0),
        "mv" | "millivolt" | "millivolts" => ("mV", 1e3),
        "v" | "volt" | "volts" => ("V", 1e6),
        "nv" => ("nV", 1e-3),
        _ => return Unit { display: t.to_string(), to_uv: None },
    };
    Unit { display: display.into(), to_uv: Some(to_uv) }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Reference {
    /// An electrode with a known position (e.g. `C3-M2`, or the second lead of a bipolar pair).
    Electrode(ElectrodeId),
    /// A named reference without a position: `REF`, linked ears, average, ...
    Named(String),
    /// No reference in the label; a common recording reference is implied.
    Unstated,
}

/// How a cap numbers its channels when it does not use 10-10 names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbering {
    TenTen,
    /// BioSemi ABC layout: A1..A32, B1..B32, ... (so `A1` is not the left earlobe).
    BioSemi,
    /// EGI geodesic nets: E1..E256 (so `E1`/`E2` are not eye electrodes).
    Egi,
}

#[derive(Clone, Debug)]
pub struct Lead {
    pub signal: usize,
    pub label: String,
    pub kind: Kind,
    pub electrode: Option<ElectrodeId>,
    /// The electrode's name as this file spells it, with standard capitalisation (`T3`).
    pub electrode_name: Option<String>,
    pub reference: Option<Reference>,
    /// Name of the reference as written (`REF`, `LE`, `M2`).
    pub reference_name: Option<String>,
    /// Short label for the trace view.
    pub display: String,
    pub unit: Unit,
    /// Drawn with the global sensitivity (µV/mm) instead of being fitted to its lane.
    pub sensitivity: bool,
    /// Oscillates around zero: the DC offset is removed for display.
    pub ac: bool,
    /// The global high-pass, low-pass and notch filters apply.
    pub filtered: bool,
    pub note: Option<String>,
}

impl Lead {
    /// Multiplier from the file's unit to the unit shown on screen (µV for EEG-like signals).
    pub fn display_scale(&self) -> f32 {
        if self.sensitivity { self.unit.to_uv.unwrap_or(1.0) } else { 1.0 }
    }

    pub fn display_unit(&self) -> &str {
        if self.sensitivity { "\u{b5}V" } else { &self.unit.display }
    }

    /// Hemisphere of the lead. A reference off the scalp (ear, mastoid) does not change it:
    /// C4-M1 is a right-hemisphere channel.
    pub fn side(&self) -> Side {
        let Some(a) = self.electrode else { return Side::None };
        match &self.reference {
            Some(Reference::Electrode(b)) if b.info().site == Site::Scalp && a.info().site == Site::Scalp => {
                let (sa, sb) = (a.side(), b.side());
                if sa == sb || sb == Side::Mid {
                    sa
                } else if sa == Side::Mid {
                    sb
                } else {
                    Side::Mid
                }
            }
            _ => a.side(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// EEG leads, with or without a known position.
    pub eeg: usize,
    /// EEG and EOG leads matched to an electrode position.
    pub positioned: usize,
    /// How many of the 19 classic 10-20 sites are present.
    pub ten_twenty: usize,
    /// The reference most EEG leads share, if any.
    pub reference: Option<String>,
    /// Most EEG leads are differences between two scalp electrodes.
    pub bipolar: bool,
    /// The file uses T3/T4/T5/T6 rather than T7/T8/P7/P8.
    pub legacy_names: bool,
    pub numbering: Option<Numbering>,
    pub kinds: Vec<(Kind, usize)>,
    /// EEG-labelled signals whose electrode could not be placed.
    pub unplaced: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LeadSet {
    pub leads: Vec<Lead>,
    pub numbering: Numbering,
    pub summary: Summary,
}

impl LeadSet {
    pub fn identify(signals: &[Signal]) -> LeadSet {
        let numbering = detect_numbering(signals);
        let leads: Vec<Lead> = signals.iter().map(|s| identify_one(s, numbering)).collect();
        let summary = summarise(&leads, numbering);
        LeadSet { leads, numbering, summary }
    }
}

const TYPE_PREFIXES: [(&str, Option<Kind>); 16] = [
    ("SEEG", Some(Kind::Eeg)),
    ("ECOG", Some(Kind::Eeg)),
    ("EEG", Some(Kind::Eeg)),
    ("POL", None),
    ("EOG", Some(Kind::Eog)),
    ("ECG", Some(Kind::Ecg)),
    ("EKG", Some(Kind::Ecg)),
    ("EMG", Some(Kind::Emg)),
    ("RESP", Some(Kind::Resp)),
    ("TEMP", Some(Kind::Temp)),
    ("SAO2", Some(Kind::SpO2)),
    ("SPO2", Some(Kind::SpO2)),
    ("LIGHT", Some(Kind::Light)),
    ("SOUND", Some(Kind::Sound)),
    ("EVENT", Some(Kind::Trigger)),
    ("MARKER", Some(Kind::Trigger)),
];

/// (keyword, kind, matches as a prefix of a word rather than the whole word)
const KEYWORDS: [(&str, Kind, bool); 54] = [
    ("ECG", Kind::Ecg, true),
    ("EKG", Kind::Ecg, true),
    ("EOG", Kind::Eog, true),
    ("HEOG", Kind::Eog, true),
    ("VEOG", Kind::Eog, true),
    ("EYE", Kind::Eog, true),
    ("EMG", Kind::Emg, true),
    ("CHIN", Kind::Emg, true),
    ("LEG", Kind::Emg, true),
    ("LLEG", Kind::Emg, true),
    ("RLEG", Kind::Emg, true),
    ("LAT", Kind::Emg, false),
    ("RAT", Kind::Emg, false),
    ("SUBMENTAL", Kind::Emg, true),
    ("TIBIAL", Kind::Emg, true),
    ("RESP", Kind::Resp, true),
    ("THOR", Kind::Resp, true),
    ("CHEST", Kind::Resp, true),
    ("ABD", Kind::Resp, true),
    ("FLOW", Kind::Resp, true),
    ("AIRFLOW", Kind::Resp, true),
    ("AIR", Kind::Resp, false),
    ("NASAL", Kind::Resp, true),
    ("PRES", Kind::Resp, true),
    ("CANNULA", Kind::Resp, true),
    ("THERM", Kind::Resp, true),
    ("EFFORT", Kind::Resp, true),
    ("RIP", Kind::Resp, false),
    ("PTAF", Kind::Resp, true),
    ("SPO2", Kind::SpO2, true),
    ("SAO2", Kind::SpO2, true),
    ("SAT", Kind::SpO2, false),
    ("PLETH", Kind::Pleth, true),
    ("PPG", Kind::Pleth, false),
    ("PULSE", Kind::Pulse, true),
    ("HR", Kind::Pulse, false),
    ("PR", Kind::Pulse, false),
    ("DHR", Kind::Pulse, false),
    ("BPM", Kind::Pulse, false),
    ("POS", Kind::Position, false),
    ("POSITION", Kind::Position, true),
    ("BODY", Kind::Position, false),
    ("TEMP", Kind::Temp, true),
    ("LIGHT", Kind::Light, true),
    ("LUX", Kind::Light, false),
    ("SNORE", Kind::Sound, true),
    ("SOUND", Kind::Sound, true),
    ("MIC", Kind::Sound, false),
    ("STATUS", Kind::Trigger, false),
    ("TRIGGER", Kind::Trigger, true),
    ("TRIG", Kind::Trigger, false),
    ("STI", Kind::Trigger, false),
    ("STIM", Kind::Trigger, true),
    ("PHOTIC", Kind::Trigger, true),
];

/// Splits a leading type word (`EEG`, `ECG`, ...) off a label. Returns the kind it names (None
/// for vendor prefixes like `POL`) and the prefix length (0 when there is none).
fn split_prefix(label: &str) -> (Option<Kind>, usize) {
    let upper = label.to_ascii_uppercase();
    for (p, kind) in TYPE_PREFIXES {
        if upper.starts_with(p) && !upper[p.len()..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            return (kind, p.len());
        }
    }
    (None, 0)
}

fn keyword_kind(label: &str) -> Option<Kind> {
    let upper = label.to_ascii_uppercase().replace('.', "");
    let words: Vec<&str> = upper.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    for (kw, kind, prefix) in KEYWORDS {
        if words.iter().any(|w| if prefix { w.starts_with(kw) } else { *w == kw }) {
            return Some(kind);
        }
    }
    None
}

fn named_reference(token: &str) -> Option<&'static str> {
    let u: String = token.to_ascii_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    Some(match u.as_str() {
        "REF" | "REFERENCE" | "REF0" | "SYSREF" | "VREF" => "REF",
        "LE" | "LINKEDEARS" | "EARS" | "A1A2" | "A12" => "linked ears",
        "LM" | "M1M2" | "LINKEDMASTOIDS" => "linked mastoids",
        "AR" | "AV" | "AVG" | "AVE" | "AVERAGE" | "CAR" => "average",
        "CMS" | "CMSDRL" => "CMS",
        _ if u.contains("A1") && u.contains("A2") => "linked ears",
        _ if u.contains("M1") && u.contains("M2") => "linked mastoids",
        _ => return None,
    })
}

fn cap_number(token: &str, numbering: Numbering) -> bool {
    let t = token.to_ascii_uppercase();
    match numbering {
        Numbering::TenTen => false,
        Numbering::BioSemi => is_biosemi(&t),
        Numbering::Egi => is_egi(&t),
    }
}

fn is_biosemi(t: &str) -> bool {
    let mut c = t.chars();
    matches!(c.next(), Some('A'..='H')) && c.as_str().parse::<u32>().is_ok_and(|n| (1..=32).contains(&n))
}

fn is_egi(t: &str) -> bool {
    t.strip_prefix('E').and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| (1..=280).contains(&n))
}

fn first_token(label: &str) -> String {
    let clean = label.trim().trim_end_matches('.');
    let (_, n) = split_prefix(clean);
    let rest = clean[n..].trim_start_matches([' ', '-', '_', ':']);
    rest.split(['-', ' ']).next().unwrap_or("").trim().to_ascii_uppercase()
}

fn detect_numbering(signals: &[Signal]) -> Numbering {
    let tokens: Vec<String> = signals.iter().map(|s| first_token(&s.label)).collect();
    let egi = tokens.iter().filter(|t| is_egi(t)).count();
    let biosemi: Vec<&String> = tokens.iter().filter(|t| is_biosemi(t)).collect();
    // E1..E32 fit both patterns; a cap whose only bank is E is an EGI net.
    let only_e = biosemi.iter().all(|t| t.starts_with('E'));
    if biosemi.len() >= 16 && !only_e {
        Numbering::BioSemi
    } else if egi >= 16 {
        Numbering::Egi
    } else {
        Numbering::TenTen
    }
}

fn identify_one(sig: &Signal, numbering: Numbering) -> Lead {
    let label = sig.label.trim();
    let mut unit = parse_unit(&sig.unit);
    let mut lead = Lead {
        signal: sig.index,
        label: label.to_string(),
        kind: Kind::Other,
        electrode: None,
        electrode_name: None,
        reference: None,
        reference_name: None,
        display: label.to_string(),
        unit: unit.clone(),
        sensitivity: false,
        ac: false,
        filtered: false,
        note: None,
    };
    if sig.is_annotation {
        lead.kind = Kind::Annotation;
        return lead;
    }

    // PhysioNet's EEGMMIDB pads labels with dots ("Fc5.", "C3..").
    let clean = label.trim_end_matches('.').trim();
    let (prefix_kind, prefix_len) = split_prefix(clean);
    let has_prefix = prefix_len > 0;
    let spaced_prefix = clean[prefix_len..].starts_with(' ');
    let rest = clean[prefix_len..].trim_start_matches([' ', '-', '_', ':']).trim();
    let parenthesised = rest.starts_with('(');
    let rest = rest.trim_start_matches('(').trim_end_matches(')').trim();
    let parts: Vec<&str> = rest.split(['-', '\u{2013}', '\u{2212}']).map(str::trim).filter(|p| !p.is_empty()).collect();

    let mut active = parts.first().copied();
    let mut reference = parts.get(1).copied();
    // "EMG-REF": a type with only a reference after it.
    if has_prefix && parts.len() == 1 && named_reference(parts[0]).is_some() {
        reference = active.take();
    }

    let mut found = active.filter(|t| !cap_number(t, numbering)).and_then(electrode);
    // MESA/SHHS style "EOG-L", "EOG(R)".
    if found.is_none() && prefix_kind == Some(Kind::Eog) {
        let side = match active.map(str::to_ascii_uppercase).as_deref() {
            Some("L" | "LEFT" | "LT") => Some("LOC"),
            Some("R" | "RIGHT" | "RT") => Some("ROC"),
            _ => None,
        };
        found = side.and_then(electrode);
    }
    // A1/E1 on a BioSemi/EGI cap are channel numbers, not the earlobe or the eye.
    let numbered = active.is_some_and(|t| cap_number(t, numbering));

    lead.kind = if is_lead_off(clean) || active.is_some_and(is_aux_input) && found.is_none() && !numbered {
        Kind::Other
    } else {
        match (prefix_kind, found) {
            (Some(k), _) if k != Kind::Eeg => k,
            (_, Some((id, _))) if id.info().site == Site::Eye => Kind::Eog,
            (_, Some(_)) => Kind::Eeg,
            _ if numbered => Kind::Eeg,
            _ => keyword_kind(clean).or(prefix_kind).unwrap_or(Kind::Other),
        }
    };

    if let Some((id, name)) = found {
        lead.electrode = Some(id);
        lead.electrode_name = Some(name.to_string());
        let info = id.info();
        lead.note = match info.site {
            Site::Ear => Some("ear electrode".into()),
            Site::Mastoid => Some("mastoid electrode".into()),
            Site::Nasopharyngeal => Some("nasopharyngeal electrode".into()),
            Site::Sphenoidal => Some("sphenoidal electrode".into()),
            Site::Eye => Some("eye (EOG) electrode".into()),
            Site::Scalp if name != info.name => Some(format!("legacy name for {}", info.name)),
            Site::Scalp => None,
        };
    } else if numbered {
        lead.note = Some(match numbering {
            Numbering::BioSemi => "BioSemi cap channel; position not in the file".into(),
            _ => "EGI net sensor; position not in the file".into(),
        });
    }

    // Only read "x-y" as a derivation when it is one; "Resp oro-nasal" is just a name.
    let ref_electrode = reference.filter(|r| !cap_number(r, numbering)).and_then(electrode);
    let ref_named = reference.and_then(named_reference);
    if let Some(r) = reference.filter(|_| found.is_some() || numbered || ref_named.is_some() || ref_electrode.is_some()) {
        if let Some(name) = ref_named {
            lead.reference = Some(Reference::Named(name.to_string()));
            lead.reference_name = Some(r.to_string());
        } else if let Some((id, name)) = ref_electrode {
            lead.reference = Some(Reference::Electrode(id));
            lead.reference_name = Some(name.to_string());
        } else {
            lead.reference = Some(Reference::Named(r.to_ascii_uppercase()));
            lead.reference_name = Some(r.to_string());
        }
    } else if found.is_some() || numbered {
        lead.reference = Some(Reference::Unstated);
    }

    lead.display = match (&lead.electrode_name, &lead.reference) {
        (Some(a), Some(Reference::Electrode(_))) => format!("{a}\u{2013}{}", lead.reference_name.as_deref().unwrap_or("")),
        (Some(a), _) => a.clone(),
        (None, _) => {
            // Drop a generic "EEG "/"POL " prefix ("EEG X1" -> "X1") but keep labels that are
            // only a type and a number ("EEG 26", "EEG2").
            let numeric = active.is_some_and(|t| t.chars().all(|c| c.is_ascii_digit()));
            let generic = has_prefix && (prefix_kind == Some(Kind::Eeg) || prefix_kind.is_none());
            let base = if generic && spaced_prefix && !numeric && !parenthesised && !rest.is_empty() {
                rest
            } else if has_prefix && rest.eq_ignore_ascii_case(&clean[..prefix_len]) {
                &clean[..prefix_len] // "ECG ECG", "EKG EKG"
            } else {
                clean
            };
            strip_named_suffix(base).to_string()
        }
    };
    if lead.display.is_empty() || lead.display == "-" {
        lead.display = format!("Signal {}", sig.index + 1);
    }

    // Units. An EEG signal without a usable unit is assumed to be in µV.
    let eeg_like = matches!(lead.kind, Kind::Eeg | Kind::Eog | Kind::Emg);
    if eeg_like && unit.to_uv.is_none() {
        lead.note.get_or_insert_with(|| format!("unit {:?} not recognised; assuming \u{b5}V", sig.unit.trim()));
        unit = Unit { display: "\u{b5}V".into(), to_uv: Some(1.0) };
    }
    // Auxiliary inputs in µV are usually ECG/EOG/EMG electrodes: treat them as AC signals, but
    // fit them to their lane since their amplitude is unknown.
    let aux_electrode = lead.kind == Kind::Other && unit.to_uv.is_some_and(|s| s <= 1.0);
    lead.sensitivity = eeg_like;
    lead.ac = eeg_like || aux_electrode || matches!(lead.kind, Kind::Ecg | Kind::Resp | Kind::Pleth | Kind::Sound);
    lead.filtered = eeg_like || aux_electrode || lead.kind == Kind::Ecg;
    lead.unit = unit;
    lead
}

/// Amplifier auxiliary inputs ("EEG X1" on Nihon Kohden, "DC01", BioSemi "EXG1"): they carry
/// whatever was plugged in, so they are not drawn at the EEG sensitivity.
fn is_aux_input(token: &str) -> bool {
    let t = token.to_ascii_uppercase();
    ["X", "DC", "AUX", "EXG"].iter().any(|p| t.strip_prefix(p).is_some_and(|n| n.is_empty() && *p == "AUX" || !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
}

/// Lead-off / status channels such as "EKG_Off".
fn is_lead_off(label: &str) -> bool {
    label
        .to_ascii_uppercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "OFF" || w.len() > 3 && w.ends_with("OFF"))
}

/// "PHOTIC-REF" -> "PHOTIC"
fn strip_named_suffix(s: &str) -> &str {
    if let Some(i) = s.rfind('-') {
        if named_reference(&s[i + 1..]).is_some() && i > 0 {
            return s[..i].trim();
        }
    }
    s.trim()
}

fn summarise(leads: &[Lead], numbering: Numbering) -> Summary {
    let mut s = Summary { numbering: (numbering != Numbering::TenTen).then_some(numbering), ..Default::default() };
    let mut kinds: HashMap<Kind, usize> = HashMap::new();
    let mut refs: HashMap<String, usize> = HashMap::new();
    let mut electrode_refs = 0usize;
    let mut sites = std::collections::HashSet::new();
    for l in leads {
        if l.kind == Kind::Annotation {
            continue;
        }
        *kinds.entry(l.kind).or_default() += 1;
        if matches!(l.kind, Kind::Eeg | Kind::Eog) && l.electrode.is_some() {
            s.positioned += 1;
        }
        if l.kind != Kind::Eeg {
            continue;
        }
        s.eeg += 1;
        match l.electrode {
            Some(id) => {
                sites.insert(id);
                if l.electrode_name.as_deref().is_some_and(|n| matches!(n, "T3" | "T4" | "T5" | "T6")) {
                    s.legacy_names = true;
                }
            }
            None => s.unplaced.push(l.label.clone()),
        }
        match &l.reference {
            Some(Reference::Named(n)) => *refs.entry(n.clone()).or_default() += 1,
            Some(Reference::Unstated) => *refs.entry(String::new()).or_default() += 1,
            Some(Reference::Electrode(id)) => {
                if id.info().site == Site::Scalp {
                    electrode_refs += 1;
                } else {
                    *refs.entry(id.info().name.clone()).or_default() += 1;
                }
            }
            None => {}
        }
    }
    s.ten_twenty = TEN_TWENTY.iter().filter(|n| electrode(n).is_some_and(|(id, _)| sites.contains(&id))).count();
    let referenced: usize = refs.values().sum();
    s.bipolar = electrode_refs > referenced && electrode_refs >= 2;
    if let Some((name, n)) = refs.iter().max_by_key(|(_, n)| **n) {
        if *n * 2 > referenced + electrode_refs && !name.is_empty() {
            s.reference = Some(name.clone());
        }
    }
    let mut kinds: Vec<(Kind, usize)> = kinds.into_iter().collect();
    kinds.sort();
    s.kinds = kinds;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(i: usize, label: &str, unit: &str) -> Signal {
        crate::edf::test_signal(i, label, unit, 256)
    }

    fn one(label: &str, unit: &str) -> Lead {
        identify_one(&sig(0, label, unit), Numbering::TenTen)
    }

    fn name(l: &Lead) -> Option<&str> {
        l.electrode.map(|e| e.info().name.as_str())
    }

    #[test]
    fn positions_are_sane() {
        let pos = |n: &str| {
            let (id, _) = electrode(n).unwrap();
            (id.info().x, id.info().y)
        };
        let close = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        assert!(close(pos("Cz"), (0.0, 0.0)));
        assert!(close(pos("Fpz"), (0.0, 0.8)));
        assert!(close(pos("T7"), (-0.8, 0.0)));
        assert!(close(pos("C3"), (-0.4, 0.0)));
        assert!(close(pos("T9"), (-1.0, 0.0)));
        assert!(close(pos("Oz"), (0.0, -0.8)));
        // F3 sits between Fz and F7, front-left.
        let (x, y) = pos("F3");
        assert!(x < -0.3 && x > -0.6 && y > 0.3 && y < 0.6, "F3 at {x},{y}");
        // Mirror symmetry and aliases.
        assert!(close(pos("F4"), (-pos("F3").0, pos("F3").1)));
        assert!(close(pos("T3"), pos("T7")));
        assert_eq!(electrode("fp1").unwrap().1, "Fp1");
        assert_eq!(electrode("FPZ").unwrap().1, "Fpz");
        assert_eq!(electrode("t5").unwrap().1, "T5");
        assert_eq!(electrode("AFZ").unwrap().1, "AFz");
        assert!(electrodes().len() > 90);
    }

    #[test]
    fn tuh_labels() {
        let l = one("EEG FP1-REF", "uV");
        assert_eq!((l.kind, name(&l), l.display.as_str()), (Kind::Eeg, Some("Fp1"), "Fp1"));
        assert_eq!(l.reference, Some(Reference::Named("REF".into())));
        let l = one("EEG T3-LE", "uV");
        assert_eq!((name(&l), l.display.as_str()), (Some("T7"), "T3"));
        assert_eq!(l.reference, Some(Reference::Named("linked ears".into())));
        let l = one("EEG EKG1-REF", "uV");
        assert_eq!((l.kind, l.display.as_str()), (Kind::Ecg, "EKG1"));
        let l = one("EMG-REF", "uV");
        assert_eq!((l.kind, l.display.as_str()), (Kind::Emg, "EMG"));
        let l = one("PHOTIC-REF", "uV");
        assert_eq!((l.kind, l.display.as_str()), (Kind::Trigger, "PHOTIC"));
        let l = one("EEG 26-REF", "uV");
        assert_eq!((l.kind, l.electrode, l.display.as_str()), (Kind::Eeg, None, "EEG 26"));
        let l = one("EEG A1-REF", "uV");
        assert_eq!((name(&l), l.note.as_deref()), (Some("A1"), Some("ear electrode")));
        let l = one("EEG SP2-REF", "uV");
        assert_eq!(name(&l), Some("Sp2"));
    }

    #[test]
    fn bipolar_and_sleep_labels() {
        let l = one("FP1-F7", "uV");
        assert_eq!((l.kind, name(&l), l.display.as_str()), (Kind::Eeg, Some("Fp1"), "Fp1\u{2013}F7"));
        assert_eq!(l.reference, Some(Reference::Electrode(electrode("F7").unwrap().0)));
        assert_eq!(l.side(), Side::Left);
        let l = one("EEG Fpz-Cz", "uV");
        assert_eq!((name(&l), l.display.as_str(), l.side()), (Some("Fpz"), "Fpz\u{2013}Cz", Side::Mid));
        let l = one("C4-M1", "uV");
        assert_eq!((l.display.as_str(), l.side()), ("C4\u{2013}M1", Side::Right));
        let l = one("E1-M2", "uV");
        assert_eq!((l.kind, name(&l)), (Kind::Eog, Some("LOC")));
        assert_eq!(one("EOG horizontal", "uV").kind, Kind::Eog);
        assert_eq!(one("EOG(L)", "uV").electrode, electrode("LOC").map(|e| e.0));
        assert_eq!(one("EOG-R", "uV").electrode, electrode("ROC").map(|e| e.0));
        let l = one("Resp oro-nasal", "");
        assert_eq!((l.kind, l.electrode), (Kind::Resp, None));
        assert_eq!(one("EMG submental", "uV").kind, Kind::Emg);
        assert_eq!(one("Temp rectal", "DegC").kind, Kind::Temp);
        assert_eq!(one("Event marker", "").kind, Kind::Trigger);
        assert_eq!(one("SaO2", "%").kind, Kind::SpO2);
        assert_eq!(one("H.R.", "bpm").kind, Kind::Pulse);
        assert_eq!(one("THOR RES", "").kind, Kind::Resp);
        assert_eq!(one("EEG(sec)", "uV").kind, Kind::Eeg);
        assert_eq!(one("EEG (sec)", "uV").display, "EEG (sec)");
        assert_eq!(one("PR", "bpm").kind, Kind::Pulse);
        assert_eq!(one("New Air", "").kind, Kind::Resp);
        assert_eq!(one("EKG EKG", "uV").kind, Kind::Ecg);
        assert_eq!(one("ECG ECG", "mV").display, "ECG");
        let l = one("EEG C3-(A1+A2)/2", "uV");
        assert_eq!(l.reference, Some(Reference::Named("linked ears".into())));
        assert_eq!(one("EKG_Off", "").kind, Kind::Other);
        assert_eq!(one("Leg", "uV").kind, Kind::Emg);
        assert_eq!(one("POSITION", "").kind, Kind::Position);
        let l = one("EEG2", "uV");
        assert_eq!((l.kind, l.display.as_str()), (Kind::Eeg, "EEG2"));
    }

    #[test]
    fn eegmmidb_and_nihon_kohden_labels() {
        assert_eq!(one("Fc5.", "uV").display, "FC5");
        assert_eq!(one("C3..", "uV").display, "C3");
        assert_eq!(one("Iz..", "uV").display, "Iz");
        let l = one("EEG PG1", "uV");
        assert_eq!((name(&l), l.reference.clone()), (Some("PG1"), Some(Reference::Unstated)));
        let l = one("EEG E", "uV");
        assert_eq!((l.kind, l.electrode, l.display.as_str()), (Kind::Eeg, None, "E"));
        let l = one("EEG X1", "uV");
        assert_eq!((l.kind, l.display.as_str(), l.sensitivity, l.ac), (Kind::Other, "X1", false, true));
        assert_eq!(one("EXG3", "uV").kind, Kind::Other);
        let l = one("DC01", "mV");
        assert_eq!((l.kind, l.sensitivity, l.ac), (Kind::Other, false, false));
    }

    #[test]
    fn cap_numbering_is_not_mistaken_for_electrodes() {
        let mut sigs: Vec<Signal> = (1..=32).map(|i| sig(i - 1, &format!("A{i}"), "uV")).collect();
        sigs.push(sig(32, "EXG1", "uV"));
        sigs.push(sig(33, "Status", "Boolean"));
        let set = LeadSet::identify(&sigs);
        assert_eq!(set.numbering, Numbering::BioSemi);
        assert_eq!(set.leads[0].electrode, None, "BioSemi A1 is not the left earlobe");
        assert_eq!(set.leads[0].kind, Kind::Eeg);
        assert_eq!(set.leads[33].kind, Kind::Trigger);

        let sigs: Vec<Signal> = (1..=64).map(|i| sig(i - 1, &format!("E{i}"), "uV")).collect();
        let set = LeadSet::identify(&sigs);
        assert_eq!(set.numbering, Numbering::Egi);
        assert_eq!((set.leads[0].kind, set.leads[0].electrode), (Kind::Eeg, None));

        // Two eye electrodes alone are AASM EOG, not EGI sensors.
        let sigs = vec![sig(0, "E1", "uV"), sig(1, "E2", "uV"), sig(2, "C3", "uV")];
        let set = LeadSet::identify(&sigs);
        assert_eq!(set.leads[0].kind, Kind::Eog);
    }

    #[test]
    fn summary_reports_reference_and_layout() {
        let names = ["FP1", "FP2", "F3", "F4", "C3", "C4", "P3", "P4", "O1", "O2", "F7", "F8", "T3", "T4", "T5", "T6", "FZ", "CZ", "PZ"];
        let mut sigs: Vec<Signal> = names.iter().enumerate().map(|(i, n)| sig(i, &format!("EEG {n}-REF"), "uV")).collect();
        sigs.push(sig(19, "EEG EKG1-REF", "uV"));
        let set = LeadSet::identify(&sigs);
        assert_eq!(set.summary.eeg, 19);
        assert_eq!(set.summary.ten_twenty, 19);
        assert_eq!(set.summary.reference.as_deref(), Some("REF"));
        assert!(set.summary.legacy_names);
        assert!(!set.summary.bipolar);

        let pairs = ["FP1-F7", "F7-T7", "T7-P7", "P7-O1", "FP2-F8", "F8-T8"];
        let sigs: Vec<Signal> = pairs.iter().enumerate().map(|(i, n)| sig(i, n, "uV")).collect();
        let set = LeadSet::identify(&sigs);
        assert!(set.summary.bipolar);
        assert_eq!(set.summary.reference, None);
    }

    #[test]
    fn units() {
        assert_eq!(parse_unit("uV").to_uv, Some(1.0));
        assert_eq!(parse_unit("\u{b5}V").display, "\u{b5}V");
        assert_eq!(parse_unit("mV").to_uv, Some(1000.0));
        assert_eq!(parse_unit("%").to_uv, None);
    }
}
