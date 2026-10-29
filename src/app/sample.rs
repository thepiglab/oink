//! The recording built into the app, so there is something to look at straight away.
//! See samples/README.md for its source, license and checksum.

pub const NAME: &str = "Subject00_1.edf";
pub const BYTES: &[u8] = include_bytes!("../../samples/Subject00_1.edf");
pub const DESCRIPTION: &str = "3 minutes of eyes-closed resting EEG: 19 channels (10-20), ECG, 500 Hz";
pub const CREDIT: &str = "Zyma et al. (2019), EEG During Mental Arithmetic Tasks, PhysioNet";
pub const LICENSE: &str = "Open Data Commons Attribution License v1.0";
pub const DOI: &str = "https://doi.org/10.3390/data4010014";
