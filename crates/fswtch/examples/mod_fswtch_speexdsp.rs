//! mod_fswtch_speexdsp — AEC + NS + AGC media bug (FS built-in speexdsp + Rust AGC).
//!
//! Attaches a `READ_REPLACE | WRITE_REPLACE` media bug on the A-leg. Uses
//! FreeSWITCH's already-loaded `libspeexdsp` (speex echo cancellation + noise
//! suppression) and a self-developed Rust AGC (speex AGC collapses output).
//!
//! Config: global XML (`fswtch_speexdsp.conf.xml`) + per-call `data` parameter
//! in the dialplan app call. `data` overrides global; global overrides defaults.
//!
//! # Use
//! ```text
//! load mod_fswtch_speexdsp
//! <action application="fswtch_speexdsp" data="aec=true,ns=true,ns_db=-20"/>
//! <action application="socket" data="127.0.0.1:8084 async full"/>
//! ```
//!
//! `data` keys: `aec=true|false`, `tail=2048`, `ns=true|false`, `ns_db=-20`,
//! `echo_suppress=-6`, `agc=true|false`, `agc_target=3000`, `agc_max_gain=20`

use std::sync::{LazyLock, Mutex};

use fswtch::{
    ApplicationInfo, MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags,
    MediaBugHandler, MediaFrameMut, SpeexEcho, SpeexPreprocess, XmlConfig,
};

fswtch::module_exports! {
    module = mod_fswtch_speexdsp,
    load = switch_module_load,
}

// ── config ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
struct SpeexDspConfig {
    aec_enabled: bool,
    tail: i32,
    ns_enabled: bool,
    ns_db: i32,
    echo_suppress_db: i32,
    agc_enabled: bool,
    agc_target_rms: f32,
    agc_max_gain_db: f32,
}

impl Default for SpeexDspConfig {
    fn default() -> Self {
        Self {
            aec_enabled: true,
            tail: 2048,
            ns_enabled: true,
            ns_db: -20,
            echo_suppress_db: -6,
            agc_enabled: false,
            agc_target_rms: 3000.0,
            agc_max_gain_db: 20.0,
        }
    }
}

/// Parse `key=value,key=value` from the dialplan app `data` parameter.
/// Starts from `global`, overriding only the keys present in `data`.
fn parse_data(data: &str, global: SpeexDspConfig) -> SpeexDspConfig {
    let mut cfg = global;
    for pair in data.split(',') {
        let pair = pair.trim();
        if let Some((key, val)) = pair.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            match key {
                "aec" => cfg.aec_enabled = val == "true" || val == "1",
                "tail" => {
                    if let Ok(v) = val.parse::<i32>() {
                        cfg.tail = v;
                    }
                }
                "ns" => cfg.ns_enabled = val == "true" || val == "1",
                "ns_db" => {
                    if let Ok(v) = val.parse::<i32>() {
                        cfg.ns_db = v;
                    }
                }
                "echo_suppress" => {
                    if let Ok(v) = val.parse::<i32>() {
                        cfg.echo_suppress_db = v;
                    }
                }
                "agc" => cfg.agc_enabled = val == "true" || val == "1",
                "agc_target" => {
                    if let Ok(v) = val.parse::<f32>() {
                        cfg.agc_target_rms = v;
                    }
                }
                "agc_max_gain" => {
                    if let Ok(v) = val.parse::<f32>() {
                        cfg.agc_max_gain_db = v;
                    }
                }
                _ => {}
            }
        }
    }
    cfg
}

static GLOBAL_CONFIG: LazyLock<Mutex<SpeexDspConfig>> =
    LazyLock::new(|| Mutex::new(SpeexDspConfig::default()));

/// Read `fswtch_speexdsp.conf.xml` at module load via the high-level `XmlConfig` API.
/// Iterates `<settings><param name="..." value="..."/></settings>`. The `XmlConfig` guard
/// frees the parsed tree on drop, so no manual `switch_xml_free` is needed.
fn load_global_config() {
    let mut cfg = SpeexDspConfig::default();
    let Some(xml) = XmlConfig::open("fswtch_speexdsp") else {
        fswtch::log_info("mod_fswtch_speexdsp", "no config file — using defaults");
        return;
    };
    let Some(settings) = xml.settings().and_then(|node| node.child("settings")) else {
        fswtch::log_info(
            "mod_fswtch_speexdsp",
            "no <settings> in config — using defaults",
        );
        return;
    };
    let mut node = settings.child("param");
    while let Some(p) = node {
        if let (Some(name), Some(val)) = (p.attr("name"), p.attr("value")) {
            match name.as_str() {
                "aec_enabled" => cfg.aec_enabled = val == "true" || val == "1",
                "tail" => cfg.tail = val.parse().unwrap_or(2048),
                "ns_enabled" => cfg.ns_enabled = val == "true" || val == "1",
                "ns_db" => cfg.ns_db = val.parse().unwrap_or(-20),
                "echo_suppress_db" => cfg.echo_suppress_db = val.parse().unwrap_or(-6),
                "agc_enabled" => cfg.agc_enabled = val == "true" || val == "1",
                "agc_target_rms" => cfg.agc_target_rms = val.parse().unwrap_or(3000.0),
                "agc_max_gain_db" => cfg.agc_max_gain_db = val.parse().unwrap_or(20.0),
                _ => {}
            }
        }
        node = p.next();
    }
    if let Ok(mut global) = GLOBAL_CONFIG.lock() {
        *global = cfg;
    }
    fswtch::log_info("mod_fswtch_speexdsp", "global config loaded");
}

// ── self-developed AGC (ported from vox-seat audio_dsp::Agc) ───────────────

const AGC_ATTACK_COEF: f32 = 0.02; // slow gain increase (~50 frames)
const AGC_RELEASE_COEF: f32 = 0.25; // fast gain decrease (~4 frames)
const AGC_EPS: f32 = 1e-6;

struct Agc {
    target_rms: f32,
    max_gain: f32,
    gain: f32,
}

impl Agc {
    fn new(target_rms: f32, max_gain_db: f32) -> Self {
        Self {
            target_rms: target_rms.max(1.0),
            max_gain: 10.0_f32.powf(max_gain_db / 20.0),
            gain: 1.0,
        }
    }

    fn process(&mut self, pcm: &mut [i16]) {
        if pcm.is_empty() {
            return;
        }
        let sum_sq: f64 = pcm.iter().map(|&s| (s as f64).powi(2)).sum();
        let rms = (sum_sq / pcm.len() as f64).sqrt() as f32;
        let desired = (self.target_rms / (rms + AGC_EPS)).clamp(0.0, self.max_gain);
        let coef = if desired > self.gain {
            AGC_ATTACK_COEF
        } else {
            AGC_RELEASE_COEF
        };
        self.gain += (desired - self.gain) * coef;
        for s in pcm.iter_mut() {
            *s = ((*s as f32) * self.gain).round().clamp(-32768.0, 32767.0) as i16;
        }
    }
}

// ── media bug handler ───────────────────────────────────────────────────────

/// Field order matters: `preproc` borrows `echo` via `set_echo_state` (the preprocessor stores
/// a raw pointer to the echo state), so `echo` must outlive `preproc`. Rust drops fields in
/// declaration order, therefore `preproc` is declared first (dropped first) and `echo` last.
/// See `SpeexPreprocess::set_echo_state` safety note.
struct SpeexDspBug {
    preproc: Option<SpeexPreprocess>,
    echo: Option<SpeexEcho>,
    agc: Option<Agc>,
    cur_play: Vec<i16>,
    aec_out: Vec<i16>,
    cfg: SpeexDspConfig,
    frame_size: usize,
    sample_rate: i32,
    initialized: bool,
    /// Read frames processed (instrumentation for AEC far-end ref verification).
    r_n: u64,
    /// Write frames captured as far-end reference (instrumentation).
    w_n: u64,
}

impl SpeexDspBug {
    /// Lazy-init speexdsp on the first read frame.
    fn lazy_init(&mut self, pcm_len: usize) {
        if self.initialized || pcm_len == 0 {
            return;
        }
        self.frame_size = pcm_len;
        self.initialized = true;
        let fsi = pcm_len as i32;
        let sr = self.sample_rate;
        let cfg = self.cfg;

        if cfg.aec_enabled || cfg.ns_enabled {
            let Some(pp) = SpeexPreprocess::new(fsi, sr) else {
                fswtch::log_error(
                    "mod_fswtch_speexdsp",
                    format!("SpeexPreprocess::new({fsi}, {sr}) failed — AEC/NS disabled"),
                );
                return;
            };
            pp.set_denoise(cfg.ns_enabled);
            pp.set_noise_suppress(cfg.ns_db);
            pp.set_echo_suppress(cfg.echo_suppress_db);
            if cfg.aec_enabled {
                if let Some(echo) = SpeexEcho::new(fsi, cfg.tail) {
                    echo.set_sampling_rate(sr);
                    pp.set_echo_state(&echo);
                    self.echo = Some(echo);
                } else {
                    fswtch::log_error(
                        "mod_fswtch_speexdsp",
                        format!(
                            "SpeexEcho::new({fsi}, tail={}) failed — AEC off (NS still runs)",
                            cfg.tail
                        ),
                    );
                }
            }
            self.preproc = Some(pp);
        }

        self.agc = if cfg.agc_enabled {
            Some(Agc::new(cfg.agc_target_rms, cfg.agc_max_gain_db))
        } else {
            None
        };

        self.aec_out.resize(pcm_len.max(1), 0);

        fswtch::log_info(
            "mod_fswtch_speexdsp",
            format!(
                "init: frame={fsi}@{sr}Hz aec={} tail={} ns={}dB agc={} (target={:.0} max_gain={:.0}dB)",
                cfg.aec_enabled && self.echo.is_some(),
                cfg.tail,
                cfg.ns_db,
                cfg.agc_enabled,
                cfg.agc_target_rms,
                cfg.agc_max_gain_db,
            ),
        );
    }

    /// AEC + NS + AGC on the near-end frame, in-place.
    fn process(&mut self, pcm: &mut [i16]) {
        let cfg = self.cfg;
        if !cfg.aec_enabled && !cfg.ns_enabled && !cfg.agc_enabled {
            return;
        }
        let fs = pcm.len();
        self.r_n = self.r_n.wrapping_add(1);
        if self.r_n <= 5 || self.r_n.is_multiple_of(50) {
            fswtch::log_info(
                "mod_fswtch_speexdsp",
                format!(
                    "proc#{} fs={} frame_size={} cur_play={} preproc={} echo={} agc={}",
                    self.r_n,
                    fs,
                    self.frame_size,
                    self.cur_play.len(),
                    self.preproc.is_some(),
                    self.echo.is_some(),
                    self.agc.is_some()
                ),
            );
        }
        if fs == 0 {
            return;
        }
        self.lazy_init(fs);

        let Some(pp) = self.preproc.as_ref() else {
            if let Some(agc) = self.agc.as_mut() {
                agc.process(pcm);
            }
            return;
        };
        // speex states are fixed to the first frame's size (`frame_size`); a later size
        // mismatch (codec/ptime change) would overrun inside libspeexdsp. Fall back to
        // AGC-only and skip speex for this frame.
        if fs != self.frame_size {
            if let Some(agc) = self.agc.as_mut() {
                agc.process(pcm);
            }
            return;
        }

        if cfg.aec_enabled && self.echo.is_some() {
            if self.cur_play.len() != fs {
                if self.r_n <= 3 || self.r_n.is_multiple_of(50) {
                    fswtch::log_info(
                        "mod_fswtch_speexdsp",
                        format!(
                            "aec SKIP read#{}: cur_play={} fs={} (far-end ref not matched → AEC/NS/AGC bypassed)",
                            self.r_n,
                            self.cur_play.len(),
                            fs
                        ),
                    );
                }
                return; // no far-end ref yet / variable ptime → skip
            }
            if self.r_n <= 3 || self.r_n.is_multiple_of(50) {
                fswtch::log_info(
                    "mod_fswtch_speexdsp",
                    format!(
                        "aec RUN read#{}: cur_play={} fs={}",
                        self.r_n,
                        self.cur_play.len(),
                        fs
                    ),
                );
            }
            let Some(echo) = self.echo.as_ref() else {
                return;
            };
            let out: &mut [i16] = self.aec_out[..fs].as_mut();
            let play: &[i16] = &self.cur_play[..fs];
            echo.cancellation(pcm, play, out);
            let _ = pp.run(out);
            if let Some(agc) = self.agc.as_mut() {
                agc.process(out);
            }
            pcm.copy_from_slice(out);
        } else if cfg.ns_enabled {
            let _ = pp.run(pcm);
            if let Some(agc) = self.agc.as_mut() {
                agc.process(pcm);
            }
        } else if let Some(agc) = self.agc.as_mut() {
            agc.process(pcm);
        }
    }
}

impl MediaBugHandler for SpeexDspBug {
    fn on_init(&mut self, _ctx: &mut MediaBugContext<'_>) -> MediaBugAction {
        fswtch::log_info(
            "mod_fswtch_speexdsp",
            format!(
                "bug init: aec={} ns={} agc={} tail={}",
                self.cfg.aec_enabled, self.cfg.ns_enabled, self.cfg.agc_enabled, self.cfg.tail,
            ),
        );
        MediaBugAction::Continue
    }

    fn on_read_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        if let Some(pcm) = frame.pcm_i16_mut() {
            self.process(pcm);
        }
        MediaBugAction::Continue
    }

    fn on_write_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        if let Some(pcm) = frame.pcm_i16_mut() {
            self.cur_play.clear();
            self.cur_play.extend_from_slice(pcm);
            self.w_n = self.w_n.wrapping_add(1);
            if self.w_n <= 3 || self.w_n.is_multiple_of(50) {
                fswtch::log_info(
                    "mod_fswtch_speexdsp",
                    format!(
                        "write ref#{}: captured {} samples",
                        self.w_n,
                        self.cur_play.len()
                    ),
                );
            }
        }
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        fswtch::log_info("mod_fswtch_speexdsp", "bug closed");
    }
}

// ── app entry ──────────────────────────────────────────────────────────────

fswtch::app_callback! {
    fn speexdsp_app(session, data) {
        let Some(session) = session else {
            fswtch::log_error("mod_fswtch_speexdsp", "missing session");
            return;
        };
        let global = GLOBAL_CONFIG.lock().map(|c| *c).unwrap_or_default();
        let cfg = match data {
            Some(d) if !d.is_empty() => parse_data(&d, global),
            _ => global,
        };
        let sample_rate = session.read_sample_rate() as i32;
        let uuid = session.channel().and_then(|c| c.uuid()).unwrap_or_default();
        fswtch::log_info(
            "mod_fswtch_speexdsp",
            format!("attaching on {uuid}: aec={} ns={} agc={}", cfg.aec_enabled, cfg.ns_enabled, cfg.agc_enabled),
        );
        let handler = SpeexDspBug {
            preproc: None,
            echo: None,
            agc: None,
            cur_play: Vec::new(),
            aec_out: Vec::new(),
            cfg,
            frame_size: 0,
            sample_rate,
            initialized: false,
            r_n: 0,
            w_n: 0,
        };
        let config = match MediaBugConfig::new(
            "fswtch_speexdsp",
            "read-write-replace",
            MediaBugFlags::READ_REPLACE | MediaBugFlags::WRITE_REPLACE | MediaBugFlags::NO_PAUSE,
        ) {
            Ok(c) => c,
            Err(e) => {
                fswtch::log_error("mod_fswtch_speexdsp", format!("media bug config failed: {e}"));
                return;
            }
        };
        if let Err(error) = session.attach_media_bug(config, handler) {
            fswtch::log_error("mod_fswtch_speexdsp", format!("attach media bug failed: {error}"));
        }
    }
}

// ── API ────────────────────────────────────────────────────────────────────

fswtch::api_callback! {
    fn speexdsp_info_api(_cmd, _session, stream) {
        let Some(stream) = stream else { return fswtch::FALSE };
        let cfg = GLOBAL_CONFIG.lock().map(|c| *c).unwrap_or_default();
        stream.write(&format!(
            "fswtch_speexdsp config: aec={} tail={} ns={} ns_db={} echo_suppress={} agc={} target={:.0} max_gain={:.0}dB\n",
            cfg.aec_enabled, cfg.tail, cfg.ns_enabled, cfg.ns_db,
            cfg.echo_suppress_db, cfg.agc_enabled, cfg.agc_target_rms, cfg.agc_max_gain_db,
        ))
    }
}

// ── module load ────────────────────────────────────────────────────────────

fswtch::module_load! {
    fn switch_module_load(module) for "mod_fswtch_speexdsp" {
        fswtch::log_info("mod_fswtch_speexdsp", "loading module");
        load_global_config();
        module
            .application(
                ApplicationInfo::new(
                    "fswtch_speexdsp",
                    "AEC + NS + AGC media bug (FS built-in speexdsp + Rust AGC). \
                     Config: fswtch_speexdsp.conf.xml (global) + data param (per-call). \
                     data keys: aec, tail, ns, ns_db, echo_suppress, agc, agc_target, agc_max_gain",
                    "Rust speexdsp AEC+NS+AGC",
                    "fswtch_speexdsp [data]",
                ),
                speexdsp_app,
            )
            .and_then(|m| {
                m.api(
                    "fswtch_speexdsp_info",
                    "shows global speexdsp config",
                    "fswtch_speexdsp_info",
                    speexdsp_info_api,
                )
            })
    }
}
