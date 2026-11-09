//! Sincra: automatic multicam audio synchronisation for FCP7 XML timelines.

pub mod dom;
pub mod dsp;
pub mod engine;
pub mod media;
pub mod project;
pub mod settings;
pub mod xmeml;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP: &str = "Sincra";

/// Timeline timecode (HH:MM:SS:FF) for a position in seconds.
pub fn tc(seconds: Option<f64>, fps: f64) -> String {
    let Some(seconds) = seconds else { return "-".into() };
    let neg = seconds < 0.0;
    let f = (seconds.abs() * fps).round_ties_even() as i64;
    let tb = fps.round_ties_even().max(1.0) as i64;
    let s = format!("{:02}:{:02}:{:02}:{:02}", f / (3600 * tb), f / (60 * tb) % 60, f / tb % 60, f % tb);
    if neg {
        format!("-{s}")
    } else {
        s
    }
}
