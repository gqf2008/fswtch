//! FreeSWITCH `mod_aec3` — WebRTC AEC3 echo cancellation as a loadable module.
//!
//! Two surfaces:
//! - `rust_aec3_smoke` API: runs the vendored `EchoCanceller3` on a synthetic echo inside the
//!   FreeSWITCH process and reports the achieved ERLE. Proves the module loads and the AEC3 C++
//!   links + runs in-process (the Docker smoke checks `"aec3 ok"`).
//! - `rust_aec3` dialplan application: attaches a media bug (`WRITE_STREAM` = far-end render,
//!   `READ_REPLACE` = near-end mic capture) that feeds 10 ms frames to `EchoCanceller3` and
//!   writes the de-echoed capture back. 20 ms SLIN frames are split into two 10 ms AEC3 calls;
//!   any error or unsupported rate falls through to passthrough so a call is never crashed.

use fswtch::{
    MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler, MediaFrame,
    MediaFrameMut,
};
use fswtch_aec3::EchoCanceller3;

fswtch::module_exports! {
    module = mod_aec3,
    load = switch_module_load,
}

const AEC3_APP: fswtch::ApplicationInfo = fswtch::ApplicationInfo::new(
    "rust_aec3",
    "Attaches a WebRTC AEC3 echo canceller (far-end render + near-end capture media bug)",
    "Rust WebRTC AEC3 echo cancellation",
    "rust_aec3",
);

/// Per-session AEC3 state held inside the media bug.
struct Aec3BugState {
    aec: Option<EchoCanceller3>,
    rate: i32,
    channels: usize,
    frames_processed: u64,
}

impl Aec3BugState {
    const fn new() -> Self {
        Self {
            aec: None,
            rate: 0,
            channels: 0,
            frames_processed: 0,
        }
    }

    /// Lazily creates the canceller at the first frame's rate/channels. Returns `false` (→ caller
    /// passthrough) for unsupported rates (32 kHz needs the QMF shim, not yet wired) or if the
    /// rate/channels change mid-call (AEC3 is created once). AEC3 supports 8/16/48 kHz.
    fn ensure(&mut self, rate: i32, channels: usize) -> bool {
        if !matches!(rate, 8000 | 16000 | 48000) || channels == 0 {
            return false;
        }
        if self.aec.is_some() {
            return self.rate == rate && self.channels == channels;
        }
        match EchoCanceller3::new(rate, channels, channels) {
            Ok(aec) => {
                self.aec = Some(aec);
                self.rate = rate;
                self.channels = channels;
                true
            }
            Err(e) => {
                fswtch::log_error(
                    "mod_aec3",
                    format!("EchoCanceller3::new({rate}, {channels}) failed: {e}"),
                );
                false
            }
        }
    }

    /// One 10 ms AEC3 frame = `(rate/100) * channels` interleaved samples.
    fn chunk_len(&self) -> usize {
        (self.rate as usize / 100) * self.channels
    }
}

impl MediaBugHandler for Aec3BugState {
    /// Far-end (loudspeaker) render: the audio written to the channel (played to the caller) is
    /// the echo source. Feed it to AEC3 in 10 ms chunks. `AnalyzeRender` is concurrency-safe with
    /// the capture side, so observing the write stream here is fine.
    fn on_write(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        frame: MediaFrame<'_>,
    ) -> MediaBugAction {
        let rate = frame.rate() as i32;
        let channels = frame.channels() as usize;
        let Some(pcm) = frame.pcm_i16() else {
            return MediaBugAction::Continue;
        };
        if !self.ensure(rate, channels) {
            return MediaBugAction::Continue;
        }
        // Copy out the metadata before mutably borrowing `self.aec` below.
        let chunk = self.chunk_len();
        let ch = self.channels;
        let Some(aec) = self.aec.as_mut() else {
            return MediaBugAction::Continue;
        };
        for half in pcm.chunks(chunk) {
            if half.len() == chunk
                && let Err(e) = aec.analyze_render(half, ch)
            {
                fswtch::log_error("mod_aec3", format!("analyze_render: {e}"));
                break;
            }
        }
        MediaBugAction::Continue
    }

    /// Near-end (mic) capture: remove echo in 10 ms chunks, writing the cleaned samples back into
    /// the frame in place. `level_change` is left false (no external gain-change detection here).
    fn on_read_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        let rate = frame.as_frame().rate() as i32;
        let channels = frame.as_frame().channels() as usize;
        if !self.ensure(rate, channels) {
            return MediaBugAction::Continue;
        }
        let chunk = self.chunk_len();
        let ch = self.channels;
        let Some(aec) = self.aec.as_mut() else {
            return MediaBugAction::Continue;
        };
        let Some(pcm) = frame.pcm_i16_mut() else {
            return MediaBugAction::Continue;
        };
        let mut processed: u64 = 0;
        for half in pcm.chunks_mut(chunk) {
            if half.len() == chunk {
                if let Err(e) = aec.process_capture(half, ch, false) {
                    fswtch::log_error("mod_aec3", format!("process_capture: {e}"));
                    break;
                }
                processed += 1;
            }
        }
        self.frames_processed += processed;
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        fswtch::log_info(
            "mod_aec3",
            format!(
                "media bug closing (rate={} ch={} frames={})",
                self.rate, self.channels, self.frames_processed
            ),
        );
    }
}

fswtch::app_callback! {
    fn aec3_app(session, _data) {
        fswtch::log_info("mod_aec3", "rust_aec3 application invoked");
        let Some(session) = session else {
            fswtch::log_error("mod_aec3", "missing session");
            return;
        };
        let config = match MediaBugConfig::new(
            "rust_aec3",
            "read-write",
            MediaBugFlags::WRITE_STREAM | MediaBugFlags::READ_REPLACE | MediaBugFlags::NO_PAUSE,
        ) {
            Ok(config) => config,
            Err(error) => {
                fswtch::log_error("mod_aec3", format!("invalid media bug config: {error}"));
                return;
            }
        };
        match fswtch::attach_media_bug(session, config, Aec3BugState::new()) {
            Ok(_) => fswtch::log_info("mod_aec3", "AEC3 media bug attached"),
            Err(error) => fswtch::log_error("mod_aec3", format!("failed to attach media bug: {error}")),
        }
    }
}

// Runs the real AEC3 on a synthetic echo (deterministic broadband noise render + delayed echo
// capture) inside the FreeSWITCH process and reports the achieved ERLE. The Docker smoke asserts
// the response contains `"aec3 ok"`.
fswtch::api_callback! {
    fn aec3_smoke_api(_cmd, _session, stream) {
        fswtch::log_info("mod_aec3", "rust_aec3_smoke invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };

        const RATE: i32 = 16_000;
        const CH: usize = 1;
        const FRAME: usize = (RATE as usize) / 100 * CH; // 160 samples / 10 ms
        const N_FRAMES: usize = 300; // 3 s
        const WARMUP: usize = 150; // 1.5 s convergence
        const ECHO_DELAY: usize = 64; // 4 ms

        let mut aec = match EchoCanceller3::new(RATE, CH, CH) {
            Ok(aec) => aec,
            Err(e) => {
                let _ = stream.write(&format!("aec3 fail create: {e}\n"));
                return fswtch::FALSE;
            }
        };

        // Deterministic LCG broadband noise render; capture = render delayed (pure echo).
        let mut lcg = 1u32;
        let mut render = vec![0i16; N_FRAMES * FRAME];
        for s in render.iter_mut() {
            lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
            *s = (((lcg >> 16) as i32 % 8000) - 4000) as i16;
        }
        let mut capture = vec![0i16; N_FRAMES * FRAME];
        capture[ECHO_DELAY..].copy_from_slice(&render[..render.len() - ECHO_DELAY]);

        let mut in_energy = 0.0_f64;
        let mut out_energy = 0.0_f64;
        for f in 0..N_FRAMES {
            let r = &render[f * FRAME..(f + 1) * FRAME];
            let c = &mut capture[f * FRAME..(f + 1) * FRAME];
            if aec.analyze_render(r, CH).is_err() {
                let _ = stream.write("aec3 fail analyze_render\n");
                return fswtch::FALSE;
            }
            if f >= WARMUP {
                for &s in c.iter() {
                    in_energy += (s as f64) * (s as f64);
                }
            }
            if aec.process_capture(c, CH, false).is_err() {
                let _ = stream.write("aec3 fail process_capture\n");
                return fswtch::FALSE;
            }
            if f >= WARMUP {
                for &s in c.iter() {
                    out_energy += (s as f64) * (s as f64);
                }
            }
        }
        let erle = 10.0 * (in_energy / out_energy.max(1e-12)).log10();
        stream.write(&format!("aec3 ok rate={RATE} erle={erle:.1}db\n"))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_aec3" {
        fswtch::log_info("mod_aec3", "loading module");
        module
            .application(AEC3_APP, aec3_app)
            .and_then(|module| {
                module.api(
                    "rust_aec3_smoke",
                    "runs the vendored WebRTC AEC3 on a synthetic echo and reports ERLE",
                    "rust_aec3_smoke",
                    aec3_smoke_api,
                )
            })
    }
}
