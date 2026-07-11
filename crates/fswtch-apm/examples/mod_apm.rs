//! FreeSWITCH `mod_apm` — the full WebRTC audio-processing chain (AEC3 + NS + AGC2 + HPF) as a
//! loadable module.
//!
//! Two surfaces:
//! - `fswtch_apm_smoke` API: runs the whole chain (HPF → AEC3 → NS → AGC2) on a synthetic echo
//!   inside the FreeSWITCH process and reports the achieved ERLE. Proves all four modules load +
//!   link + run in-process (the Docker/local smoke checks `"apm ok"`).
//! - `fswtch_apm` dialplan application: attaches a media bug (`WRITE_STREAM` = far-end render,
//!   `READ_REPLACE` = near-end capture). Per 10 ms tick: render → AEC3.analyze_render; capture →
//!   HPF → AEC3.process_capture → NS → AGC2. 20 ms SLIN frames are split into two 10 ms ticks;
//!   any error / unsupported rate falls through to passthrough so a call is never crashed.
//!
//! AGC2 is used as fixed-gain 0 dB + limiter (no adaptive/RNN). AEC3 uses the default config.

use fswtch::{
    MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler, MediaFrame,
    MediaFrameMut,
};
use fswtch_apm::{EchoCanceller3, GainController2, HighPassFilter, NoiseSuppressor, NsLevel};

fswtch::module_exports! {
    module = mod_apm,
    load = switch_module_load,
}

const APM_APP: fswtch::ApplicationInfo = fswtch::ApplicationInfo::new(
    "fswtch_apm",
    "Attaches the WebRTC APM chain: HPF -> AEC3 -> NS -> AGC2 (far-end render + near-end capture)",
    "Rust WebRTC audio processing chain",
    "fswtch_apm",
);

/// Per-session APM state held inside the media bug.
struct ApmState {
    aec: Option<EchoCanceller3>,
    ns: Option<NoiseSuppressor>,
    agc2: Option<GainController2>,
    hpf: Option<HighPassFilter>,
    rate: i32,
    channels: usize,
    frames_processed: u64,
}

impl ApmState {
    const fn new() -> Self {
        Self {
            aec: None,
            ns: None,
            agc2: None,
            hpf: None,
            rate: 0,
            channels: 0,
            frames_processed: 0,
        }
    }

    /// Lazily creates the four modules at the first frame's rate/channels. Returns `false` (→
    /// passthrough) for unsupported rates (32 kHz needs the QMF shim) or a mid-call rate change.
    fn ensure(&mut self, rate: i32, channels: usize) -> bool {
        if !matches!(rate, 8000 | 16000 | 48000) || channels == 0 {
            return false;
        }
        if self.aec.is_some() {
            return self.rate == rate && self.channels == channels;
        }
        // AEC3 default config, NS 12 dB, AGC2 fixed 0 dB + limiter, HPF.
        let (aec, ns, agc2, hpf) = match (
            EchoCanceller3::new(rate, channels, channels),
            NoiseSuppressor::new(NsLevel::Db12, rate, channels),
            GainController2::new(0.0, true, rate, channels),
            HighPassFilter::new(rate, channels),
        ) {
            (Ok(a), Ok(n), Ok(g), Ok(h)) => (a, n, g, h),
            _ => {
                fswtch::log_error("mod_apm", "module creation failed");
                return false;
            }
        };
        self.aec = Some(aec);
        self.ns = Some(ns);
        self.agc2 = Some(agc2);
        self.hpf = Some(hpf);
        self.rate = rate;
        self.channels = channels;
        true
    }

    fn chunk_len(&self) -> usize {
        (self.rate as usize / 100) * self.channels
    }
}

impl MediaBugHandler for ApmState {
    /// Far-end (loudspeaker) render → AEC3.analyze_render (the echo source).
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
        let ch = self.channels;
        let chunk = self.chunk_len();
        let Some(aec) = self.aec.as_mut() else {
            return MediaBugAction::Continue;
        };
        for half in pcm.chunks(chunk) {
            if half.len() == chunk
                && let Err(e) = aec.analyze_render(half, ch)
            {
                fswtch::log_error("mod_apm", format!("analyze_render: {e}"));
                break;
            }
        }
        MediaBugAction::Continue
    }

    /// Near-end (mic) capture → HPF → AEC3.process_capture → NS → AGC2, in place.
    fn on_read_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        let rate = frame.as_frame().rate() as i32;
        let channels = frame.as_frame().channels() as usize;
        let Some(pcm) = frame.pcm_i16_mut() else {
            return MediaBugAction::Continue;
        };
        if !self.ensure(rate, channels) {
            return MediaBugAction::Continue;
        }
        let ch = self.channels;
        let chunk = self.chunk_len();
        let Some(aec) = self.aec.as_mut() else {
            return MediaBugAction::Continue;
        };
        let Some(ns) = self.ns.as_mut() else {
            return MediaBugAction::Continue;
        };
        let Some(agc2) = self.agc2.as_mut() else {
            return MediaBugAction::Continue;
        };
        let Some(hpf) = self.hpf.as_mut() else {
            return MediaBugAction::Continue;
        };
        let mut processed: u64 = 0;
        for half in pcm.chunks_mut(chunk) {
            if half.len() != chunk {
                break;
            }
            // HPF -> AEC3 -> NS -> AGC2. Each stage mutates the slice in place.
            if hpf.process(half, ch).is_err()
                || aec.process_capture(half, ch, false).is_err()
                || ns.process(half, ch).is_err()
                || agc2.process(half, ch).is_err()
            {
                fswtch::log_error("mod_apm", "chain stage failed");
                break;
            }
            processed += 1;
        }
        self.frames_processed += processed;
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        fswtch::log_info(
            "mod_apm",
            format!(
                "media bug closing (rate={} ch={} frames={})",
                self.rate, self.channels, self.frames_processed
            ),
        );
    }
}

fswtch::app_callback! {
    fn apm_app(session, _data) {
        fswtch::log_info("mod_apm", "fswtch_apm application invoked");
        let Some(session) = session else {
            fswtch::log_error("mod_apm", "missing session");
            return;
        };
        let config = match MediaBugConfig::new(
            "fswtch_apm",
            "read-write",
            MediaBugFlags::WRITE_STREAM | MediaBugFlags::READ_REPLACE | MediaBugFlags::NO_PAUSE,
        ) {
            Ok(config) => config,
            Err(error) => {
                fswtch::log_error("mod_apm", format!("invalid media bug config: {error}"));
                return;
            }
        };
        match fswtch::attach_media_bug(session, config, ApmState::new()) {
            Ok(_) => fswtch::log_info("mod_apm", "APM media bug attached"),
            Err(error) => fswtch::log_error("mod_apm", format!("failed to attach media bug: {error}")),
        }
    }
}

// Runs the full APM chain (HPF → AEC3 → NS → AGC2) on a synthetic echo inside the FreeSWITCH
// process and reports the AEC3 ERLE. The smoke asserts the response contains `"apm ok"`.
fswtch::api_callback! {
    fn apm_smoke_api(_cmd, _session, stream) {
        fswtch::log_info("mod_apm", "fswtch_apm_smoke invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };

        const RATE: i32 = 16_000;
        const CH: usize = 1;
        const FRAME: usize = (RATE as usize) / 100 * CH;
        const N_FRAMES: usize = 300;
        const WARMUP: usize = 150;
        const ECHO_DELAY: usize = 64;

        let mut aec = match EchoCanceller3::new(RATE, CH, CH) {
            Ok(a) => a,
            Err(e) => {
                let _ = stream.write(&format!("apm fail aec3: {e}\n"));
                return fswtch::FALSE;
            }
        };
        let mut ns = match NoiseSuppressor::new(NsLevel::Db12, RATE, CH) {
            Ok(n) => n,
            Err(e) => {
                let _ = stream.write(&format!("apm fail ns: {e}\n"));
                return fswtch::FALSE;
            }
        };
        let mut agc2 = match GainController2::new(0.0, true, RATE, CH) {
            Ok(g) => g,
            Err(e) => {
                let _ = stream.write(&format!("apm fail agc2: {e}\n"));
                return fswtch::FALSE;
            }
        };
        let mut hpf = match HighPassFilter::new(RATE, CH) {
            Ok(h) => h,
            Err(e) => {
                let _ = stream.write(&format!("apm fail hpf: {e}\n"));
                return fswtch::FALSE;
            }
        };

        // Render = broadband noise; capture = render delayed (pure echo). Run the chain on
        // capture: HPF -> AEC3 -> NS -> AGC2; render only feeds AEC3.analyze_render.
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
                let _ = stream.write("apm fail analyze_render\n");
                return fswtch::FALSE;
            }
            if f >= WARMUP {
                for &s in c.iter() {
                    in_energy += (s as f64) * (s as f64);
                }
            }
            // HPF -> AEC3 -> NS -> AGC2.
            if hpf.process(c, CH).is_err()
                || aec.process_capture(c, CH, false).is_err()
                || ns.process(c, CH).is_err()
                || agc2.process(c, CH).is_err()
            {
                let _ = stream.write("apm fail chain\n");
                return fswtch::FALSE;
            }
            if f >= WARMUP {
                for &s in c.iter() {
                    out_energy += (s as f64) * (s as f64);
                }
            }
        }
        let erle = 10.0 * (in_energy / out_energy.max(1e-12)).log10();
        stream.write(&format!("apm ok rate={RATE} erle={erle:.1}db\n"))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_apm" {
        fswtch::log_info("mod_apm", "loading module");
        module
            .application(APM_APP, apm_app)
            .and_then(|module| {
                module.api(
                    "fswtch_apm_smoke",
                    "runs the WebRTC APM chain (HPF->AEC3->NS->AGC2) on a synthetic echo",
                    "fswtch_apm_smoke",
                    apm_smoke_api,
                )
            })
    }
}
