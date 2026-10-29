//! End-to-end tests on generated EDF/BDF files: header parsing, sample decoding, EDF+
//! timekeeping, triggers, damaged files, montages that must not depend on channel order, and
//! band-power maps.

use std::path::PathBuf;
use std::sync::Arc;

use oink_eeg::dsp::{BREAK, FilterSettings};
use oink_eeg::edf::{AnnotationSource, EdfFile, Format};
use oink_eeg::leads::{LeadSet, TEN_TWENTY, electrode};
use oink_eeg::maps::{self, Measure};
use oink_eeg::montage::{self, MontageId};
use oink_eeg::pipeline::{self, TraceRequest};

#[derive(Clone)]
struct Sig {
    label: String,
    unit: &'static str,
    phys: (f64, f64),
    dig: (i32, i32),
    spr: usize,
}

fn sig(label: &str, unit: &'static str, spr: usize) -> Sig {
    Sig { label: label.into(), unit, phys: (-3276.8, 3276.7), dig: (-32768, 32767), spr }
}

fn ann(spr: usize) -> Sig {
    Sig { label: "EDF Annotations".into(), unit: "", phys: (-1.0, 1.0), dig: (-32768, 32767), spr }
}

struct Spec {
    bdf: bool,
    reserved: &'static str,
    records_field: String,
    duration: &'static str,
    sigs: Vec<Sig>,
    records: usize,
}

fn put(out: &mut Vec<u8>, s: &str, width: usize) {
    let mut b = s.as_bytes().to_vec();
    b.resize(width, b' ');
    out.extend_from_slice(&b[..width]);
}

/// Writes a file. `sample(signal, record, j)` gives digital values; `tal(record)` gives the
/// annotation text of each record.
fn write(name: &str, spec: &Spec, sample: impl Fn(usize, usize, usize) -> i32, tal: impl Fn(usize) -> Vec<u8>) -> PathBuf {
    let ns = spec.sigs.len();
    let mut h = Vec::new();
    if spec.bdf {
        h.push(0xFF);
        h.extend_from_slice(b"BIOSEMI");
    } else {
        put(&mut h, "0", 8);
    }
    put(&mut h, "X X X X", 80);
    put(&mut h, "Startdate 02-MAR-2002 X X X", 80);
    put(&mut h, "02.03.02", 8);
    put(&mut h, "14.30.00", 8);
    put(&mut h, &(256 * (ns + 1)).to_string(), 8);
    put(&mut h, spec.reserved, 44);
    put(&mut h, &spec.records_field, 8);
    put(&mut h, spec.duration, 8);
    put(&mut h, &ns.to_string(), 4);
    for s in &spec.sigs {
        put(&mut h, &s.label, 16);
    }
    for _ in &spec.sigs {
        put(&mut h, "AgAgCl electrode", 80);
    }
    for s in &spec.sigs {
        put(&mut h, s.unit, 8);
    }
    for s in &spec.sigs {
        put(&mut h, &s.phys.0.to_string(), 8);
    }
    for s in &spec.sigs {
        put(&mut h, &s.phys.1.to_string(), 8);
    }
    for s in &spec.sigs {
        put(&mut h, &s.dig.0.to_string(), 8);
    }
    for s in &spec.sigs {
        put(&mut h, &s.dig.1.to_string(), 8);
    }
    for _ in &spec.sigs {
        put(&mut h, "HP:0.1Hz LP:75Hz", 80);
    }
    for s in &spec.sigs {
        put(&mut h, &s.spr.to_string(), 8);
    }
    for _ in &spec.sigs {
        put(&mut h, "", 32);
    }
    let bytes = if spec.bdf { 3 } else { 2 };
    for r in 0..spec.records {
        for (i, s) in spec.sigs.iter().enumerate() {
            if s.label.ends_with("Annotations") {
                let mut t = tal(r);
                t.resize(s.spr * bytes, 0);
                h.extend_from_slice(&t);
            } else {
                for j in 0..s.spr {
                    let v = sample(i, r, j);
                    h.extend_from_slice(&v.to_le_bytes()[..bytes]);
                }
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("oink-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, h).unwrap();
    path
}

fn time_tal(onset: &str) -> Vec<u8> {
    format!("+{onset}\x14\x14\x00").into_bytes()
}

#[test]
fn edf_plus_header_rates_and_subsecond_start() {
    let spec = Spec {
        bdf: false,
        reserved: "EDF+C",
        records_field: "10".into(),
        duration: "1",
        sigs: vec![sig("EEG Fp1-REF", "uV", 256), sig("ECG", "mV", 128), ann(30)],
        records: 10,
    };
    let path = write(
        "plus.edf",
        &spec,
        |i, r, j| if i == 0 { ((r * 256 + j) % 1000) as i32 - 500 } else { 7 },
        |r| {
            let mut t = time_tal(&format!("{}", r as f64 + 0.5));
            if r == 3 {
                t.extend_from_slice(b"+3.25\x1512\x14Seizure onset\x14\x00");
            }
            if r == 4 {
                // One about a single signal (EDF+ "@@" convention), one naming no signal.
                t.extend_from_slice(b"+4.5\x152\x14Spike@@eeg fp1-ref\x14\x00+5\x14Note@@nowhere\x14\x00");
            }
            t
        },
    );
    let f = EdfFile::open(&path).unwrap();
    assert_eq!(f.format, Format::EdfPlus);
    assert!(!f.discontinuous);
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    assert_eq!(f.signals[0].fs, 256.0);
    assert_eq!(f.signals[1].fs, 128.0);
    assert!(f.signals[2].is_annotation);
    assert_eq!(f.first_record_offset, 0.5);
    assert_eq!(f.time_range(), (0.5, 10.5));
    assert_eq!(f.clock(0.5).time_string(true), "14:30:00.500");
    assert_eq!(f.start.date.unwrap().year, 2002);

    // Physical value of digital d is d * 0.1 here; sample j of record r sits at 0.5 + r + j/fs.
    let w = &f.read_window(&[0], 2.5, 2.5 + 4.0 / 256.0).unwrap()[0];
    assert_eq!(w.start, 640);
    let expect: Vec<f32> = (0..4).map(|j| ((2 * 256 + j) % 1000) as f32 - 500.0).map(|d| d * 0.1).collect();
    for (a, b) in w.data.iter().zip(&expect) {
        assert!((a - b).abs() < 1e-3, "{a} vs {b}");
    }
    // Before the first record there is no data.
    assert!(f.read_window(&[0], 0.0, 0.25).unwrap()[0].data.iter().all(|v| v.is_nan()));

    let a = f.read_annotations().unwrap();
    let got: Vec<_> = a.iter().map(|a| (a.onset, a.duration, a.text.as_str(), a.signal)).collect();
    assert_eq!(got, [(3.25, 12.0, "Seizure onset", None), (4.5, 2.0, "Spike", Some(0)), (5.0, 0.0, "Note@@nowhere", None)]);
}

#[test]
fn edf_plus_discontinuous_has_gaps() {
    let starts = [0.0, 1.0, 5.0, 6.0];
    let spec = Spec {
        bdf: false,
        reserved: "EDF+D",
        records_field: "4".into(),
        duration: "1",
        sigs: vec![sig("EEG Cz", "uV", 100), ann(20)],
        records: 4,
    };
    let path = write("discontinuous.edf", &spec, |_, r, _| r as i32 + 1, |r| time_tal(&starts[r].to_string()));
    let f = EdfFile::open(&path).unwrap();
    assert!(f.discontinuous);
    assert_eq!(f.time_range(), (0.0, 7.0));
    assert_eq!(f.gaps(), vec![(2.0, 5.0)]);
    let w = &f.read_window(&[0], 0.0, 7.0).unwrap()[0];
    assert!((w.data[150] - 0.2).abs() < 1e-4, "record 1");
    assert!(w.data[200..500].iter().all(|v| v.is_nan()), "gap");
    assert!((w.data[550] - 0.3).abs() < 1e-4, "record 2 starts at 5 s");
    assert_eq!(f.records_overlapping(2.5, 4.5), 2..2);
}

#[test]
fn bdf_24bit_values_and_status_triggers() {
    let mut eeg = sig("Fp1", "uV", 64);
    eeg.dig = (-8_388_608, 8_388_607);
    eeg.phys = (-262_144.0, 262_143.97);
    let mut status = sig("Status", "Boolean", 64);
    status.dig = (-8_388_608, 8_388_607);
    let spec = Spec { bdf: true, reserved: "24BIT", records_field: "3".into(), duration: "1", sigs: vec![eeg, status], records: 3 };
    let path = write(
        "triggers.bdf",
        &spec,
        |i, r, j| {
            if i == 0 {
                if j == 0 { -8_388_608 } else { (r * 64 + j) as i32 - 100 }
            } else {
                // Upper status bits (e.g. CMS in range) must not create events.
                let code = if r == 1 && (10..20).contains(&j) { 5 } else if r == 2 && j >= 60 { 12 } else { 0 };
                (1 << 20) | code
            }
        },
        |_| Vec::new(),
    );
    let f = EdfFile::open(&path).unwrap();
    assert_eq!(f.format, Format::Bdf);
    let w = &f.read_window(&[0], 0.0, 3.0).unwrap()[0];
    assert!((w.data[0] + 262_144.0).abs() < 0.01, "most negative 24-bit value: {}", w.data[0]);
    let s = &f.signals[0];
    assert!((w.data[1] as f64 - (-99.0 * s.gain + s.offset)).abs() < 1e-3);
    let events = f.read_trigger_events(1).unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].text, "Trigger 5");
    assert_eq!(events[0].source, AnnotationSource::Trigger);
    assert!((events[0].onset - (1.0 + 10.0 / 64.0)).abs() < 1e-9);
    assert!((events[0].duration - 10.0 / 64.0).abs() < 1e-9);
    assert_eq!(events[1].text, "Trigger 12");
    let leads = LeadSet::identify(&f.signals);
    assert_eq!(leads.leads[1].kind, oink_eeg::leads::Kind::Trigger);
}

#[test]
fn truncated_and_unfinished_files() {
    let spec = Spec {
        bdf: false,
        reserved: "",
        records_field: "10".into(),
        duration: "0.5",
        sigs: vec![sig("EEG O1", "mV", 50)],
        records: 7,
    };
    let path = write("truncated.edf", &spec, |_, _, _| 1, |_| Vec::new());
    // Append half a record of junk.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(&[0u8; 50]);
    std::fs::write(&path, bytes).unwrap();
    let f = EdfFile::open(&path).unwrap();
    assert_eq!(f.n_records, 7);
    assert_eq!(f.signals[0].fs, 100.0);
    assert_eq!(f.time_range(), (0.0, 3.5));
    assert!(f.warnings.iter().any(|w| w.contains("truncated")), "{:?}", f.warnings);

    let spec = Spec { records_field: "-1".into(), ..spec };
    let path = write("unfinished.edf", &spec, |_, _, _| 1, |_| Vec::new());
    let f = EdfFile::open(&path).unwrap();
    assert_eq!(f.n_records, 7);
    assert!(f.warnings.iter().any(|w| w.contains("number of data records")));

    // mV EEG is converted to µV for montages.
    let leads = LeadSet::identify(&f.signals);
    assert_eq!(leads.leads[0].display_scale(), 1000.0);
}

#[test]
fn not_an_edf_file() {
    let dir = std::env::temp_dir().join(format!("oink-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("junk.edf");
    std::fs::write(&path, b"hello").unwrap();
    assert!(EdfFile::open(&path).is_err());
    std::fs::write(&path, vec![b'x'; 600]).unwrap();
    assert!(EdfFile::open(&path).is_err());
}

const TUH: [&str; 19] = [
    "EEG FP1-REF", "EEG FP2-REF", "EEG F3-REF", "EEG F4-REF", "EEG C3-REF", "EEG C4-REF", "EEG P3-REF", "EEG P4-REF",
    "EEG O1-REF", "EEG O2-REF", "EEG F7-REF", "EEG F8-REF", "EEG T3-REF", "EEG T4-REF", "EEG T5-REF", "EEG T6-REF",
    "EEG FZ-REF", "EEG CZ-REF", "EEG PZ-REF",
];

/// A recording with the same 19 leads in a given order. Each electrode's signal depends only
/// on its name, never on its position in the file.
fn tuh_file(name: &str, order: &[usize]) -> PathBuf {
    let sigs: Vec<Sig> = order.iter().map(|&k| sig(TUH[k], "uV", 250)).chain([sig("EEG EKG1-REF", "uV", 250)]).collect();
    let spec = Spec { bdf: false, reserved: "", records_field: "8".into(), duration: "1", sigs, records: 8 };
    let order = order.to_vec();
    write(
        name,
        &spec,
        move |i, r, j| {
            let k = order.get(i).copied().unwrap_or(99) as f64;
            let t = (r * 250 + j) as f64 / 250.0;
            (300.0 * (t * (3.0 + k) + k).sin() + 40.0 * k) as i32
        },
        |_| Vec::new(),
    )
}

#[test]
fn montages_do_not_depend_on_channel_order() {
    let natural: Vec<usize> = (0..19).collect();
    let shuffled = [11, 4, 17, 0, 9, 14, 2, 18, 6, 13, 1, 16, 8, 3, 12, 5, 15, 10, 7];
    let traces = |path: PathBuf, id: MontageId| {
        let file = Arc::new(EdfFile::open(&path).unwrap());
        let leads = LeadSet::identify(&file.signals);
        let stats = pipeline::signal_stats(&file, &leads);
        let m = Arc::new(montage::build(id, &leads, &file.signals, &stats).unwrap());
        let visible = Arc::new(vec![true; m.channels.len()]);
        let req = TraceRequest {
            file,
            montage: m.clone(),
            visible,
            t0: 2.0,
            t1: 6.0,
            px_per_sec: 400.0,
            filters: FilterSettings { highpass: Some(1.0), lowpass: Some(40.0), notch: Some(50.0) },
        };
        let data = pipeline::compute(&req, &|| false).unwrap().unwrap();
        m.channels
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let r = &data.ranges[i];
                let ys: Vec<f32> = data.points[r.start as usize..r.end as usize].iter().filter(|p| p.ch != BREAK).map(|p| p.y).collect();
                (c.label.clone(), ys)
            })
            .collect::<Vec<_>>()
    };
    for id in [MontageId::AsRecorded, MontageId::Banana, MontageId::Transverse, MontageId::Average, MontageId::Tcp] {
        if id == MontageId::Tcp {
            continue; // needs A1/A2
        }
        let a = traces(tuh_file("natural.edf", &natural), id);
        let b = traces(tuh_file("shuffled.edf", &shuffled), id);
        let sort = |mut v: Vec<(String, Vec<f32>)>| {
            if id == MontageId::AsRecorded {
                v.sort_by(|x, y| x.0.cmp(&y.0));
            }
            v
        };
        let (a, b) = (sort(a), sort(b));
        assert_eq!(a.len(), b.len());
        for ((la, ya), (lb, yb)) in a.iter().zip(&b) {
            assert_eq!(la, lb, "{id:?}: channel order differs");
            assert_eq!(ya.len(), yb.len(), "{id:?} {la}");
            for (x, y) in ya.iter().zip(yb) {
                assert!((x - y).abs() < 1e-3, "{id:?} {la}: {x} vs {y}");
            }
        }
    }
}

#[test]
fn band_power_maps_find_posterior_alpha_and_frontal_delta() {
    use std::f64::consts::TAU;
    let sigs: Vec<Sig> = TUH.iter().map(|l| sig(l, "uV", 250)).collect();
    let spec = Spec { bdf: false, reserved: "", records_field: "12".into(), duration: "1", sigs, records: 12 };
    let path = write(
        "bands.edf",
        &spec,
        |i, r, j| {
            let t = (r * 250 + j) as f64 / 250.0;
            let alpha = if TUH[i].contains(" O") { 40.0 } else { 0.0 };
            let delta = if TUH[i].contains(" FP") { 60.0 } else { 0.0 };
            let uv = alpha * (TAU * 10.0 * t).sin()
                + delta * (TAU * 2.0 * t).sin()
                + 8.0 * (TAU * 6.0 * t + i as f64).sin()
                + 3.0 * (TAU * 20.0 * t + 2.0 * i as f64).sin();
            (uv * 10.0) as i32 // 0.1 µV per digital unit
        },
        |_| Vec::new(),
    );
    let file = EdfFile::open(&path).unwrap();
    let leads = LeadSet::identify(&file.signals);
    let stats = pipeline::signal_stats(&file, &leads);
    let avg = montage::build(MontageId::Average, &leads, &file.signals, &stats).unwrap();
    let s = maps::band_powers(&file, &avg, 1.0, 11.0, &|| false).unwrap().unwrap();
    assert_eq!((s.powers.len(), s.segments, s.skipped, s.sampled), (19, 9, 0, false));

    let value = |m: Measure, name: &str| {
        let e = electrode(name).unwrap().0;
        m.value(s.powers.iter().find(|p| p.electrode == e).unwrap()).unwrap()
    };
    let top_two = |m: Measure| {
        let mut v: Vec<(&str, f64)> = TEN_TWENTY.iter().map(|n| (*n, value(m, n))).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut names = [v[0].0, v[1].0];
        names.sort();
        names
    };
    assert_eq!(top_two(Measure::Alpha), ["O1", "O2"]);
    assert_eq!(top_two(Measure::Delta), ["Fp1", "Fp2"]);
    assert!(value(Measure::Alpha, "O1") > 80.0, "{}", value(Measure::Alpha, "O1"));
    assert!(value(Measure::AlphaDelta, "O2") > 10.0 && value(Measure::AlphaDelta, "Fp2") < 0.1);
}

/// The bundled sample's band powers against `scipy.signal.welch` on the same data, read with MNE:
/// average reference over the 19 scalp electrodes, 0-10 s, 2 s Hann segments overlapping by half.
#[test]
fn sample_band_powers_match_scipy() {
    // (electrode, alpha 8-13 Hz and delta 1-4 Hz as % of 1-45 Hz power)
    const SCIPY: [(&str, f64, f64); 19] = [
        ("Fp1", 63.737, 14.484),
        ("Fp2", 49.429, 24.492),
        ("F3", 60.753, 15.808),
        ("F4", 60.499, 18.819),
        ("F7", 18.372, 63.667),
        ("F8", 31.839, 44.726),
        ("T3", 37.385, 36.235),
        ("T4", 39.919, 27.935),
        ("C3", 26.310, 45.870),
        ("C4", 34.404, 26.412),
        ("T5", 43.173, 33.016),
        ("T6", 47.355, 26.928),
        ("P3", 61.860, 16.390),
        ("P4", 67.954, 12.104),
        ("O1", 62.675, 12.321),
        ("O2", 74.634, 8.680),
        ("Fz", 61.463, 18.199),
        ("Cz", 25.310, 31.991),
        ("Pz", 54.556, 21.938),
    ];
    let file = EdfFile::open(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/Subject00_1.edf")).unwrap();
    let leads = LeadSet::identify(&file.signals);
    let stats = pipeline::signal_stats(&file, &leads);
    let avg = montage::build(MontageId::Average, &leads, &file.signals, &stats).unwrap();
    let s = maps::band_powers(&file, &avg, 0.0, 10.0, &|| false).unwrap().unwrap();
    assert_eq!((s.powers.len(), s.segments), (19, 9));
    for (name, alpha, delta) in SCIPY {
        let e = electrode(name).unwrap().0;
        let p = s.powers.iter().find(|p| p.electrode == e).unwrap();
        let (a, d) = (Measure::Alpha.value(p).unwrap(), Measure::Delta.value(p).unwrap());
        assert!((a - alpha).abs() < 0.01 && (d - delta).abs() < 0.01, "{name}: alpha {a:.4} vs {alpha}, delta {d:.4} vs {delta}");
    }
}

#[test]
fn motor_sample_has_its_labelled_events() {
    let file = EdfFile::open(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/S001R04.edf")).unwrap();
    assert!(file.warnings.is_empty(), "{:?}", file.warnings);
    let leads = LeadSet::identify(&file.signals);
    assert_eq!((leads.summary.eeg, leads.summary.ten_twenty), (64, 19));
    assert!(leads.summary.unplaced.is_empty(), "{:?}", leads.summary.unplaced);

    let a = file.read_annotations().unwrap();
    let count = |code: &str| a.iter().filter(|e| e.text == code).count();
    assert_eq!((a.len(), count("T0"), count("T1"), count("T2")), (30, 15, 8, 7));
    assert!(a.iter().all(|e| (4.0..4.3).contains(&e.duration) && e.signal.is_none()));
    assert_eq!(a[0].onset, 0.0);
    for pair in a.windows(2) {
        assert!((pair[0].onset + pair[0].duration - pair[1].onset).abs() < 0.01, "back to back");
    }
    let stats = pipeline::signal_stats(&file, &leads);
    for (id, m) in montage::catalog(&leads, &file.signals, &stats) {
        assert!(m.is_ok(), "{id:?}: {:?}", m.err());
    }
}

#[test]
fn bundled_sample_is_complete_and_reads_the_same_from_memory() {
    static BYTES: &[u8] = include_bytes!("../samples/Subject00_1.edf");
    let disk = EdfFile::open(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/Subject00_1.edf")).unwrap();
    let mem = EdfFile::from_static("Subject00_1.edf", BYTES).unwrap();
    assert!(mem.in_memory() && !disk.in_memory());
    assert_eq!((mem.n_records, mem.signals.len()), (disk.n_records, disk.signals.len()));
    assert!(mem.warnings.is_empty(), "{:?}", mem.warnings);
    let all: Vec<usize> = (0..mem.signals.len()).filter(|&i| !mem.signals[i].is_annotation).collect();
    let (a, b) = (disk.read_window(&all, 30.0, 32.0).unwrap(), mem.read_window(&all, 30.0, 32.0).unwrap());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.data, y.data);
    }

    let leads = LeadSet::identify(&mem.signals);
    assert_eq!(leads.summary.ten_twenty, 19);
    assert!(leads.leads.iter().any(|l| l.kind == oink_eeg::leads::Kind::Ecg && l.display == "ECG"));
    let stats = pipeline::signal_stats(&mem, &leads);
    for (id, m) in montage::catalog(&leads, &mem.signals, &stats) {
        assert!(m.is_ok(), "{id:?}: {:?}", m.err());
    }
}
