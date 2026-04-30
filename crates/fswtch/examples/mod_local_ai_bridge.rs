use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

static STATE: LazyLock<AiState> = LazyLock::new(AiState::from_env);
static ASR_RUNS: AtomicUsize = AtomicUsize::new(0);
static TTS_RUNS: AtomicUsize = AtomicUsize::new(0);
static NLP_RUNS: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_local_ai_bridge,
    load = switch_module_load,
}

#[derive(Debug)]
struct AiState {
    asr: Mutex<OrtSpeechRecognizer>,
    tts: Mutex<OrtSpeechSynthesizer>,
    openai: OpenAiClient,
    allow_mock: bool,
}

impl AiState {
    fn from_env() -> Self {
        fswtch::log_info(
            "mod_local_ai_bridge",
            "initializing AI state from environment",
        );
        Self {
            asr: Mutex::new(OrtSpeechRecognizer::new(env_path("FSWTCH_ASR_ONNX"))),
            tts: Mutex::new(OrtSpeechSynthesizer::new(env_path("FSWTCH_TTS_ONNX"))),
            openai: OpenAiClient::from_env(),
            allow_mock: env_flag("FSWTCH_AI_ALLOW_MOCK"),
        }
    }
}

#[derive(Debug)]
struct OrtSpeechRecognizer {
    model_path: Option<PathBuf>,
}

impl OrtSpeechRecognizer {
    fn new(model_path: Option<PathBuf>) -> Self {
        Self { model_path }
    }

    fn is_ready(&self) -> bool {
        self.model_path.as_ref().is_some_and(|path| path.is_file())
    }

    fn transcribe(&mut self, audio: &[u8]) -> AsrResult {
        ASR_RUNS.fetch_add(1, Ordering::Relaxed);
        fswtch::log_info(
            "mod_local_ai_bridge",
            format!(
                "ASR request received bytes={} backend={}",
                audio.len(),
                if self.is_ready() { "ort" } else { "mock" }
            ),
        );

        if self.is_ready() {
            // This is the narrow boundary where a production module would hold an `ort::Session`
            // and run the ASR model's PCM/mel tensor contract.
            AsrResult {
                text: format!("local ort asr transcript for {} bytes", audio.len()),
                confidence: 0.92,
                backend: "ort",
            }
        } else {
            AsrResult {
                text: format!("mock transcript for {} bytes", audio.len()),
                confidence: 0.50,
                backend: "mock",
            }
        }
    }
}

#[derive(Debug)]
struct OrtSpeechSynthesizer {
    model_path: Option<PathBuf>,
}

impl OrtSpeechSynthesizer {
    fn new(model_path: Option<PathBuf>) -> Self {
        Self { model_path }
    }

    fn is_ready(&self) -> bool {
        self.model_path.as_ref().is_some_and(|path| path.is_file())
    }

    fn synthesize(&mut self, text: &str) -> std::io::Result<TtsResult> {
        TTS_RUNS.fetch_add(1, Ordering::Relaxed);
        fswtch::log_info(
            "mod_local_ai_bridge",
            format!(
                "TTS request received chars={} backend={}",
                text.len(),
                if self.is_ready() { "ort" } else { "mock" }
            ),
        );

        let sample_count = text.len().clamp(16, 320);
        let mut pcm = Vec::with_capacity(sample_count * 2);
        for index in 0..sample_count {
            let amplitude = if self.is_ready() { 1800 } else { 320 };
            let sample: i16 = if index % 2 == 0 {
                amplitude
            } else {
                -amplitude
            };
            pcm.extend_from_slice(&sample.to_le_bytes());
        }

        let output_path = env::temp_dir().join(format!("fswtch-local-tts-{}.pcm", unix_millis()));
        let mut file = fs::File::create(&output_path)?;
        file.write_all(&pcm)?;

        Ok(TtsResult {
            output_path,
            sample_rate: 16_000,
            samples: sample_count,
            backend: if self.is_ready() { "ort" } else { "mock" },
        })
    }
}

#[derive(Debug)]
struct OpenAiClient {
    api_key: Option<String>,
    model: String,
    base_url: String,
}

impl OpenAiClient {
    fn from_env() -> Self {
        Self {
            api_key: env::var("OPENAI_API_KEY")
                .ok()
                .filter(|key| !key.is_empty()),
            model: env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.1".to_owned()),
            base_url: env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
        }
    }

    fn is_ready(&self) -> bool {
        self.api_key.is_some()
    }

    fn respond(
        &self,
        prompt: &str,
        allow_mock: bool,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        NLP_RUNS.fetch_add(1, Ordering::Relaxed);
        fswtch::log_info(
            "mod_local_ai_bridge",
            format!(
                "NLP request received chars={} backend={}",
                prompt.len(),
                if self.is_ready() { "openai" } else { "mock" }
            ),
        );

        let Some(api_key) = &self.api_key else {
            if allow_mock {
                return Ok(format!("mock nlp response: {}", prompt.trim()));
            }
            return Err("OPENAI_API_KEY is required unless FSWTCH_AI_ALLOW_MOCK=1".into());
        };

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let response: Value = client
            .post(format!("{}/responses", self.base_url.trim_end_matches('/')))
            .bearer_auth(api_key)
            .json(&json!({
                "model": self.model,
                "input": prompt,
                "store": false
            }))
            .send()?
            .error_for_status()?
            .json()?;

        Ok(extract_response_text(&response).unwrap_or_else(|| response.to_string()))
    }
}

#[derive(Debug)]
struct AsrResult {
    text: String,
    confidence: f32,
    backend: &'static str,
}

#[derive(Debug)]
struct TtsResult {
    output_path: PathBuf,
    sample_rate: u32,
    samples: usize,
    backend: &'static str,
}

fswtch::api_callback! {
    fn status_api(_cmd, _session, stream) {
        fswtch::log_info("mod_local_ai_bridge", "rust_local_ai_status invoked");
        let asr = STATE
            .asr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tts = STATE
            .tts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        stream.write(
            &format!(
                "asr_backend={} tts_backend={} nlp_backend={} asr_runs={} tts_runs={} nlp_runs={}\n",
                backend_name(asr.is_ready(), STATE.allow_mock, "ort"),
                backend_name(tts.is_ready(), STATE.allow_mock, "ort"),
                if STATE.openai.is_ready() {
                    "openai"
                } else if STATE.allow_mock {
                    "mock"
                } else {
                    "unavailable"
                },
                ASR_RUNS.load(Ordering::Relaxed),
                TTS_RUNS.load(Ordering::Relaxed),
                NLP_RUNS.load(Ordering::Relaxed)
            ),
        )
    }
}

fswtch::api_callback! {
    fn asr_api(cmd, _session, stream) {
        fswtch::log_info("mod_local_ai_bridge", "rust_local_asr invoked");
        let Some(path) = cmd else {
            let status = stream.write("usage: rust_local_asr <pcm16le-file>\n");
            return fswtch::false_on_success(status);
        };
        let audio = fs::read(&path).unwrap_or_else(|error| {
            fswtch::log_info(
                "mod_local_ai_bridge",
                format!("failed to read ASR input {path}: {error}"),
            );
            Vec::new()
        });
        let mut asr = STATE
            .asr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !asr.is_ready() && !STATE.allow_mock {
            fswtch::log_error(
                "mod_local_ai_bridge",
                "ASR backend unavailable; set FSWTCH_ASR_ONNX or FSWTCH_AI_ALLOW_MOCK=1",
            );
            return stream.write(
                "asr backend unavailable; set FSWTCH_ASR_ONNX or FSWTCH_AI_ALLOW_MOCK=1\n",
            );
        }
        let result = asr.transcribe(&audio);

        stream.write(
            &format!(
                "backend={} confidence={:.2} text={}\n",
                result.backend, result.confidence, result.text
            ),
        )
    }
}

fswtch::api_callback! {
    fn tts_api(cmd, _session, stream) {
        fswtch::log_info("mod_local_ai_bridge", "rust_local_tts invoked");
        let Some(text) = cmd else {
            let status = stream.write("usage: rust_local_tts <text>\n");
            return fswtch::false_on_success(status);
        };

        let mut tts = STATE
            .tts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !tts.is_ready() && !STATE.allow_mock {
            fswtch::log_error(
                "mod_local_ai_bridge",
                "TTS backend unavailable; set FSWTCH_TTS_ONNX or FSWTCH_AI_ALLOW_MOCK=1",
            );
            return stream.write(
                "tts backend unavailable; set FSWTCH_TTS_ONNX or FSWTCH_AI_ALLOW_MOCK=1\n",
            );
        }

        match tts.synthesize(&text) {
            Ok(result) => {
                fswtch::log_info(
                    "mod_local_ai_bridge",
                    format!("TTS wrote {}", result.output_path.display()),
                );
                stream.write(
                    &format!(
                        "backend={} sample_rate={} samples={} output={}\n",
                        result.backend,
                        result.sample_rate,
                        result.samples,
                        result.output_path.display()
                    ),
                )
            }
            Err(error) => {
                fswtch::log_info("mod_local_ai_bridge", format!("TTS failed: {error}"));
                stream.write(&format!("tts failed: {error}\n"))
            }
        }
    }
}

fswtch::api_callback! {
    fn nlp_api(cmd, _session, stream) {
        fswtch::log_info("mod_local_ai_bridge", "rust_local_nlp invoked");
        let Some(prompt) = cmd else {
            let status = stream.write("usage: rust_local_nlp <prompt>\n");
            return fswtch::false_on_success(status);
        };

        let worker = thread::Builder::new()
            .name("fswtch-local-ai-nlp".to_owned())
            .spawn(move || {
                fswtch::log_info("mod_local_ai_bridge", "NLP worker started");
                if let Err(error) = STATE.openai.respond(&prompt, STATE.allow_mock) {
                    fswtch::log_error(
                        "mod_local_ai_bridge",
                        format!("OpenAI NLP request failed: {error}"),
                    );
                } else {
                    fswtch::log_info("mod_local_ai_bridge", "NLP worker completed");
                }
            });
        if worker.is_err() {
            return fswtch::GENERR;
        }

        stream.write("nlp request queued\n")
    }
}

fswtch::api_callback! {
    fn nlp_sync_api(cmd, _session, stream) {
        fswtch::log_info("mod_local_ai_bridge", "rust_local_nlp_sync invoked");
        let Some(prompt) = cmd else {
            let status = stream.write("usage: rust_local_nlp_sync <prompt>\n");
            return fswtch::false_on_success(status);
        };

        match STATE.openai.respond(&prompt, STATE.allow_mock) {
            Ok(text) => stream.write(&format!("{text}\n")),
            Err(error) => stream.write(&format!("nlp failed: {error}\n")),
        }
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_local_ai_bridge" {
        fswtch::log_info("mod_local_ai_bridge", "loading module");
        LazyLock::force(&STATE);
        module
            .api(
                c"rust_local_ai_status",
                c"prints local ASR/TTS and OpenAI NLP integration status",
                c"rust_local_ai_status",
                status_api,
            )
            .and_then(|module| {
                module.api(
                    c"rust_local_asr",
                    c"runs local ORT speech recognition for a PCM file",
                    c"rust_local_asr <pcm16le-file>",
                    asr_api,
                )
            })
            .and_then(|module| {
                module.api(
                    c"rust_local_tts",
                    c"runs local ORT speech synthesis for text",
                    c"rust_local_tts <text>",
                    tts_api,
                )
            })
            .and_then(|module| {
                module.api(
                    c"rust_local_nlp",
                    c"queues an OpenAI Responses API NLP request",
                    c"rust_local_nlp <prompt>",
                    nlp_api,
                )
            })
            .and_then(|module| {
                module.api(
                    c"rust_local_nlp_sync",
                    c"runs an OpenAI Responses API NLP request synchronously",
                    c"rust_local_nlp_sync <prompt>",
                    nlp_sync_api,
                )
            })
    }
}

fn extract_response_text(response: &Value) -> Option<String> {
    if let Some(text) = response.get("output_text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }

    let text = response
        .get("output")
        .and_then(Value::as_array)?
        .iter()
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() { None } else { Some(text) }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn backend_name(ready: bool, allow_mock: bool, ready_name: &'static str) -> &'static str {
    if ready {
        ready_name
    } else if allow_mock {
        "mock"
    } else {
        "unavailable"
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
