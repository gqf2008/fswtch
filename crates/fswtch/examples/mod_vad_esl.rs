//! mod_vad_esl — FS ASR (detect_speech) → ESL text event, + inbound ESL "speak" (text) → FS TTS.
//!
//! A thin ESL bridge: audio stays in FreeSWITCH's media path (ASR/TTS modules), events carry
//! only text — so there are no files, HTTP, or base64.
//!
//! # Outbound (ASR → text event)
//! At load the module subscribes to `DETECTED_SPEECH` events. FreeSWITCH fires these when its
//! ASR (started via the `detect_speech` app — mod_unimrcp / online ASR, NOT offline pocketsphinx)
//! recognizes speech. The recognized text is in the event **body**; this module re-fires it as a
//! custom `fswtch::asr_result` event with headers `Call-UUID` (from the session's `Unique-ID`),
//! `Speech-Type` (`detected-speech` / `detected-partial-speech` / `begin-speaking`) and the
//! transcript as the body.
//!
//! # Inbound (speak text → TTS → playback)
//! At load the module also subscribes to custom `fswtch::speak` events (any ESL socket sends
//! them via `sendevent CUSTOM` with `Event-Subclass: fswtch::speak`). The event carries
//! `Target-UUID` + `Speak-Args` (the full `speak` app arg, e.g. `engine|voice|text`). A worker
//! queues the `speak` app on the target session via [`fswtch::execute_application_async`] (FS
//! `sendmsg` `call-command: execute`), so it runs on the session's own thread — thread-safe from
//! the event worker (no need to locate the session first).
//!
//! # Use
//! ```text
//! load mod_vad_esl
//! # dialplan: start ASR (engine/grammar/name/dest per your ASR module):
//! <action application="rust_vad_esl" data="unimrcp grammar.json name /path/dest"/>
//! # or fs_cli against an existing call:
//! fs_cli -x 'rust_vad_esl_start <uuid> unimrcp grammar.json name /path/dest'
//! # external LLM brain: subscribe `event custom fswtch::asr_result`, then:
//! fs_cli -x "sendevent CUSTOM\nEvent-Subclass: fswtch::speak\nTarget-UUID: <uuid>\nSpeak-Args: cepstral|david|hello there"
//! ```
//! The fswtch VAD API (`Vad`, `speech_segments`, `snap_segments`) stays in the crate as a separate
//! feature (see `mod_vad_detect`); this module uses FS's own ASR endpointing via `detect_speech`.

use std::thread;

use fswtch::EventType;

const ASR_SUBCLASS: &str = "fswtch::asr_result";
const SPEAK_SUBCLASS: &str = "fswtch::speak";

fswtch::module_exports! {
    module = mod_vad_esl,
    load = switch_module_load,
}

// ── outbound: DETECTED_SPEECH → fswtch::asr_result ────────────────────────

fswtch::event_callback! {
    fn on_detected_speech(event) {
        let call_uuid = event.header("Unique-ID").unwrap_or_default();
        let speech_type = event.header("Speech-Type").unwrap_or_default();
        // FS puts the recognized result (XML) in the event body.
        let body = event.body_str().unwrap_or_default();
        if let Err(error) = fswtch::Event::custom(ASR_SUBCLASS)
            .and_then(|mut ev| {
                ev.add_header("Call-UUID", &call_uuid)?;
                ev.add_header("Speech-Type", &speech_type)?;
                ev.add_body(&body)?;
                ev.fire()
            }) {
            fswtch::log_error(
                "mod_vad_esl",
                format!("fire asr_result failed: {error}"),
            );
        }
    }
}

// ── inbound: fswtch::speak (text) → FS TTS (speak app) → playback ──────────

fswtch::event_callback! {
    fn on_speak(event) {
        let target = match event.header("Target-UUID") {
            Some(t) if !t.is_empty() => t,
            _ => {
                fswtch::log_error("mod_vad_esl", "speak event missing Target-UUID");
                return;
            }
        };
        let args = match event.header("Speak-Args") {
            Some(a) if !a.is_empty() => a,
            _ => {
                fswtch::log_error("mod_vad_esl", "speak event missing Speak-Args");
                return;
            }
        };
        if let Err(error) = thread::Builder::new()
            .name("fswtch-vad-esl-speak".to_owned())
            .spawn(move || run_speak(&target, &args))
        {
            fswtch::log_error(
                "mod_vad_esl",
                format!("failed to spawn speak worker: {error}"),
            );
        }
    }
}

fn run_speak(uuid: &str, speak_args: &str) {
    fswtch::log_info("mod_vad_esl", format!("speaking on {uuid}: {speak_args}"));
    // sendmsg-queue the speak app on the session's own thread (thread-safe from this worker).
    if let Err(error) = fswtch::execute_application_async(uuid, "speak", speak_args) {
        fswtch::log_error("mod_vad_esl", format!("speak sendmsg failed: {error}"));
    }
}

// ── module entry: start-ASR app/api + the two subscriptions ────────────────

fswtch::app_callback! {
    fn vad_esl_app(session, data) {
        let Some(session) = session else {
            return;
        };
        let args = data.unwrap_or_default();
        if let Err(error) = session.execute_application("detect_speech", &args) {
            fswtch::log_error("mod_vad_esl", format!("detect_speech failed: {error}"));
        }
    }
}

fswtch::api_callback! {
    fn vad_esl_start_api(cmd, _session, stream) {
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let cmd = cmd.unwrap_or_default();
        let (uuid, args) = match cmd.split_once(char::is_whitespace) {
            Some((u, a)) => (u.trim(), a.trim()),
            None => (cmd.trim(), ""),
        };
        if uuid.is_empty() {
            return stream.write("usage: rust_vad_esl_start <uuid> <detect_speech args>\n");
        }
        // sendmsg-queue detect_speech on the session's own thread (thread-safe from the API thread).
        match fswtch::execute_application_async(uuid, "detect_speech", args) {
            Ok(()) => stream.write(&format!("asr started on {uuid}\n")),
            Err(error) => stream.write(&format!("detect_speech failed: {error}\n")),
        }
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_esl" {
        fswtch::log_info("mod_vad_esl", "loading module");
        module
            .application(
                fswtch::ApplicationInfo::new(
                    "rust_vad_esl",
                    "Starts FS ASR (detect_speech); results forwarded as fswtch::asr_result",
                    "Rust ASR/TTS to ESL bridge",
                    "rust_vad_esl <detect_speech args>",
                ),
                vad_esl_app,
            )
            .and_then(|m| {
                m.api(
                    "rust_vad_esl_start",
                    "starts FS ASR on an existing call by uuid",
                    "rust_vad_esl_start <uuid> <detect_speech args>",
                    vad_esl_start_api,
                )
            })
            .and_then(|m| {
                // Outbound: forward FS ASR results as a custom ESL event.
                match fswtch::EventBinder::bind(
                    "mod_vad_esl.asr",
                    EventType::DETECTED_SPEECH,
                    None,
                    Some(on_detected_speech),
                    std::ptr::null_mut(),
                ) {
                    Ok(b) => std::mem::forget(b),
                    Err(e) => fswtch::log_error("mod_vad_esl", format!("asr bind failed: {e}")),
                }
                // Inbound: speak text → FS TTS.
                match fswtch::EventBinder::bind(
                    "mod_vad_esl.speak",
                    EventType::CUSTOM,
                    Some(SPEAK_SUBCLASS),
                    Some(on_speak),
                    std::ptr::null_mut(),
                ) {
                    Ok(b) => std::mem::forget(b),
                    Err(e) => fswtch::log_error("mod_vad_esl", format!("speak bind failed: {e}")),
                }
                Ok(m)
            })
    }
}
