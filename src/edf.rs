//! EDF, EDF+, BDF and BDF+ reader.
//!
//! Only the header is kept in memory. Samples are fetched on demand with positioned reads,
//! so memory use scales with what is on screen instead of with the size of the file.
//!
//! Every per-signal property (label, unit, scaling, sampling rate) comes from that signal's
//! own header entry. Nothing assumes a fixed channel order.

use std::fmt;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Edf,
    EdfPlus,
    Bdf,
    BdfPlus,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Edf => "EDF",
            Format::EdfPlus => "EDF+",
            Format::Bdf => "BDF",
            Format::BdfPlus => "BDF+",
        }
    }

    /// Bytes per sample: 16-bit for EDF, 24-bit for BDF.
    pub fn sample_bytes(self) -> usize {
        match self {
            Format::Edf | Format::EdfPlus => 2,
            Format::Bdf | Format::BdfPlus => 3,
        }
    }

    pub fn is_plus(self) -> bool {
        matches!(self, Format::EdfPlus | Format::BdfPlus)
    }

    fn digital_limits(self) -> (i32, i32) {
        match self {
            Format::Edf | Format::EdfPlus => (-32768, 32767),
            Format::Bdf | Format::BdfPlus => (-8_388_608, 8_388_607),
        }
    }
}

#[derive(Debug)]
pub enum EdfError {
    Io(io::Error),
    Invalid(String),
}

impl fmt::Display for EdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdfError::Io(e) => write!(f, "{e}"),
            EdfError::Invalid(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for EdfError {}

impl From<io::Error> for EdfError {
    fn from(e: io::Error) -> Self {
        EdfError::Io(e)
    }
}

fn invalid(msg: impl Into<String>) -> EdfError {
    EdfError::Invalid(msg.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Date {
    fn days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    fn from_days(days: i64) -> Self {
        let (year, month, day) = civil_from_days(days);
        Date { year, month, day }
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Start of the recording as written in the header (whole seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    pub date: Option<Date>,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Wall-clock time of a point in the recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    pub date: Option<Date>,
    pub hour: u32,
    pub minute: u32,
    pub second: f64,
}

impl Clock {
    /// `HH:MM:SS`, or `HH:MM:SS.mmm` when `millis` is set.
    pub fn time_string(&self, millis: bool) -> String {
        if millis {
            let ms = (self.second * 1000.0).round() as u64;
            let (s, ms) = (ms / 1000, ms % 1000);
            format!("{:02}:{:02}:{:02}.{:03}", self.hour, self.minute, s.min(59), ms)
        } else {
            format!("{:02}:{:02}:{:02}", self.hour, self.minute, (self.second.floor() as u32).min(59))
        }
    }
}

/// EDF+ "local patient identification" subfields. `None` where the file says `X` (unknown).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patient {
    pub code: Option<String>,
    pub sex: Option<String>,
    pub birthdate: Option<String>,
    pub name: Option<String>,
    pub additional: Option<String>,
}

/// EDF+ "local recording identification" subfields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    pub startdate: Option<Date>,
    pub admin_code: Option<String>,
    pub technician: Option<String>,
    pub equipment: Option<String>,
    pub additional: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Signal {
    pub index: usize,
    pub label: String,
    pub transducer: String,
    /// Physical dimension exactly as written (e.g. `uV`).
    pub unit: String,
    pub phys_min: f64,
    pub phys_max: f64,
    pub dig_min: i32,
    pub dig_max: i32,
    pub prefilter: String,
    pub samples_per_record: usize,
    /// Sampling rate in Hz (samples per record / record duration).
    pub fs: f64,
    /// physical = digital * gain + offset
    pub gain: f64,
    pub offset: f64,
    /// An "EDF Annotations" / "BDF Annotations" signal: text, not samples.
    pub is_annotation: bool,
    /// Byte offset of this signal inside each data record.
    byte_offset: usize,
}

impl Signal {
    pub fn byte_len(&self, sample_bytes: usize) -> usize {
        self.samples_per_record * sample_bytes
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    /// Seconds after the header start time.
    pub onset: f64,
    /// Seconds; 0 when the annotation has no duration.
    pub duration: f64,
    pub text: String,
    pub source: AnnotationSource,
    /// The signal it is about, from the EDF+ convention "text@@signal label" (the label is
    /// then removed from `text`). `None` for the whole recording.
    pub signal: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationSource {
    /// EDF+/BDF+ annotation signal.
    Annotation,
    /// Decoded from a trigger/status channel.
    Trigger,
}

/// Samples of one signal on that signal's sample grid. Sample `k` is at `k / fs` seconds after
/// the header start time. Positions no data record covers (gaps, before the start, past the
/// end) hold NaN.
#[derive(Clone, Debug)]
pub struct Window {
    pub signal: usize,
    pub start: i64,
    pub data: Vec<f32>,
}

/// Where a recording's bytes come from: a file on disk, or a recording built into the program.
enum Source {
    File(File),
    Memory(&'static [u8]),
}

impl Source {
    fn len(&self) -> io::Result<u64> {
        match self {
            Source::File(f) => Ok(f.metadata()?.len()),
            Source::Memory(m) => Ok(m.len() as u64),
        }
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        match self {
            Source::File(f) => read_exact_at(f, offset, buf),
            Source::Memory(m) => {
                let eof = || io::Error::from(io::ErrorKind::UnexpectedEof);
                let start = usize::try_from(offset).map_err(|_| eof())?;
                buf.copy_from_slice(m.get(start..start.saturating_add(buf.len())).ok_or_else(eof)?);
                Ok(())
            }
        }
    }
}

pub struct EdfFile {
    /// Path on disk; for a built-in recording, just its name.
    pub path: PathBuf,
    source: Source,
    pub file_size: u64,
    pub format: Format,
    /// EDF+D / BDF+D: data records are not contiguous in time.
    pub discontinuous: bool,
    pub version: String,
    pub patient_raw: String,
    pub recording_raw: String,
    pub patient: Option<Patient>,
    pub recording: Option<Recording>,
    pub start: Start,
    pub header_bytes: u64,
    pub record_bytes: usize,
    pub n_records: usize,
    pub record_duration: f64,
    pub signals: Vec<Signal>,
    /// Start of record 0 relative to the header start time. EDF+ stores sub-second starts here.
    pub first_record_offset: f64,
    /// Start time of every record, only for discontinuous files.
    record_starts: Option<Vec<f64>>,
    /// Annotations collected while scanning a discontinuous file at open time.
    preloaded_annotations: Option<Vec<Annotation>>,
    /// Problems found in the header. The file is still readable.
    pub warnings: Vec<String>,
}

const MAX_SIGNALS: usize = 4096;
/// Upper bound on a single read when scanning blocks of records.
const BLOCK_BYTES: usize = 4 << 20;

impl EdfFile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EdfError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        Self::read(path, Source::File(file))
    }

    /// A recording built into the program, such as the bundled sample.
    pub fn from_static(name: &str, bytes: &'static [u8]) -> Result<Self, EdfError> {
        Self::read(PathBuf::from(name), Source::Memory(bytes))
    }

    /// True for a recording built into the program rather than read from disk.
    pub fn in_memory(&self) -> bool {
        matches!(self.source, Source::Memory(_))
    }

    fn read(path: PathBuf, source: Source) -> Result<Self, EdfError> {
        let file_size = source.len()?;
        if file_size < 256 {
            return Err(invalid("The file is smaller than an EDF header (256 bytes)."));
        }
        let mut fixed = [0u8; 256];
        source.read_exact_at(0, &mut fixed)?;
        let mut warnings = Vec::new();

        let bdf = fixed[0] == 0xFF;
        let version = if bdf { "BIOSEMI".to_string() } else { text(&fixed[0..8]) };
        if bdf && &fixed[1..8] != b"BIOSEMI" {
            warnings.push("BDF identification code is not \"BIOSEMI\".".into());
        }
        if !bdf && version != "0" {
            warnings.push(format!("Unexpected version field {version:?} (EDF requires \"0\")."));
        }

        let patient_raw = text(&fixed[8..88]);
        let recording_raw = text(&fixed[88..168]);
        let date_field = text(&fixed[168..176]);
        let time_field = text(&fixed[176..184]);
        let header_bytes_field = number(&fixed[184..192]);
        let reserved = text(&fixed[192..236]);
        let n_records_field = number(&fixed[236..244]);
        let record_duration = number(&fixed[244..252])
            .ok_or_else(|| invalid("The data record duration in the header is not a number."))?;
        let ns = number(&fixed[252..256])
            .filter(|n| *n >= 1.0 && *n <= MAX_SIGNALS as f64)
            .ok_or_else(|| invalid("The header does not give a valid number of signals."))?
            as usize;

        let reserved_upper = reserved.to_ascii_uppercase();
        let format = match (bdf, reserved_upper.starts_with("EDF+") || reserved_upper.starts_with("BDF+")) {
            (false, false) => Format::Edf,
            (false, true) => Format::EdfPlus,
            (true, false) => Format::Bdf,
            (true, true) => Format::BdfPlus,
        };
        let discontinuous = format.is_plus() && reserved_upper.as_bytes().get(4) == Some(&b'D');

        let header_bytes = 256 * (ns as u64 + 1);
        if header_bytes_field.map(|v| v as u64) != Some(header_bytes) {
            warnings.push(format!(
                "Header size field says {} bytes; {} signals need {header_bytes}. Using {header_bytes}.",
                header_bytes_field.map_or("nothing".to_string(), |v| format!("{v}")),
                ns
            ));
        }
        if file_size < header_bytes {
            return Err(invalid(format!(
                "The header describes {ns} signals but the file ends inside the header."
            )));
        }

        let mut block = vec![0u8; ns * 256];
        source.read_exact_at(256, &mut block)?;
        let field = |start: usize, width: usize, i: usize| -> &[u8] {
            let base = start * ns + i * width;
            &block[base..base + width]
        };

        let sample_bytes = format.sample_bytes();
        let (dig_lo, dig_hi) = format.digital_limits();
        let mut signals = Vec::with_capacity(ns);
        let mut byte_offset = 0usize;
        for i in 0..ns {
            // Signal headers are stored field by field: all labels, then all transducers, ...
            let label = text(field(0, 16, i));
            let transducer = text(field(16, 80, i));
            let unit = text(field(96, 8, i));
            let phys_min = number(field(104, 8, i));
            let phys_max = number(field(112, 8, i));
            let dig_min = number(field(120, 8, i));
            let dig_max = number(field(128, 8, i));
            let prefilter = text(field(136, 80, i));
            let spr = number(field(216, 8, i));

            let name = if label.is_empty() { format!("signal {}", i + 1) } else { format!("{label:?}") };
            let spr = match spr {
                Some(v) if v >= 0.0 => v.round() as usize,
                _ => {
                    return Err(invalid(format!("Signal {name} has an invalid number of samples per record.")));
                }
            };
            let is_annotation = {
                let l = label.to_ascii_lowercase();
                l == "edf annotations" || l == "bdf annotations"
            };
            if is_annotation && !format.is_plus() {
                warnings.push(format!(
                    "{name} looks like an EDF+ annotation signal, but the header does not say EDF+; reading it as one."
                ));
            }

            let phys_min = phys_min.unwrap_or_else(|| {
                warnings.push(format!("Signal {name}: physical minimum is not a number."));
                0.0
            });
            let phys_max = phys_max.unwrap_or_else(|| {
                warnings.push(format!("Signal {name}: physical maximum is not a number."));
                0.0
            });
            let dig_min = dig_min.map(|v| v.round() as i32).unwrap_or(dig_lo);
            let dig_max = dig_max.map(|v| v.round() as i32).unwrap_or(dig_hi);

            let (gain, offset) = if is_annotation {
                (1.0, 0.0)
            } else if dig_max <= dig_min {
                warnings.push(format!(
                    "Signal {name}: digital maximum ({dig_max}) is not above the minimum ({dig_min}); showing raw values."
                ));
                (1.0, 0.0)
            } else if phys_max == phys_min {
                warnings.push(format!("Signal {name}: physical range is empty; showing raw values."));
                (1.0, 0.0)
            } else {
                // phys_max < phys_min is allowed and means inverted polarity; the negative gain
                // undoes it.
                let gain = (phys_max - phys_min) / (dig_max - dig_min) as f64;
                (gain, phys_min - gain * dig_min as f64)
            };
            if !is_annotation && (dig_min < dig_lo || dig_max > dig_hi) {
                warnings.push(format!(
                    "Signal {name}: digital range {dig_min}..{dig_max} exceeds what {} samples can hold.",
                    if sample_bytes == 2 { "16-bit" } else { "24-bit" }
                ));
            }

            signals.push(Signal {
                index: i,
                label,
                transducer,
                unit,
                phys_min,
                phys_max,
                dig_min,
                dig_max,
                prefilter,
                samples_per_record: spr,
                fs: 0.0,
                gain,
                offset,
                is_annotation,
                byte_offset,
            });
            byte_offset += spr * sample_bytes;
        }
        let record_bytes = byte_offset;
        if record_bytes == 0 {
            return Err(invalid("Every signal has zero samples per record; there is no data."));
        }

        let has_samples = signals.iter().any(|s| !s.is_annotation && s.samples_per_record > 0);
        let record_duration = if record_duration > 0.0 && record_duration.is_finite() {
            record_duration
        } else if !has_samples {
            0.0
        } else {
            return Err(invalid(format!(
                "The data record duration is {record_duration} s; it must be positive."
            )));
        };
        for s in &mut signals {
            if !s.is_annotation && record_duration > 0.0 {
                s.fs = s.samples_per_record as f64 / record_duration;
            }
            if !s.is_annotation && s.samples_per_record == 0 {
                warnings.push(format!("Signal {:?} has no samples.", s.label));
            }
        }

        let data_bytes = file_size - header_bytes;
        let available = (data_bytes / record_bytes as u64) as usize;
        let leftover = data_bytes % record_bytes as u64;
        let n_records = match n_records_field.map(|v| v.round() as i64) {
            Some(n) if n >= 0 => {
                let n = n as usize;
                if n > available {
                    warnings.push(format!(
                        "The file is truncated: the header lists {n} data records but only {available} are complete."
                    ));
                    available
                } else {
                    if available > n {
                        warnings.push(format!(
                            "The file has {} complete data records past the {n} the header lists; they are ignored.",
                            available - n
                        ));
                    } else if leftover > 0 {
                        warnings.push(format!("{leftover} stray bytes at the end of the file are ignored."));
                    }
                    n
                }
            }
            _ => {
                warnings.push(format!(
                    "The header does not give the number of data records (recording not finalised?); found {available} in the file."
                ));
                available
            }
        };

        let mut recording = None;
        let mut patient = None;
        if format.is_plus() {
            patient = Some(parse_patient(&patient_raw));
            recording = parse_recording(&recording_raw);
            if recording.is_none() {
                warnings.push("EDF+ recording field does not start with \"Startdate\".".into());
            }
            if !signals.iter().any(|s| s.is_annotation) {
                warnings.push("EDF+ file without an annotation signal.".into());
            }
        }
        let start = parse_start(&date_field, &time_field, recording.as_ref().and_then(|r| r.startdate), &mut warnings);

        let mut edf = EdfFile {
            path,
            source,
            file_size,
            format,
            discontinuous,
            version,
            patient_raw,
            recording_raw,
            patient,
            recording,
            start,
            header_bytes,
            record_bytes,
            n_records,
            record_duration,
            signals,
            first_record_offset: 0.0,
            record_starts: None,
            preloaded_annotations: None,
            warnings,
        };

        if edf.n_records > 0 && edf.signals.iter().any(|s| s.is_annotation) {
            if edf.discontinuous {
                // Record times are only known from each record's time-keeping annotation,
                // so a discontinuous file has to be scanned before it can be drawn.
                let (starts, annotations) = edf.scan_annotations()?;
                let mut fixed = 0usize;
                let mut starts: Vec<f64> = starts
                    .into_iter()
                    .enumerate()
                    .map(|(r, s)| {
                        s.unwrap_or_else(|| {
                            fixed += 1;
                            r as f64 * edf.record_duration
                        })
                    })
                    .collect();
                if fixed > 0 {
                    edf.warnings.push(format!(
                        "{fixed} data records have no time-keeping annotation; assumed contiguous."
                    ));
                }
                if starts.windows(2).any(|w| w[1] < w[0]) {
                    edf.warnings.push("Data records are not in time order; sorted for display.".into());
                    starts.sort_by(f64::total_cmp);
                }
                let gaps = starts.windows(2).filter(|w| w[1] - w[0] > edf.record_duration + 1e-6).count();
                if gaps == 0 {
                    edf.warnings.push("Marked discontinuous (EDF+D) but has no gaps.".into());
                }
                edf.first_record_offset = starts.first().copied().unwrap_or(0.0);
                edf.record_starts = Some(starts);
                edf.preloaded_annotations = Some(annotations);
            } else {
                let mut buf = Vec::new();
                let first = edf.annotation_bytes(0, &mut buf).ok().and_then(|_| {
                    let mut sink = Vec::new();
                    parse_tals(&buf, true, &mut sink)
                });
                match first {
                    Some(t) if t.is_finite() => edf.first_record_offset = t,
                    _ if edf.format.is_plus() => edf.warnings.push(
                        "The first data record has no time-keeping annotation; assuming it starts at the header time."
                            .into(),
                    ),
                    _ => {}
                }
            }
        }

        Ok(edf)
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn sample_bytes(&self) -> usize {
        self.format.sample_bytes()
    }

    /// Start of data record `r`, in seconds after the header start time.
    pub fn record_start(&self, r: usize) -> f64 {
        match &self.record_starts {
            Some(starts) => starts[r],
            None => self.first_record_offset + r as f64 * self.record_duration,
        }
    }

    /// Time span covered by data records, in seconds after the header start time.
    pub fn time_range(&self) -> (f64, f64) {
        if self.n_records == 0 || self.record_duration <= 0.0 {
            return (0.0, 0.0);
        }
        (self.record_start(0), self.record_start(self.n_records - 1) + self.record_duration)
    }

    /// Gaps between data records (discontinuous files only).
    pub fn gaps(&self) -> Vec<(f64, f64)> {
        let Some(starts) = &self.record_starts else { return Vec::new() };
        starts
            .windows(2)
            .filter_map(|w| {
                let end = w[0] + self.record_duration;
                (w[1] > end + 1e-6).then_some((end, w[1]))
            })
            .collect()
    }

    /// Wall-clock time `t` seconds after the header start time.
    pub fn clock(&self, t: f64) -> Clock {
        let total = self.start.hour as f64 * 3600.0 + self.start.minute as f64 * 60.0 + self.start.second as f64 + t;
        let days = (total / 86400.0).floor();
        let rem = total - days * 86400.0;
        let hour = (rem / 3600.0).floor() as u32;
        let minute = ((rem - hour as f64 * 3600.0) / 60.0).floor() as u32;
        let second = rem - hour as f64 * 3600.0 - minute as f64 * 60.0;
        Clock {
            date: self.start.date.map(|d| Date::from_days(d.days() + days as i64)),
            hour: hour.min(23),
            minute: minute.min(59),
            second,
        }
    }

    /// Data records that overlap `[t0, t1)`.
    pub fn records_overlapping(&self, t0: f64, t1: f64) -> Range<usize> {
        if self.n_records == 0 || self.record_duration <= 0.0 || t1 <= t0 {
            return 0..0;
        }
        let dur = self.record_duration;
        match &self.record_starts {
            Some(starts) => {
                let a = starts.partition_point(|&s| s + dur <= t0);
                let b = starts.partition_point(|&s| s < t1);
                a..b.max(a)
            }
            None => {
                let off = self.first_record_offset;
                let a = ((t0 - off) / dur).floor().max(0.0) as usize;
                let b = ((t1 - off) / dur).ceil().max(0.0) as usize;
                a.min(self.n_records)..b.min(self.n_records)
            }
        }
    }

    /// Index on signal `s`'s sample grid of the first sample in record `r`.
    fn record_sample_index(&self, r: usize, s: &Signal) -> i64 {
        match &self.record_starts {
            Some(starts) => (starts[r] * s.fs).round() as i64,
            None => (self.first_record_offset * s.fs).round() as i64 + (r * s.samples_per_record) as i64,
        }
    }

    /// Reads `signals` over `[t0, t1)` seconds, in physical units.
    pub fn read_window(&self, signals: &[usize], t0: f64, t1: f64) -> io::Result<Vec<Window>> {
        let mut windows: Vec<Window> = signals
            .iter()
            .map(|&s| {
                let fs = self.signals[s].fs;
                let k0 = (t0 * fs).floor() as i64;
                let k1 = (t1 * fs).ceil() as i64;
                Window { signal: s, start: k0, data: vec![f32::NAN; (k1 - k0).max(0) as usize] }
            })
            .collect();
        let records = self.records_overlapping(t0, t1);
        if records.is_empty() || signals.is_empty() {
            return Ok(windows);
        }
        let sample_bytes = self.sample_bytes();
        self.scan_records(records, signals, |r, parts| {
            for (w, bytes) in windows.iter_mut().zip(parts) {
                let sig = &self.signals[w.signal];
                let first = self.record_sample_index(r, sig) - w.start;
                decode(sig, sample_bytes, bytes, first, &mut w.data);
            }
            true
        })?;
        Ok(windows)
    }

    /// Calls `f(record, bytes_of_each_signal)` for every record in `records`. Reads whole
    /// blocks of records when the requested signals make up much of a record, and only the
    /// requested byte ranges otherwise (e.g. 4 signals out of 256).
    fn scan_records(
        &self,
        records: Range<usize>,
        signals: &[usize],
        mut f: impl FnMut(usize, &[&[u8]]) -> bool,
    ) -> io::Result<()> {
        let sb = self.sample_bytes();
        let ranges: Vec<Range<usize>> = signals
            .iter()
            .map(|&s| {
                let sig = &self.signals[s];
                sig.byte_offset..sig.byte_offset + sig.byte_len(sb)
            })
            .collect();
        let wanted: usize = ranges.iter().map(|r| r.len()).sum();
        let rb = self.record_bytes;

        if rb <= 16 * 1024 || wanted * 4 >= rb {
            let per_block = (BLOCK_BYTES / rb).max(1);
            let mut buf = Vec::new();
            let mut r = records.start;
            while r < records.end {
                let n = per_block.min(records.end - r);
                buf.resize(n * rb, 0);
                self.source.read_exact_at(self.header_bytes + (r * rb) as u64, &mut buf)?;
                let mut parts: Vec<&[u8]> = Vec::with_capacity(ranges.len());
                for i in 0..n {
                    let rec = &buf[i * rb..(i + 1) * rb];
                    parts.clear();
                    parts.extend(ranges.iter().map(|range| &rec[range.clone()]));
                    if !f(r + i, &parts) {
                        return Ok(());
                    }
                }
                r += n;
            }
        } else {
            let mut bufs: Vec<Vec<u8>> = ranges.iter().map(|range| vec![0u8; range.len()]).collect();
            for r in records {
                let base = self.header_bytes + (r * rb) as u64;
                for (range, buf) in ranges.iter().zip(bufs.iter_mut()) {
                    self.source.read_exact_at(base + range.start as u64, buf)?;
                }
                let parts: Vec<&[u8]> = bufs.iter().map(|b| b.as_slice()).collect();
                if !f(r, &parts) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Bytes of every annotation signal in record `r`, concatenated.
    fn annotation_bytes(&self, r: usize, out: &mut Vec<u8>) -> io::Result<()> {
        out.clear();
        let sb = self.sample_bytes();
        let base = self.header_bytes + (r * self.record_bytes) as u64;
        for s in self.signals.iter().filter(|s| s.is_annotation) {
            let start = out.len();
            out.resize(start + s.byte_len(sb), 0);
            self.source.read_exact_at(base + s.byte_offset as u64, &mut out[start..])?;
        }
        Ok(())
    }

    /// Reads every annotation in the file (EDF+/BDF+), sorted by onset.
    pub fn read_annotations(&self) -> io::Result<Vec<Annotation>> {
        if let Some(a) = &self.preloaded_annotations {
            return Ok(a.clone());
        }
        Ok(self.scan_annotations()?.1)
    }

    /// Walks the annotation signals of every record. Returns each record's start time (from
    /// its time-keeping annotation, if present) and the text annotations.
    fn scan_annotations(&self) -> io::Result<(Vec<Option<f64>>, Vec<Annotation>)> {
        let ann: Vec<usize> = self.signals.iter().filter(|s| s.is_annotation).map(|s| s.index).collect();
        let mut starts = Vec::with_capacity(self.n_records);
        let mut annotations = Vec::new();
        if ann.is_empty() {
            return Ok((starts, annotations));
        }
        self.scan_records(0..self.n_records, &ann, |_, parts| {
            let mut start = None;
            for (i, bytes) in parts.iter().enumerate() {
                let t = parse_tals(bytes, i == 0, &mut annotations);
                if i == 0 {
                    start = t;
                }
            }
            starts.push(start);
            true
        })?;
        annotations.sort_by(|a, b| a.onset.total_cmp(&b.onset));
        // "text@@label" ties an annotation to one signal (EDF+ standard texts; MNE writes it too).
        for a in &mut annotations {
            let Some(at) = a.text.rfind("@@") else { continue };
            let (text, label) = (a.text[..at].trim_end(), a.text[at + 2..].trim());
            let signal = self.signals.iter().find(|s| !s.is_annotation && s.label.trim().eq_ignore_ascii_case(label));
            if let (Some(s), false) = (signal, text.is_empty()) {
                a.signal = Some(s.index);
                a.text = text.to_string();
            }
        }
        Ok((starts, annotations))
    }

    /// Turns a trigger/status channel into events: one per run of non-zero codes. For BDF
    /// "Status" channels only the lower 16 bits carry trigger codes; the upper bits are
    /// amplifier status flags.
    pub fn read_trigger_events(&self, s: usize) -> io::Result<Vec<Annotation>> {
        const MAX_EVENTS: usize = 200_000;
        let sig = &self.signals[s];
        let mask: i64 = if self.sample_bytes() == 3 { 0xFFFF } else { 0xFFFF_FFFF };
        let sb = self.sample_bytes();
        let fs = sig.fs;
        let mut events = Vec::new();
        let mut current: Option<(f64, i64)> = None;
        let mut last_t = 0.0;
        if fs <= 0.0 {
            return Ok(events);
        }
        self.scan_records(0..self.n_records, &[s], |r, parts| {
            let t0 = self.record_start(r);
            let bytes = parts[0];
            for j in 0..sig.samples_per_record {
                let raw = read_sample(bytes, j, sb) as i64;
                let code = if sb == 3 { raw & mask } else { raw };
                let t = t0 + j as f64 / fs;
                last_t = t;
                match current {
                    Some((_, c)) if c == code => {}
                    _ => {
                        if let Some((onset, c)) = current.take() {
                            events.push(Annotation {
                                onset,
                                duration: t - onset,
                                text: format!("Trigger {c}"),
                                source: AnnotationSource::Trigger,
                                signal: None,
                            });
                        }
                        if code != 0 {
                            current = Some((t, code));
                        }
                    }
                }
            }
            events.len() < MAX_EVENTS
        })?;
        if let Some((onset, c)) = current {
            events.push(Annotation {
                onset,
                duration: last_t - onset,
                text: format!("Trigger {c}"),
                source: AnnotationSource::Trigger,
                signal: None,
            });
        }
        Ok(events)
    }
}

/// Decodes one record's samples of one signal into `dst`, starting at `dst[first]`.
fn decode(sig: &Signal, sample_bytes: usize, bytes: &[u8], first: i64, dst: &mut [f32]) {
    let n = sig.samples_per_record as i64;
    let lo = (-first).clamp(0, n);
    let hi = (dst.len() as i64 - first).clamp(0, n);
    if lo >= hi {
        return;
    }
    let gain = sig.gain as f32;
    let offset = sig.offset as f32;
    let out = &mut dst[(first + lo) as usize..(first + hi) as usize];
    let (lo, hi) = (lo as usize, hi as usize);
    if sample_bytes == 2 {
        for (o, c) in out.iter_mut().zip(bytes[lo * 2..hi * 2].chunks_exact(2)) {
            *o = i16::from_le_bytes([c[0], c[1]]) as f32 * gain + offset;
        }
    } else {
        for (o, c) in out.iter_mut().zip(bytes[lo * 3..hi * 3].chunks_exact(3)) {
            let v = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
            *o = v as f32 * gain + offset;
        }
    }
}

fn read_sample(bytes: &[u8], j: usize, sample_bytes: usize) -> i32 {
    if sample_bytes == 2 {
        i16::from_le_bytes([bytes[2 * j], bytes[2 * j + 1]]) as i32
    } else {
        i32::from_le_bytes([0, bytes[3 * j], bytes[3 * j + 1], bytes[3 * j + 2]]) >> 8
    }
}

/// Parses the Time-stamped Annotation Lists in one annotation signal's bytes.
///
/// A TAL is `+onset[\x15duration]\x14text\x14...\x14\x00`. The first TAL of the first
/// annotation signal in each record is the time-keeping TAL: its (empty) first text marks when
/// the record starts; that onset is returned.
pub fn parse_tals(bytes: &[u8], first_in_record: bool, out: &mut Vec<Annotation>) -> Option<f64> {
    let mut record_start = None;
    let mut first = first_in_record;
    for tal in bytes.split(|&b| b == 0).filter(|t| !t.is_empty()) {
        let mut parts = tal.split(|&b| b == 0x14);
        let Some(head) = parts.next() else { continue };
        let (onset, duration) = match head.iter().position(|&b| b == 0x15) {
            Some(i) => (&head[..i], Some(&head[i + 1..])),
            None => (head, None),
        };
        let Some(onset) = tal_number(onset) else { continue };
        let duration = duration.and_then(tal_number).filter(|d| *d >= 0.0).unwrap_or(0.0);
        let mut texts = parts.peekable();
        if first {
            first = false;
            record_start = Some(onset);
            if texts.peek().is_some_and(|t| t.is_empty()) {
                texts.next();
            }
        }
        for t in texts {
            let text = String::from_utf8_lossy(t).trim().to_string();
            if !text.is_empty() {
                out.push(Annotation { onset, duration, text, source: AnnotationSource::Annotation, signal: None });
            }
        }
    }
    record_start
}

fn tal_number(b: &[u8]) -> Option<f64> {
    std::str::from_utf8(b).ok()?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            match file.seek_read(&mut buf[done..], offset + done as u64) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Header text field: ASCII per the spec, but tolerate UTF-8 and Latin-1 from sloppy writers.
fn text(b: &[u8]) -> String {
    let s = match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    };
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// Header number field. Accepts a decimal comma, which some European writers produce.
fn number(b: &[u8]) -> Option<f64> {
    let s = text(b).replace(',', ".");
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_patient(raw: &str) -> Patient {
    let mut it = raw.split_whitespace();
    let mut next = || it.next().filter(|v| *v != "X").map(|v| v.replace('_', " "));
    let code = next();
    let sex = next();
    let birthdate = next();
    let name = next();
    let rest: Vec<String> = it.map(|v| v.replace('_', " ")).collect();
    Patient { code, sex, birthdate, name, additional: (!rest.is_empty()).then(|| rest.join(" ")) }
}

fn parse_recording(raw: &str) -> Option<Recording> {
    let mut it = raw.split_whitespace();
    if !it.next()?.eq_ignore_ascii_case("startdate") {
        return None;
    }
    let startdate = it.next().and_then(parse_edfplus_date);
    let mut next = || it.next().filter(|v| *v != "X").map(|v| v.replace('_', " "));
    let admin_code = next();
    let technician = next();
    let equipment = next();
    let rest: Vec<String> = it.map(|v| v.replace('_', " ")).collect();
    Some(Recording { startdate, admin_code, technician, equipment, additional: (!rest.is_empty()).then(|| rest.join(" ")) })
}

/// `02-MAR-2002`
fn parse_edfplus_date(s: &str) -> Option<Date> {
    let mut it = s.split('-');
    let day: u32 = it.next()?.parse().ok()?;
    let month = match it.next()?.to_ascii_uppercase().as_str() {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => return None,
    };
    let year: i32 = it.next()?.parse().ok()?;
    valid_date(year, month, day)
}

fn valid_date(year: i32, month: u32, day: u32) -> Option<Date> {
    let date = Date { year, month, day };
    ((1..=12).contains(&month) && day >= 1 && Date::from_days(date.days()) == date).then_some(date)
}

fn three_numbers(s: &str) -> Option<[u32; 3]> {
    let parts: Vec<u32> = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    (parts.len() == 3).then(|| [parts[0], parts[1], parts[2]])
}

/// Header `dd.mm.yy` + `hh.mm.ss`. Two-digit years 85-99 are 19xx and 00-84 are 20xx; an EDF+
/// recording field with a four-digit year takes precedence.
fn parse_start(date: &str, time: &str, edfplus_date: Option<Date>, warnings: &mut Vec<String>) -> Start {
    let header_date = three_numbers(date).and_then(|[d, m, y]| {
        let year = if y >= 100 { y as i32 } else if y >= 85 { 1900 + y as i32 } else { 2000 + y as i32 };
        valid_date(year, m, d)
    });
    if header_date.is_none() {
        warnings.push(format!("Start date {date:?} is not a valid dd.mm.yy date."));
    }
    let date = match (edfplus_date, header_date) {
        (Some(e), Some(h)) if (e.month, e.day) == (h.month, h.day) => Some(e),
        (Some(e), Some(h)) => {
            warnings.push(format!("Header start date {h} disagrees with the EDF+ recording field ({e})."));
            Some(h)
        }
        (e, h) => h.or(e),
    };
    let (hour, minute, second) = match three_numbers(time) {
        Some([h, m, s]) if h < 24 && m < 60 && s < 61 => (h, m, s.min(59)),
        _ => {
            warnings.push(format!("Start time {time:?} is not a valid hh.mm.ss time."));
            (0, 0, 0)
        }
    };
    Start { date, hour, minute, second }
}

/// Days since 1970-01-01 (proleptic Gregorian). From Howard Hinnant's date algorithms.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

/// A signal header for unit tests elsewhere in the crate: `spr` samples per one-second record.
#[cfg(test)]
pub(crate) fn test_signal(index: usize, label: &str, unit: &str, spr: usize) -> Signal {
    Signal {
        index,
        label: label.into(),
        transducer: String::new(),
        unit: unit.into(),
        phys_min: -3200.0,
        phys_max: 3200.0,
        dig_min: -32768,
        dig_max: 32767,
        prefilter: String::new(),
        samples_per_record: spr,
        fs: spr as f64,
        gain: 6400.0 / 65535.0,
        offset: 0.0,
        is_annotation: false,
        byte_offset: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tal_parsing() {
        let bytes = b"+0\x14\x14\x00+12.5\x1530\x14Lights off\x14Second\x14\x00\x00\x00";
        let mut out = Vec::new();
        assert_eq!(parse_tals(bytes, true, &mut out), Some(0.0));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].onset, 12.5);
        assert_eq!(out[0].duration, 30.0);
        assert_eq!(out[0].text, "Lights off");
        assert_eq!(out[1].text, "Second");

        // Sub-second start and an annotation sharing the time-keeping TAL.
        let mut out = Vec::new();
        let bytes = b"+3600.25\x14\x14Recording starts\x14\x00";
        assert_eq!(parse_tals(bytes, true, &mut out), Some(3600.25));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].onset, 3600.25);

        // Negative onset, UTF-8 text.
        let mut out = Vec::new();
        parse_tals("-0.5\x14\x14\x00+1\x14Ångström µV\x14\x00".as_bytes(), true, &mut out);
        assert_eq!(out[0].text, "Ångström µV");
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(parse_edfplus_date("02-MAR-2002"), Some(Date { year: 2002, month: 3, day: 2 }));
        assert_eq!(parse_edfplus_date("31-FEB-2002"), None);
        let mut w = Vec::new();
        let s = parse_start("15.09.05", "10.18.42", None, &mut w);
        assert_eq!(s.date, Some(Date { year: 2005, month: 9, day: 15 }));
        assert_eq!((s.hour, s.minute, s.second), (10, 18, 42));
        let s = parse_start("01.01.85", "00:00:00", Some(Date { year: 2091, month: 1, day: 1 }), &mut w);
        assert_eq!(s.date.unwrap().year, 2091);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn header_numbers() {
        assert_eq!(number(b"-3200   "), Some(-3200.0));
        assert_eq!(number(b"3199,902"), Some(3199.902));
        assert_eq!(number(b"        "), None);
        assert_eq!(text(b"EEG Fp1\0\0\0"), "EEG Fp1");
        assert_eq!(text(&[b'u', 0xB5, b'V']), "u\u{b5}V");
    }

    #[test]
    fn edfplus_subfields() {
        let p = parse_patient("MCH-0234567 F 02-MAY-1951 Haagse_Harry");
        assert_eq!(p.name.as_deref(), Some("Haagse Harry"));
        assert_eq!(p.sex.as_deref(), Some("F"));
        let p = parse_patient("X X X X");
        assert_eq!(p, Patient::default());
        let r = parse_recording("Startdate 15-SEP-2005 2 Kesteren Nihon_Kohden_EEG-1100C_V01.00").unwrap();
        assert_eq!(r.equipment.as_deref(), Some("Nihon Kohden EEG-1100C V01.00"));
        assert_eq!(r.startdate.unwrap().year, 2005);
    }
}
