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

/// Signal from the media bug (VAD) to the call actor's AI pipeline.
///
/// Replaces the former separate `SpeechTurn` and `BargeIn` actix messages.
/// `Turn` carries the raw 16 kHz i16 PCM of one detected speech segment; the
/// [`crate::actor::CallActorImpl`] forwards it to the
/// [`crate::orchestrator::Orchestrator::process_speech_segment`]. `BargeIn`
/// signals caller interruption while the AI is speaking; the actor forwards it
/// to [`crate::orchestrator::Orchestrator::cancel_current`].
#[derive(Message)]
#[rtype(result = "()")]
pub enum SpeechSignal {
    /// A completed speech segment (caller finished a turn). `audio` is 16 kHz
    /// mono i16 PCM (possibly with VAD pre-roll prepended).
    Turn { audio: Vec<i16> },
    /// Caller barged in while the AI was speaking — interrupt current TTS.
    BargeIn,
}

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
/// Using a concrete address type (rather than `Addr<dyn ...>`) avoids any
/// `dyn`-compatibility concerns.
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

/// AI speaking flag shared between the media bug and the orchestrator.
///
/// Set while TTS is playing toward the caller so the bug's write-leg VAD can
/// detect barge-in. Cloned cheaply (Arc-inner).
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
