//! Voice Activity Detection — showcases `fswtch::Vad` / `fswtch::VadState`.
//!
//! Creates a `Vad` for 8 kHz mono audio, feeds it a synthetic block of PCM silence (a vector
//! of zero `i16` samples) via [`Vad::process`], reads back the resulting `VadState`, and writes
//! the state's canonical name to the stream. Demonstrates the `VadState` mapping — named consts
//! (`NONE`, `START_TALKING`, `TALKING`, `STOP_TALKING`, `ERROR`), the `is_talking()` predicate,
//! the `as_u32()` raw value, and the `Display` impl that defers to FreeSWITCH's
//! `switch_vad_state2str`.
//!
//! Load `mod_vad_detect`; from fs_cli run `rust_vad_detect` to process one silent frame and
//! report the resulting state. Against a live FreeSWITCH, swap the zeroed buffer for real PCM
//! captured from a session (the `api_callback` receives `session: Option<Session>`).

fswtch::module_exports! {
    module = mod_vad_detect,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn vad_detect_api(_cmd, _session, stream) {
        fswtch::log_info("mod_vad_detect", "rust_vad_detect invoked");

        // Build a VAD for 8 kHz, mono. `Vad::new` allocates a `switch_vad_t`; the handle owns it
        // and destroys it on drop.
        let vad = match fswtch::Vad::new(8000, 1) {
            Ok(vad) => vad,
            Err(error) => {
                fswtch::log_error("mod_vad_detect", error);
                return stream.write(&format!("vad init failed: {error}\n"));
            }
        };

        // A synthetic frame of silence: 160 zeroed i16 samples (20 ms @ 8 kHz). `process` takes a
        // `&mut [i16]` because FreeSWITCH may mutate the buffer in place.
        let mut frame: Vec<i16> = vec![0i16; 160];
        let state = vad.process(&mut frame);

        // Showcase the VadState mapping: the named const comparison, the is_talking() predicate,
        // the raw u32 value, and the Display impl (which calls switch_vad_state2str).
        let label = match state {
            fswtch::VadState::NONE => "NONE",
            fswtch::VadState::START_TALKING => "START_TALKING",
            fswtch::VadState::TALKING => "TALKING",
            fswtch::VadState::STOP_TALKING => "STOP_TALKING",
            fswtch::VadState::ERROR => "ERROR",
            other => return stream.write(&format!("vad state=unknown({})\n", other.as_u32())),
        };

        stream.write(&format!(
            "vad state={} (display={}, raw={}, talking={})\n",
            label,
            state,
            state.as_u32(),
            state.is_talking(),
        ))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_detect" {
        fswtch::log_info("mod_vad_detect", "loading module");
        module.api(
            "rust_vad_detect",
            "runs one silent PCM frame through fswtch::Vad and reports the state",
            "rust_vad_detect",
            vad_detect_api,
        )
    }
}
