//! VoiceSeatBug - Media bug handler for AI agent seat.
//!
//! This module implements the [`fswtch::MediaBugHandler`] trait to intercept audio frames
//! from FreeSWITCH and process them through VAD, then dispatches completed speech turns
//! to the [`CallActor`](crate::call_core::CallActor) for ASR processing.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use fswtch::{
    MediaBugAction, MediaBugContext, MediaBugFlags, MediaBugHandler, MediaFrameMut, Session,
};

use crate::audio_dsp::{PIPELINE_SAMPLE_RATE, SampleRateConverter, get_codec_rate};
use crate::call_core::{SpeechTurn, registry};
use crate::voice_core::Config;

/// earshot predicts on fixed 256-sample (16 ms at 16 kHz) frames.
const EARSHOT_FRAME_SIZE: usize = 256;

/// Score above which an earshot frame is considered speech.
const SPEECH_SCORE_THRESHOLD: f32 = 0.5;

/// Media bug handler for AI agent seat.
///
/// Intercepts audio frames from FreeSWITCH and processes them through VAD
/// and sends speech turns to the CallActor for ASR processing.
pub struct VoiceSeatBug {
    uuid: String,
    config: Option<Config>,
    /// VAD detector for speech detection.
    vad: Option<earshot::Detector>,
    /// Sample rate converter for upsampling to pipeline rate (16kHz).
    resampler: Option<SampleRateConverter>,
    /// Pre-roll buffer for capturing audio before speech detection.
    pre_roll: VecDeque<i16>,
    /// Whether we're currently in a speech segment.
    in_speech: bool,
    /// Accumulated speech audio for current segment.
    speech_buffer: Vec<i16>,
    /// Staging buffer for accumulating samples into earshot-sized frames.
    vad_stage: Vec<i16>,
    /// AI speaking flag (shared with CallActor).
    ai_speaking: Arc<AtomicBool>,
    /// Silence timeout counter (samples).
    silence_samples: u32,
    /// Current silence sample count.
    current_silence: u32,
    /// Barge-in confirmation time (samples).
    barge_in_confirm_samples: u32,
    /// Current barge-in confirmation counter.
    barge_in_confirm: u32,
}

impl VoiceSeatBug {
    /// Create a new VoiceSeatBug from a FreeSWITCH session.
    pub fn from_session(session: Session, config: Option<Config>) -> Result<Self> {
        let uuid = session.channel().and_then(|c| c.uuid()).unwrap_or_default();

        // Get codec sample rate from session
        let codec_rate = unsafe { get_codec_rate(session.as_ptr()) };

        // Create resampler if codec rate != pipeline rate
        let resampler = if codec_rate != PIPELINE_SAMPLE_RATE {
            Some(SampleRateConverter::new(codec_rate, PIPELINE_SAMPLE_RATE)?)
        } else {
            None
        };

        // Get VAD config or use defaults
        let vad_config = config.as_ref().map(|c| c.vad.clone()).unwrap_or_default();

        // Create VAD detector. earshot works at 16 kHz; the pipeline already guarantees that rate.
        let vad = if PIPELINE_SAMPLE_RATE == 16000 {
            Some(earshot::Detector::default())
        } else {
            None
        };

        // Pre-roll buffer size: 300ms at pipeline rate
        let pre_roll_size = (PIPELINE_SAMPLE_RATE * 300 / 1000) as usize;

        // Get AI speaking flag from registry or create new
        let ai_speaking = Arc::new(AtomicBool::new(false));

        // Silence timeout in samples
        let silence_samples = vad_config.silence_timeout_ms * PIPELINE_SAMPLE_RATE / 1000;

        // Barge-in confirmation time in samples
        let barge_in_confirm_samples = vad_config.barge_in_confirm_ms * PIPELINE_SAMPLE_RATE / 1000;

        Ok(Self {
            uuid,
            config,
            vad,
            resampler,
            pre_roll: VecDeque::with_capacity(pre_roll_size),
            in_speech: false,
            speech_buffer: Vec::new(),
            vad_stage: Vec::with_capacity(EARSHOT_FRAME_SIZE),
            ai_speaking,
            silence_samples,
            current_silence: 0,
            barge_in_confirm_samples,
            barge_in_confirm: 0,
        })
    }

    /// Returns the flags the bug should be attached with: tap both read and write streams so we
    /// can run VAD on the caller's audio and detect barge-in while the AI speaks.
    pub fn bug_flags() -> MediaBugFlags {
        MediaBugFlags::READ_REPLACE | MediaBugFlags::WRITE_REPLACE
    }

    /// Feed upsampled 16 kHz samples through the earshot detector and call `frame_cb` for each
    /// completed 256-sample frame with its speech score.
    ///
    /// `frame_cb` receives `(score, frame)` where `score` is the earshot prediction in
    /// `[0.0, 1.0]` (or `-1.0` when the detector is unavailable / the frame size mismatched).
    fn process_vad_frames<S>(&mut self, samples_16k: &[i16], mut frame_cb: S)
    where
        S: FnMut(&mut Self, f32, &[i16]),
    {
        self.vad_stage.extend_from_slice(samples_16k);

        while self.vad_stage.len() >= EARSHOT_FRAME_SIZE {
            let frame: &[i16] = &self.vad_stage[..EARSHOT_FRAME_SIZE];
            // earshot::Detector::predict_i16 requires exactly 256 samples. Borrow the detector
            // just for this call and release it before invoking the callback so the callback
            // can borrow `self` mutably.
            let score = match self.vad.as_mut() {
                Some(detector) => detector.predict_i16(frame),
                None => -1.0,
            };
            // Copy the frame out so we can drop the shared borrow on `vad_stage` before the
            // callback mutates `self` (e.g. drains the staging buffer).
            let frame_owned: Vec<i16> = frame.to_vec();
            self.vad_stage.drain(..EARSHOT_FRAME_SIZE);
            frame_cb(self, score, &frame_owned);
        }
    }

    /// Returns the stored configuration, if any.
    pub fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    /// Replace the AI-speaking flag. Used to share the flag with the CallActor.
    pub fn set_ai_speaking_flag(&mut self, flag: Arc<AtomicBool>) {
        self.ai_speaking = flag;
    }

    /// A clone of the AI-speaking flag for the CallActor to observe.
    pub fn ai_speaking_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ai_speaking)
    }
}

impl MediaBugHandler for VoiceSeatBug {
    fn on_init(&mut self, _ctx: &mut MediaBugContext<'_>) -> MediaBugAction {
        tracing::info!("VoiceSeatBug initialized for session {}", self.uuid);
        MediaBugAction::Continue
    }

    fn on_read_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        // Get audio samples from frame (read leg = caller's audio).
        let Some(samples) = frame.as_frame().pcm_i16() else {
            return MediaBugAction::Continue;
        };
        if samples.is_empty() {
            return MediaBugAction::Continue;
        }

        // Upsample to pipeline rate if needed
        let samples_16k: Vec<i16> = if let Some(resampler) = &mut self.resampler {
            resampler.process(samples)
        } else {
            samples.to_vec()
        };

        // Process through VAD, frame by frame.
        self.process_vad_frames(&samples_16k, |this, score, frame| {
            let is_speech = score >= SPEECH_SCORE_THRESHOLD;

            if is_speech {
                if !this.in_speech {
                    // Speech started - flush pre-roll buffer
                    this.in_speech = true;
                    this.speech_buffer.clear();
                    this.speech_buffer.extend(this.pre_roll.iter());
                    this.current_silence = 0;

                    tracing::debug!("Speech started for session {}", this.uuid);
                }

                // Accumulate speech audio
                this.speech_buffer.extend_from_slice(frame);
                this.barge_in_confirm = 0;
            } else if this.in_speech {
                this.current_silence += frame.len() as u32;

                if this.current_silence >= this.silence_samples {
                    // Speech ended - send speech turn to CallActor
                    this.in_speech = false;
                    let speech = std::mem::take(&mut this.speech_buffer);

                    if !speech.is_empty() {
                        // Send speech turn (with accumulated audio) to CallActor for ASR.
                        if let Some(addr) = registry().get(&this.uuid) {
                            addr.do_send(SpeechTurn {
                                text: String::new(),
                                audio: speech,
                            });
                        }

                        this.pre_roll.clear();
                        tracing::debug!("Speech ended for session {}", this.uuid);
                    }
                } else {
                    // Tail of speech still accumulating while silence grows.
                    this.speech_buffer.extend_from_slice(frame);
                }
            } else {
                // Update pre-roll buffer
                this.pre_roll.extend(frame);
                while this.pre_roll.len() > this.pre_roll.capacity() {
                    this.pre_roll.pop_front();
                }
            }
        });

        MediaBugAction::Continue
    }

    fn on_write_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        // Check if AI is currently speaking
        let ai_speaking = self.ai_speaking.load(Ordering::Relaxed);

        if ai_speaking
            && let Some(samples) = frame.as_frame().pcm_i16()
            && !samples.is_empty()
        {
            // AI is speaking - check for barge-in (caller speaking while AI speaks).
            // Score the write leg too; if energy/speech is detected, confirm barge-in.
            let samples_16k: Vec<i16> = if let Some(resampler) = &mut self.resampler {
                resampler.process(samples)
            } else {
                samples.to_vec()
            };

            self.process_vad_frames(&samples_16k, |this, score, _frame| {
                if score >= SPEECH_SCORE_THRESHOLD {
                    this.barge_in_confirm += _frame.len() as u32;

                    if this.barge_in_confirm >= this.barge_in_confirm_samples {
                        // Barge-in confirmed - interrupt AI
                        tracing::info!("Barge-in detected for session {}", this.uuid);

                        if let Some(addr) = registry().get(&this.uuid) {
                            addr.do_send(crate::call_core::BargeIn);
                        }

                        // Reset AI speaking flag
                        this.ai_speaking.store(false, Ordering::Relaxed);
                        this.barge_in_confirm = 0;
                    }
                }
            });
        }

        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        tracing::info!("VoiceSeatBug closed for session {}", self.uuid);

        // Unregister from registry
        registry().unregister(&self.uuid);
    }
}
