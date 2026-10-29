//! Core of the viewer: file format, lead identification, montages and signal processing.
//! Nothing here depends on the UI.

pub mod dsp;
pub mod edf;
pub mod leads;
pub mod maps;
pub mod montage;
pub mod overview;
pub mod pipeline;
