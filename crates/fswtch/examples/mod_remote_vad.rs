use std::{
    error::Error,
    ffi::{CStr, CString, c_char},
    ptr, thread,
    time::Duration,
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const FRAME_INTERVAL: Duration = Duration::from_millis(20);
const MAX_FRAMES: usize = 80;
const LOCAL_VAD_ENERGY_THRESHOLD: u32 = 50_000;
const LOCAL_VAD_PERIOD: u64 = 23;

fswtch::module_exports! {
    module = mod_remote_vad,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn start_vad_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let Some(config) = VadConfig::parse(cmd) else {
        let status = write_response(
            stream,
            "usage: rust_vad_start <call-uuid> <wss://vad.example/session>\n",
        );
        return if status == SUCCESS { FALSE } else { status };
    };

    let status = write_response(stream, "remote VAD worker started\n");
    if status != SUCCESS {
        return status;
    }

    let worker = thread::Builder::new()
        .name("fswtch-remote-vad".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    if let Err(event_error) = fire_vad_event(
                        &config,
                        VadEventKind::Error,
                        &format!("failed to start async runtime: {error}"),
                        None,
                    ) {
                        eprintln!("failed to fire VAD runtime error event: {event_error}");
                    }
                    return;
                }
            };

            runtime.block_on(async move {
                if let Err(error) = run_remote_vad_worker(config.clone()).await
                    && let Err(event_error) = fire_vad_event(
                        &config,
                        VadEventKind::Error,
                        &format!("remote VAD worker failed: {error}"),
                        None,
                    )
                {
                    eprintln!("failed to fire VAD worker error event: {event_error}");
                }
            });
        });
    if let Err(error) = worker {
        eprintln!("failed to start remote VAD worker: {error}");
        return fswtch::GENERR;
    }

    SUCCESS
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_remote_vad") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
    if let Err(error) = unsafe {
        module.add_api(
            c"rust_vad_start",
            c"starts an async remote websocket VAD worker",
            c"rust_vad_start <call-uuid> <wss://vad.example/session>",
            start_vad_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}

#[derive(Debug, Clone)]
struct VadConfig {
    call_uuid: String,
    websocket_url: String,
}

impl VadConfig {
    fn parse(cmd: *const c_char) -> Option<Self> {
        let text = command_text(cmd)?;
        let mut fields = text.split_whitespace();
        let call_uuid = fields.next()?.to_owned();
        let websocket_url = fields.next()?.to_owned();

        Some(Self {
            call_uuid,
            websocket_url,
        })
    }
}

async fn run_remote_vad_worker(config: VadConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    fire_vad_event(
        &config,
        VadEventKind::Started,
        "connecting to remote VAD",
        None,
    )?;

    let (mut socket, _) = connect_async(config.websocket_url.as_str()).await?;
    fire_vad_event(&config, VadEventKind::Started, "remote VAD connected", None)?;

    // A production media module would feed this from a FreeSWITCH media bug attached to the call.
    // This example keeps the current binding surface small by modeling party audio from the UUID.
    for frame in PartyAudioFrames::new(&config).take(MAX_FRAMES) {
        let payload = encode_audio_message(&config, &frame);
        socket.send(Message::Text(payload.into())).await?;

        let Some(message) = socket.next().await else {
            break;
        };
        if let Some(result) = vad_result_from_message(message?, &frame)? {
            let message = if result.speech {
                "speech detected"
            } else {
                "silence detected"
            };
            fire_vad_event(&config, VadEventKind::Result, message, Some(&result))?;
        }
    }

    socket.close(None).await?;
    fire_vad_event(&config, VadEventKind::Stopped, "remote VAD stopped", None)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct AudioFrame {
    sequence: u64,
    sample_rate: u32,
    samples: Vec<i16>,
}

struct PartyAudioFrames {
    sequence: u64,
    call_uuid: String,
}

impl PartyAudioFrames {
    fn new(config: &VadConfig) -> Self {
        Self {
            sequence: 0,
            call_uuid: config.call_uuid.clone(),
        }
    }
}

impl Iterator for PartyAudioFrames {
    type Item = AudioFrame;

    fn next(&mut self) -> Option<Self::Item> {
        thread::sleep(FRAME_INTERVAL);
        self.sequence += 1;

        let speech_like =
            self.sequence.is_multiple_of(17) || self.call_uuid.len().is_multiple_of(2);
        let amplitude = if speech_like { 1200 } else { 42 };
        let samples = (0..160)
            .map(|index| {
                if index % 2 == 0 {
                    amplitude
                } else {
                    -amplitude
                }
            })
            .collect();

        Some(AudioFrame {
            sequence: self.sequence,
            sample_rate: 8_000,
            samples,
        })
    }
}

#[derive(Debug, Clone)]
struct VadResult {
    sequence: u64,
    speech: bool,
    confidence: String,
    label: String,
}

fn encode_audio_message(config: &VadConfig, frame: &AudioFrame) -> String {
    let energy: u32 = frame
        .samples
        .iter()
        .map(|sample| sample.unsigned_abs() as u32)
        .sum();

    json!({
        "type": "audio",
        "call_uuid": config.call_uuid,
        "sequence": frame.sequence,
        "sample_rate": frame.sample_rate,
        "sample_count": frame.samples.len(),
        "encoding": "pcm_s16le",
        "energy": energy,
    })
    .to_string()
}

fn vad_result_from_message(
    message: Message,
    frame: &AudioFrame,
) -> Result<Option<VadResult>, serde_json::Error> {
    match message {
        Message::Text(text) => parse_vad_json(&text).map(Some),
        Message::Binary(data) => {
            let text = String::from_utf8_lossy(&data);
            parse_vad_json(&text).map(Some)
        }
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(None),
        Message::Close(_) => Ok(Some(local_vad_fallback(frame))),
    }
}

fn parse_vad_json(text: &str) -> Result<VadResult, serde_json::Error> {
    let json: Value = serde_json::from_str(text)?;
    Ok(VadResult {
        sequence: json
            .get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        speech: json.get("speech").and_then(Value::as_bool).unwrap_or(false),
        confidence: json
            .get("confidence")
            .and_then(|value| match value {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "0.0".to_owned()),
        label: json
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
    })
}

fn local_vad_fallback(frame: &AudioFrame) -> VadResult {
    let energy: u32 = frame
        .samples
        .iter()
        .map(|sample| sample.unsigned_abs() as u32)
        .sum();
    let speech =
        energy > LOCAL_VAD_ENERGY_THRESHOLD || frame.sequence.is_multiple_of(LOCAL_VAD_PERIOD);

    VadResult {
        sequence: frame.sequence,
        speech,
        confidence: if speech { "0.91" } else { "0.12" }.to_owned(),
        label: if speech { "speech" } else { "silence" }.to_owned(),
    }
}

#[derive(Debug, Copy, Clone)]
enum VadEventKind {
    Started,
    Result,
    Error,
    Stopped,
}

impl VadEventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Result => "result",
            Self::Error => "error",
            Self::Stopped => "stopped",
        }
    }
}

fn fire_vad_event(
    config: &VadConfig,
    kind: VadEventKind,
    message: &str,
    result: Option<&VadResult>,
) -> fswtch::Result<()> {
    let mut event = ptr::null_mut();
    // SAFETY: FreeSWITCH initializes `event` for the custom subclass when the call succeeds.
    let status = unsafe {
        sys::switch_event_create_subclass_detailed(
            c"mod_remote_vad.rs".as_ptr(),
            c"fire_vad_event".as_ptr(),
            line!() as _,
            &mut event,
            sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
            c"fswtch::remote_vad".as_ptr(),
        )
    };
    fswtch::status_to_result(status)?;

    add_event_header(event, c"VAD-Event", kind.as_str())?;
    add_event_header(event, c"VAD-Call-UUID", &config.call_uuid)?;
    add_event_header(event, c"VAD-Websocket-URL", &config.websocket_url)?;
    add_event_header(event, c"VAD-Message", message)?;

    if let Some(result) = result {
        add_event_header(event, c"VAD-Sequence", &result.sequence.to_string())?;
        add_event_header(
            event,
            c"VAD-Speech",
            if result.speech { "true" } else { "false" },
        )?;
        add_event_header(event, c"VAD-Confidence", &result.confidence)?;
        add_event_header(event, c"VAD-Label", &result.label)?;
    }

    // SAFETY: `event` was created above and ownership is transferred to FreeSWITCH on success.
    let status = unsafe {
        sys::switch_event_fire_detailed(
            c"mod_remote_vad.rs".as_ptr(),
            c"fire_vad_event".as_ptr(),
            line!() as _,
            &mut event,
            ptr::null_mut(),
        )
    };
    fswtch::status_to_result(status)
}

fn add_event_header(
    event: *mut sys::switch_event_t,
    name: &'static CStr,
    value: &str,
) -> fswtch::Result<()> {
    let value = CString::new(value).map_err(|_| fswtch::SwitchError(fswtch::GENERR))?;
    // SAFETY: `event` is a live FreeSWITCH event and the header/value C strings are valid for
    // the duration of this call.
    let status = unsafe {
        sys::switch_event_add_header_string(
            event,
            sys::switch_stack_t::SWITCH_STACK_BOTTOM,
            name.as_ptr(),
            value.as_ptr(),
        )
    };
    fswtch::status_to_result(status)
}

fn command_text(cmd: *const c_char) -> Option<String> {
    if cmd.is_null() {
        return None;
    }

    // SAFETY: FreeSWITCH passes a null-terminated command string when one is present.
    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return SUCCESS;
    };
    if let Err(error) = stream.write_str(text) {
        return error.0;
    }

    SUCCESS
}
