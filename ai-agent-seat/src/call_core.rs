use actix::prelude::*;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// Call control trait for controlling an active call.
///
/// All methods are synchronous: the FFI-backed implementation (see
/// `crate::control::FfiControl`) issues FreeSWITCH C calls that are themselves
/// non-blocking, so there is no need for `async_trait`. This also keeps the trait
/// `dyn`-compatible should a caller want to store it as a trait object.
pub trait CallControl: Send + Sync {
    fn hangup(&self, uuid: &str) -> Result<()>;
    fn answer(&self, uuid: &str) -> Result<()>;
    fn send_dtmf(&self, uuid: &str, digits: &str) -> Result<()>;
    fn transfer(&self, uuid: &str, destination: &str) -> Result<()>;
    fn fire_transcript(&self, uuid: &str, body: &str) -> Result<()>;
}

/// Call actor trait for handling call state and media.
///
/// This trait is kept object-safe-friendly by using synchronous methods only
/// (no `async_trait`), so it does not require `dyn`-compatibility. Concrete
/// actors are stored as [`Addr<crate::actor::CallActorImpl>`] in the registry.
pub trait CallActor: Actor + Send {
    /// The UUID of the call this actor owns.
    fn uuid(&self) -> &str;
    /// Process incoming caller audio. Returns `true` to continue processing.
    fn process_audio(&mut self, audio: &[i16], sample_rate: u32) -> bool;
    /// Write synthesized TTS audio back toward the caller.
    fn write_tts_audio(&mut self, audio: &[i16], sample_rate: u32) -> Result<()>;
    /// Handle a completed speech turn (transcribed text).
    fn handle_speech_turn(&mut self, text: String) -> Result<()>;
    /// Handle caller barge-in (interruption of AI speech).
    fn handle_barge_in(&mut self) -> Result<()>;
}

/// Message sent when ASR produces a speech turn.
#[derive(Message)]
#[rtype(result = "()")]
pub struct SpeechTurn {
    /// Transcribed text of the speech turn (may be empty until ASR runs).
    pub text: String,
    /// Raw caller audio samples (16kHz i16 PCM) associated with this turn.
    pub audio: Vec<i16>,
}

/// Message sent when caller barges in.
#[derive(Message)]
#[rtype(result = "()")]
pub struct BargeIn;

/// Message to answer the call.
#[derive(Message)]
#[rtype(result = "()")]
pub struct AnswerCall;

/// Message to hangup the call.
#[derive(Message)]
#[rtype(result = "()")]
pub struct HangupCall {
    pub uuid: String,
    pub cause: Option<String>,
}

/// Message to send DTMF digits.
#[derive(Message)]
#[rtype(result = "()")]
pub struct SendDtmf {
    pub uuid: String,
    pub digits: String,
    pub duration_ms: Option<u32>,
}

/// Message to transfer the call.
#[derive(Message)]
#[rtype(result = "()")]
pub struct TransferCall {
    pub uuid: String,
    pub destination: String,
}

/// Registry for active call actors.
///
/// Stores concrete [`Addr<crate::actor::CallActorImpl>`] handles keyed by call UUID.
/// Using a concrete address type (rather than `Addr<dyn CallActor>`) avoids the
/// `dyn`-compatibility issues that arise from `async_trait` on the actor trait.
pub struct CallRegistry {
    actors: Mutex<HashMap<String, Addr<crate::actor::CallActorImpl>>>,
}

impl CallRegistry {
    pub fn new() -> Self {
        Self {
            actors: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, uuid: String, addr: Addr<crate::actor::CallActorImpl>) {
        let mut actors = self.actors.lock().unwrap();
        actors.insert(uuid, addr);
    }

    pub fn unregister(&self, uuid: &str) {
        let mut actors = self.actors.lock().unwrap();
        actors.remove(uuid);
    }

    pub fn get(&self, uuid: &str) -> Option<Addr<crate::actor::CallActorImpl>> {
        let actors = self.actors.lock().unwrap();
        actors.get(uuid).cloned()
    }

    pub fn contains(&self, uuid: &str) -> bool {
        let actors = self.actors.lock().unwrap();
        actors.contains_key(uuid)
    }

    /// Remove all registered call actors.
    pub fn clear(&self) {
        let mut actors = self.actors.lock().unwrap();
        actors.clear();
    }
}

impl Default for CallRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Global registry instance.
///
/// Uses [`std::sync::OnceLock`] (rather than `once_cell`) since `once_cell` is not
/// a dependency of this crate.
pub static REGISTRY: OnceLock<Arc<CallRegistry>> = OnceLock::new();

/// Returns the global [`CallRegistry`], initializing it on first access.
pub fn registry() -> &'static Arc<CallRegistry> {
    REGISTRY.get_or_init(|| Arc::new(CallRegistry::new()))
}

/// AI speaking flag shared between bug and tool executor.
pub struct AiSpeakingFlag {
    flag: Arc<AtomicBool>,
}

impl AiSpeakingFlag {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_true(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn set_false(&self) {
        self.flag.store(false, Ordering::Relaxed);
    }

    pub fn is_speaking(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    pub fn clone_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

impl Default for AiSpeakingFlag {
    fn default() -> Self {
        Self::new()
    }
}
