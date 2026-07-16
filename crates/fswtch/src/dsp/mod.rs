//! Pure-Rust DSP utilities — resampling, RMS, AGC, and AI denoising.
//!
//! This module was once the top-level DSP pipeline of the `audio-dsp` crate (HPF +
//! far-field gate + jitter buffer + sink chain). That pipeline was standalone-binary-era
//! plumbing and is now dead — the active AEC/NS run in FreeSWITCH's `preprocess`
//! dialplan app and the active VAD runs inline in the media bug. The dead
//! pipeline/jitter-buffer/far-field/sink-chain modules have been removed.
//!
//! What remains:
//! - [`params`] — shared constants (`PIPELINE_SAMPLE_RATE` 16 kHz,
//!   `TELEPHONY_SAMPLE_RATE` 8 kHz) + the `PcmFrame` alias.
//! - [`util`] — `SampleRateConverter` (rubato) + RMS helper, used by TTS
//!   resampling.
//! - [`agc`] — self-developed RMS-target AGC (an alternative to speex's
//!   `SPEEX_PREPROCESS_SET_AGC`, which collapses output on the AEC-Challenge set).
//! - [`denoise`] — nnnoiseless wrapper (feature-gated).
//!
//! These are pure-Rust algorithms with no FreeSWITCH FFI coupling; the C-layer
//! AEC/NS/VAD wrappers live in [`crate::speex`], [`crate::vad`], [`crate::resample`].

mod agc;
#[cfg(feature = "denoise")]
mod denoise;
mod params;
mod util;

pub use agc::Agc;
#[cfg(feature = "denoise")]
pub use denoise::DenoiseStage;
pub use params::{PIPELINE_SAMPLE_RATE, PcmFrame, TELEPHONY_SAMPLE_RATE};
pub use util::{SampleRateConverter, rms};
