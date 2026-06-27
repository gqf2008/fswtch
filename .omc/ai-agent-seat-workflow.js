// AI-Agent-Seat Implementation Workflow
// Implements complete AI agent seat module with ASR→LLM→TTS pipeline

export const meta = {
  name: 'ai-agent-seat-implementation',
  description: 'Implement complete AI-Agent-Seat module in ai-agent-seat crate with ASR→LLM→TTS pipeline',
  phases: [
    { title: 'Core infrastructure', detail: 'call_core, voice_core, audio_dsp modules' },
    { title: 'Module structure', detail: 'lib.rs, bug.rs, actor.rs' },
    { title: 'AI pipeline', detail: 'ASR, LLM, TTS actors' },
    { title: 'VAD and resample', detail: 'aec_vad submodule' },
    { title: 'Supporting modules', detail: 'config, runtime, boundary, event_sub, control' },
    { title: 'Verification', detail: 'Build, clippy, tests' },
  ],
}

const conventions = "You are implementing a module in the ai-agent-seat crate at /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/.\n\nCONVENTIONS:\n- Use Rust 2024 edition\n- All unsafe code must have # Safety doc and // SAFETY: comments\n- Use anyhow::Result for error handling\n- Use actix::prelude::* for actors\n- Use tracing for logging\n- Use dashmap for concurrent maps\n- Use serde for serialization\n- Follow the mod_voice_seat pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/\n\nDEPENDENCIES (already in Cargo.toml):\n- fswtch (FreeSWITCH FFI)\n- actix (actor framework)\n- tokio (async runtime)\n- async-trait (async trait)\n- anyhow (error handling)\n- tracing (logging)\n- dashmap (concurrent maps)\n- serde (serialization)\n- earshot (VAD)\n- rubato (audio resampling)\n- mimalloc (global allocator)\n\nREFERENCE: Read /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/ for the complete implementation pattern.\n\nOUTPUT: Write the files to /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/. Return the list of files created."

// Phase 1: Core infrastructure (parallel)
const phase1 = parallel([
  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/call_core.rs with:\n- CallControl trait (async_trait) with hangup, answer, send_dtmf, transfer, fire_transcript methods\n- CallActor trait (async_trait) with uuid, process_audio, write_tts_audio, handle_speech_turn, handle_barge_in methods\n- Messages: SpeechTurn, BargeIn, AnswerCall, HangupCall, SendDtmf, TransferCall (all actix Message)\n- CallRegistry struct with DashMap<String, Addr<dyn CallActor>>\n- AiSpeakingFlag struct with Arc<AtomicBool>\n- Global REGISTRY static\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/call_core/.\n\n" + conventions, {label: 'call_core'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/voice_core.rs with:\n- Config struct (serde Deserialize) with ai endpoints, keys, VAD params\n- Config::load(path: &str) -> Result<Self>\n- Config::default() for fallback\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/voice_core/.\n\n" + conventions, {label: 'voice_core'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/audio_dsp.rs with:\n- PIPELINE_SAMPLE_RATE constant (16000)\n- SampleRateConverter struct wrapping rubato::FastFixedIn<f32>\n- SampleRateConverter::new(from_rate, to_rate) -> Result<Self>\n- SampleRateConverter::process(&mut self, samples: &[i16]) -> Vec<i16>\n- SampleRateConverter::reset(&mut self)\n\nUse rubato crate for resampling.\n\n" + conventions, {label: 'audio_dsp'}),
])

// Phase 2: Module structure (depends on phase 1)
const phase2 = phase1.then(() => parallel([
  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/lib.rs with:\n- Module exports (module_exports!)\n- Module load (module_load!) with switch_module_load function\n- switch_module_shutdown function\n- voice_seat_app callback (attach media bug, spawn CallActor)\n- Use mimalloc as global allocator\n- Use tracing-subscriber for logging\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/lib.rs.\n\n" + conventions, {label: 'lib'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/bug.rs with:\n- VoiceSeatBug struct implementing MediaBugHandler trait\n- on_init, on_read_replace, on_write_replace, on_close methods\n- Pre-roll buffer (300ms @ 16kHz)\n- TTS accumulator (VecDeque<i16>)\n- Barge-in detection\n- Fade-out on barge-in (80ms cosine fade)\n- ai_speaking flag (Arc<AtomicBool>)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/bug.rs.\n\n" + conventions, {label: 'bug'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/actor.rs with:\n- spawn_call_actor function (launch CallActor on actix System)\n- CallActor struct (actix Actor)\n- Handler implementations for SpeechTurn, BargeIn, AnswerCall, HangupCall, SendDtmf, TransferCall\n- Wire speech_rx (mpsc::Receiver<SpeechSignal>) and tts_tx (mpsc::Sender<TtsSignal>)\n- Watchdog (optional max_call_secs)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/actor.rs.\n\n" + conventions, {label: 'actor'}),
]))

// Phase 3: AI pipeline (depends on phase 2, parallel)
const phase3 = phase2.then(() => parallel([
  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/asr.rs with:\n- AsrActor struct (actix Actor)\n- AsrActor::new(config: Config) -> Self\n- Handler for audio chunks (SpeechSignal::Turn)\n- Send recognized text to LLM actor via mpsc channel\n- Use earshot for VAD (already integrated in aec_vad)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/agent/asr/.\n\n" + conventions, {label: 'asr'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/llm.rs with:\n- LlmActor struct (actix Actor)\n- LlmActor::new(config: Config) -> Self\n- Handler for recognized text from ASR\n- Call external LLM API (e.g. OpenAI, Claude)\n- Send response to TTS actor via mpsc channel\n- Tool execution support (optional)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/agent/llm/.\n\n" + conventions, {label: 'llm'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/tts.rs with:\n- TtsActor struct (actix Actor)\n- TtsActor::new(config: Config) -> Self\n- Handler for text from LLM\n- Call external TTS API (e.g. ElevenLabs, Azure)\n- Send audio chunks to VoiceSeatBug via mpsc channel\n- Downsample from API rate to 16kHz if needed\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/agent/tts/.\n\n" + conventions, {label: 'tts'}),
]))

// Phase 4: VAD + resample (parallel with phase 3)
const phase4 = phase2.then(() => parallel([
  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/aec_vad/mod.rs with:\n- VadBug struct wrapping earshot VAD\n- VadBug::from_session(session, config) -> Result<Self>\n- process_capture(pcm: &mut [i16], ai_speaking: bool) method\n- process_render(pcm: &[i16]) method (for barge-in detection)\n- take_cleaned_16k() method (get upsampled audio)\n- SpeechEvent enum (Start, End, BargeIn)\n- SpeechSink type (Arc<dyn Fn(SpeechEvent) + Send + Sync>)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/aec_vad/bug.rs.\n\n" + conventions, {label: 'aec_vad_bug'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/aec_vad/vad.rs with:\n- VadConfig struct (speech_threshold, silence_timeout_ms, sample_rate, min_speech_rms)\n- BargeInConfig struct (confirm_ms)\n- SpeechEvent enum (Start, End { silence_ms }, BargeIn { voiced_ms })\n- SpeechSink type alias\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/aec_vad/vad.rs.\n\n" + conventions, {label: 'aec_vad_vad'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/aec_vad/resample.rs with:\n- FsResampler struct wrapping rubato::FastFixedIn<f32>\n- FsResampler::new(from_rate, to_rate, channels) -> Result<Self>\n- FsResampler::process(&mut self, samples: &[i16]) -> Vec<i16>\n- FsResampler::reset(&mut self)\n- get_codec_rate(session_ptr) helper function\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/aec_vad/resample.rs.\n\n" + conventions, {label: 'aec_vad_resample'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/aec_vad/events.rs with:\n- fire_transcript(uuid: &str, body: &str) function\n- Fire CUSTOM voice_seat::transcript event\n- Synchronous (block until event sent)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/aec_vad/events.rs.\n\n" + conventions, {label: 'aec_vad_events'}),
]))

// Phase 5: Supporting modules (parallel with phase 3)
const phase5 = phase2.then(() => parallel([
  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/config.rs with:\n- Config struct (serde Deserialize) with all config fields\n- load() function (load from YAML path)\n- get() function (get global config)\n- Default implementation\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/config.rs.\n\n" + conventions, {label: 'config'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/runtime.rs with:\n- start() function (start tokio runtime + actix System)\n- stop() function (stop runtime)\n- spawn() function (spawn async task on runtime)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/runtime.rs.\n\n" + conventions, {label: 'runtime'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/boundary.rs with:\n- catch_fs() function (wrap FS thread callbacks in catch_unwind)\n- Downgrade panic to drop this frame/call instead of aborting FS process\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/boundary.rs.\n\n" + conventions, {label: 'boundary'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/event_sub.rs with:\n- bind() function (bind to CUSTOM voice_seat::command events)\n- unbind() function\n- on_command callback (dispatch hangup/send_dtmf to CallActor)\n- Manually declare switch_event_bind/unbind (fswtch-sys does not have them)\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/event_sub.rs.\n\n" + conventions, {label: 'event_sub'}),

  () => agent("Create /Volumes/Workspace/GitHub/fswtch/ai-agent-seat/src/control.rs with:\n- FfiControl struct (stateless, locate session per-op)\n- CallControl trait implementation\n- hangup, answer, send_dtmf, transfer, fire_transcript methods\n- Use fswtch::Channel to locate session by UUID\n\nFollow the pattern from /Users/sqb/Workspace/projects/voice-call/freeswitch/mod_voice_seat/src/control.rs.\n\n" + conventions, {label: 'control'}),
]))

// Phase 6: Verification (depends on all phases)
const phase6 = parallel([phase3, phase4, phase5]).then(() =>
  agent("Verify the ai-agent-seat module builds and passes clippy:\n1. Run: cd /Volumes/Workspace/GitHub/fswtch && cargo build -p ai-agent-seat\n2. Run: cargo clippy -p ai-agent-seat -- -D warnings\n3. Report any errors or warnings\n\n" + conventions, {label: 'verify'}),
)

return phase6.then(() => 'All phases complete')
