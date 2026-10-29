//! Recordings built into the app, so there is something to look at straight away.
//! See samples/README.md for their sources, licenses and checksums.

use oink_eeg::montage::MontageId;

pub struct Sample {
    /// Name on the command line: `oink --sample motor`.
    pub key: &'static str,
    /// File name, which also identifies the sample once it is open.
    pub name: &'static str,
    pub title: &'static str,
    pub bytes: &'static [u8],
    pub description: &'static str,
    /// What its event codes mean, where the file itself does not say: (code, meaning).
    pub labels: &'static [(&'static str, &'static str)],
    /// How to show it first, where the defaults do not suit it.
    pub montage: Option<MontageId>,
    /// µV per millimetre.
    pub sensitivity: Option<f32>,
    pub credit: &'static str,
    pub license: &'static str,
    pub link: &'static str,
}

pub static SAMPLES: [Sample; 2] = [
    Sample {
        key: "resting",
        name: "Subject00_1.edf",
        title: "Resting EEG",
        bytes: include_bytes!("../../samples/Subject00_1.edf"),
        description: "3 minutes of eyes-closed rest: 19 channels (10-20), ECG, 500 Hz",
        labels: &[],
        montage: None,
        sensitivity: None,
        credit: "Zyma et al. (2019), EEG During Mental Arithmetic Tasks, PhysioNet",
        license: "Open Data Commons Attribution License v1.0",
        link: "https://doi.org/10.3390/data4010014",
    },
    Sample {
        key: "motor",
        name: "S001R04.edf",
        title: "Motor imagery, with labelled events",
        bytes: include_bytes!("../../samples/S001R04.edf"),
        description: "2 minutes of imagined fist movement: 64 channels (10-10), 160 Hz",
        labels: &[("T0", "rest"), ("T1", "imagined left fist"), ("T2", "imagined right fist")],
        // 64 channels of large, slow activity: 18 bipolar lanes at a gentler sensitivity.
        montage: Some(MontageId::Banana),
        sensitivity: Some(30.0),
        credit: "Schalk et al. (2004), EEG Motor Movement/Imagery Dataset, PhysioNet",
        license: "Open Data Commons Attribution License v1.0",
        link: "https://doi.org/10.13026/C28G6P",
    },
];

/// The built-in sample with this file name.
pub fn named(name: &str) -> Option<&'static Sample> {
    SAMPLES.iter().find(|s| s.name == name)
}
