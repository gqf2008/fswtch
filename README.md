# fswtch

[![Crates.io Version](https://img.shields.io/crates/v/fswtch)](https://crates.io/crates/fswtch)
[![fswtch-sys](https://img.shields.io/crates/v/fswtch-sys?label=fswtch-sys)](https://crates.io/crates/fswtch-sys)

Rust bindings and helper APIs for writing FreeSWITCH modules.

This workspace is intentionally split into three crates:

- `fswtch`: safe-ish helpers for module exports, module interface creation, API command registration, stream writes, status conversion, and example logging.
- `fswtch-sys`: raw FreeSWITCH ABI bindings generated with bindgen.
- `fswtch-src`: packaged FreeSWITCH headers used by default bundled builds.

## Wrapper API

The `fswtch` crate provides a higher-level layer over the raw FreeSWITCH ABI. It covers the parts every module needs first and is organized into focused subsystems.

Core (session, channel, caller):

- `module_exports!` declares the exported FreeSWITCH module table.
- `module_load!` generates the FreeSWITCH load callback while giving the body a `ModuleBuilder`.
- `Module::create` builds the loader-owned module interface from raw load callback arguments for lower-level integrations.
- `Module::add_api`, `Module::add_application`, `Module::add_chat_application`, and `Module::add_endpoint` register common FreeSWITCH interfaces without hand-writing interface allocation and field assignment.
- `ModuleBuilder` chains module and interface registration in load callbacks.
- `api_callback!`, `app_callback!`, and `chat_callback!` generate FreeSWITCH ABI callbacks while giving the callback body typed wrapper values.
- `Session` and `SessionGuard` wrap common application callback operations such as answering, sleeping, and file playback, and provide a `from_raw` bridge from raw `switch_core_session_t` pointers.
- `Channel` exposes channel state, hangup cause helpers (`cause_to_str`, `str_to_cause`), and channel variable get/set over a live session.
- `CallerProfile` reads caller-profile fields such as `username` and `caller_id_number`.
- `command_text` converts nullable FreeSWITCH callback command pointers into trimmed Rust strings.
- Core helpers `get_domain`, `get_hostname`, `get_switchname`, `get_uuid`, `get_variable`, and `set_variable` read and write core/global state from Rust strings.

Events:

- `Event`, `EventRef`, and `EventBinder` wrap custom event creation, headers, firing, cleanup, and inbound event header reads while accepting Rust strings. `EventBinder::bind` registers an `event_callback!`-generated function for a specific event node.
- `event_callback!` generates a FreeSWITCH event callback that wraps the raw event pointer in an `EventRef` for a safe body.

Media (codec, timer, media bug, resample, VAD, video, jitter buffer):

- `Codec` wraps `switch_codec` initialization and negotiation.
- `Timer` wraps `switch_timer` over a named interval.
- `MediaBugConfig`, `MediaBugFlags`, `MediaBugHandler`, `MediaFrame`, and `attach_media_bug` provide a higher-level media bug API for bidirectional read/write audio stream callbacks and read/write replacement hooks.
- `Resample` and the `Agc`/`AgcConfig` types wrap the resampler and automatic-gain-control interfaces; helpers like `change_sln_volume`, `mux_channels`, `merge_sln`, `unmerge_sln`, `short_to_float`, and `float_to_short` operate on SLN PCM frames.
- `Vad` and `VadState` feed PCM frames to FreeSWITCH's voice activity detector and report silence/speech.
- `Chromakey`, `Color`, `Image`, and `ImageFormat` wrap video frame helpers for image overlay and chromakey setup.
- `JitterBuffer`, `JitterBufferConfig`, `JbFrames`, `JbFlag`, and `JbKind` configure and drive the jitter buffer.

IVR:

- `park` and `record_file` wrap the common IVR application entry points.

Storage (core_db, limit):

- `CoreDb`, `Stmt`, and `StmtRows` wrap the in-memory SQLite (`switch_core_db`) handle with bound parameter INSERT, SELECT iteration, and `column_text` reads.
- `limit` exposes `init`, `incr`, `release`, `reset`, `interval_reset`, `usage`, `status`, `fire_event`, and `backend` over FreeSWITCH's limit backends.

Utilities (buffer, regex, utils, console):

- `Buffer` wraps `switch_buffer` with write/peek/read, `toss`, and `inuse`/`len`/`freespace` accounting.
- `Regex`, `RegexMatch`, and `CaptureCallback` wrap PCRE2 (`switch_regex`) with `compile`, `matches`, `is_match`, and capture iteration; free functions `is_match` and `is_match_partial` are available for one-shot checks.
- `utils` provides `escape_string`, `format_number`, `url_encode`, and `find_end_paren` string utilities.
- `console` exposes `complete`, `execute`, `expand_alias`, `CompletionFunc`, `CompletionMatches`, and `free_matches` for console completion and alias expansion.

Networking:

- `Rtp`, `RtpConfig`, and `request_port` wrap `switch_rtp` session creation and port allocation.

Scheduling:

- `scheduler` exposes `TaskHandler`, `spawn`, `start`, `stop`, `cancel_group`, `TaskConfig`, `TaskFlags`, and `TaskHandle` over FreeSWITCH's background task scheduler.

Endpoint IO:

- `endpoint` exposes `IoRoutinesBuilder`, `Frame`, `FrameMut`, `IoFlags`, `SessionMessage`, `Dtmf`, and `DtmfSource` for endpoint I/O routine table construction and inbound frame/DTMF handling.

Memory:

- `Pool` wraps `switch_memory_pool_t` allocation and lifetimes.

Logging, status, stream, and XML helpers (shared across subsystems):

- `Status`, `SwitchError`, and `status_to_result` convert common FreeSWITCH status handling into Rust `Result` values.
- `LogLevel`, `log`, and convenience helpers such as `log_info`, `log_warning`, `log_error`, and `log_debug1` through `log_debug10` route module logs through FreeSWITCH logging.
- `Stream`, `ApiStream`, and `write_stream_response` wrap `switch_stream_handle_t` for byte and string responses.
- `XmlConfig` and `XmlNode` wrap FreeSWITCH XML config loading and traversal.
- Module registration, media bug config, session playback, XML helpers, and event helpers convert Rust strings to C strings inside `fswtch`.

cJSON is intentionally not wrapped. Use Rust `serde_json` for JSON in modules; anything not yet wrapped is an internal `fswtch::sys` detail and is NOT part of the public API.

The raw `fswtch-sys` crate is deliberately hidden: `fswtch::sys` is `pub(crate)`, no `*-sys` type appears in any documented signature, all `#[macro_export]` macros expand to `sys`-free code, and the module-interface table is built through the `#[doc(hidden)]` `__ModuleFunctionTable` wrapper. Examples use only the safe wrappers + macros — endpoint I/O routine tables (`EndpointIoBuilder::build::<T>()`), lifecycle callbacks (`module_exports!` with `-> fswtch::Status`), and state-change hooks (`EndpointIoRoutines::state_change`) all stay in safe Rust. Prefer adding focused safe helpers to `fswtch` over reaching for raw types.

Media bug handlers are owned by FreeSWITCH until the close callback. A module can implement `MediaBugHandler` to observe read and write frames, mutate replacement frames, or pull frames explicitly through `MediaBugContext`:

```rust
struct Meter;

impl fswtch::MediaBugHandler for Meter {
    fn on_read(
        &mut self,
        _ctx: &mut fswtch::MediaBugContext<'_>,
        frame: fswtch::MediaFrame<'_>,
    ) -> fswtch::MediaBugAction {
        fswtch::log_debug("mod_meter", format!("read {} bytes", frame.data_len()));
        fswtch::MediaBugAction::Continue
    }
}

let config = fswtch::MediaBugConfig::new(
    "mod_meter",
    "read-write",
    fswtch::MediaBugFlags::READ_STREAM
        | fswtch::MediaBugFlags::WRITE_STREAM
        | fswtch::MediaBugFlags::NO_PAUSE,
)?;

fswtch::attach_media_bug(session, config, Meter)?;
```

## Subsystem coverage

| Subsystem | fswtch type(s) | Example |
| --- | --- | --- |
| Module / load interface | `module_exports!`, `module_load!`, `ModuleBuilder`, `Module` | [`mod_hello.rs`](crates/fswtch/examples/mod_hello.rs) |
| API / app / chat callbacks | `api_callback!`, `app_callback!`, `chat_callback!` | [`mod_api_suite.rs`](crates/fswtch/examples/mod_api_suite.rs) |
| Session | `Session`, `SessionGuard` | [`mod_app_playback_control.rs`](crates/fswtch/examples/mod_app_playback_control.rs) |
| Channel | `Channel`, `cause_to_str`, `str_to_cause` | [`mod_channel_vars.rs`](crates/fswtch/examples/mod_channel_vars.rs) |
| Caller profile | `CallerProfile` | [`mod_channel_vars.rs`](crates/fswtch/examples/mod_channel_vars.rs) |
| Core helpers | `get_domain`, `get_hostname`, `get_switchname`, `get_uuid`, `get_variable`, `set_variable` | [`mod_channel_vars.rs`](crates/fswtch/examples/mod_channel_vars.rs) |
| Events | `Event`, `EventRef`, `EventBinder`, `event_callback!` | [`mod_event_listener.rs`](crates/fswtch/examples/mod_event_listener.rs) |
| Event firing | `Event` (create/fire) | [`mod_event_sink.rs`](crates/fswtch/examples/mod_event_sink.rs) |
| Codec | `Codec` | [`mod_media_bug_meter.rs`](crates/fswtch/examples/mod_media_bug_meter.rs) |
| Timer | `Timer` | [`mod_media_bug_meter.rs`](crates/fswtch/examples/mod_media_bug_meter.rs) |
| Media bug | `MediaBug`, `MediaBugConfig`, `MediaBugFlags`, `MediaBugHandler`, `attach_media_bug` | [`mod_media_bug_meter.rs`](crates/fswtch/examples/mod_media_bug_meter.rs) |
| Resample / AGC | `Resample`, `Agc`, `AgcConfig`, `change_sln_volume`, `mux_channels` | [`mod_stream_tools.rs`](crates/fswtch/examples/mod_stream_tools.rs) |
| VAD | `Vad`, `VadState` | [`mod_vad_detect.rs`](crates/fswtch/examples/mod_vad_detect.rs) |
| ASR → ESL + ESL → TTS | `Event`, `EventBinder`, `event_callback!`, `Session::execute_application`, `execute_application_async`, `EventType::DETECTED_SPEECH` | [`mod_vad_esl.rs`](crates/fswtch/examples/mod_vad_esl.rs) |
| Video | `Chromakey`, `Color`, `Image`, `ImageFormat` | [`mod_endpoint_skeleton.rs`](crates/fswtch/examples/mod_endpoint_skeleton.rs) |
| Jitter buffer | `JitterBuffer`, `JitterBufferConfig`, `JbFrames`, `JbFlag`, `JbKind` | [`mod_endpoint_skeleton.rs`](crates/fswtch/examples/mod_endpoint_skeleton.rs) |
| IVR | `park`, `record_file` | [`mod_app_playback_control.rs`](crates/fswtch/examples/mod_app_playback_control.rs) |
| Core DB (SQLite) | `CoreDb`, `Stmt`, `StmtRows` | [`mod_db_lookup.rs`](crates/fswtch/examples/mod_db_lookup.rs) |
| Limit | `limit::init`, `incr`, `release`, `reset`, `usage`, `fire_event`, `backend` | [`mod_rate_limiter.rs`](crates/fswtch/examples/mod_rate_limiter.rs) |
| Buffer | `Buffer` | [`mod_buffer_demo.rs`](crates/fswtch/examples/mod_buffer_demo.rs) |
| Regex (PCRE2) | `Regex`, `RegexMatch`, `CaptureCallback`, `is_match` | [`mod_regex_match.rs`](crates/fswtch/examples/mod_regex_match.rs) |
| Utils | `escape_string`, `format_number`, `url_encode`, `find_end_paren` | [`mod_utils_demo.rs`](crates/fswtch/examples/mod_utils_demo.rs) |
| Console | `complete`, `execute`, `expand_alias`, `CompletionFunc`, `CompletionMatches` | [`mod_lifecycle.rs`](crates/fswtch/examples/mod_lifecycle.rs) |
| RTP | `Rtp`, `RtpConfig`, `request_port` | [`mod_remote_vad.rs`](crates/fswtch/examples/mod_remote_vad.rs) |
| Scheduler | `TaskHandler`, `spawn`, `TaskConfig`, `TaskHandle`, `cancel_group` | [`mod_scheduler_task.rs`](crates/fswtch/examples/mod_scheduler_task.rs) |
| Endpoint IO | `IoRoutinesBuilder`, `Frame`, `FrameMut`, `IoFlags`, `SessionMessage`, `Dtmf` | [`mod_endpoint_skeleton.rs`](crates/fswtch/examples/mod_endpoint_skeleton.rs) |
| Memory pool | `Pool` | [`mod_endpoint_skeleton.rs`](crates/fswtch/examples/mod_endpoint_skeleton.rs) |
| Status / Result | `Status`, `SwitchError`, `status_to_result`, `Cause` | [`mod_hello.rs`](crates/fswtch/examples/mod_hello.rs) |
| Logging | `LogLevel`, `log`, `log_info` .. `log_debug10` | [`mod_hello.rs`](crates/fswtch/examples/mod_hello.rs) |
| Stream | `Stream`, `ApiStream`, `write_stream_response` | [`mod_stream_tools.rs`](crates/fswtch/examples/mod_stream_tools.rs) |
| XML config | `XmlConfig`, `XmlNode` | [`mod_config_xml.rs`](crates/fswtch/examples/mod_config_xml.rs) |

cJSON is intentionally not wrapped; use `serde_json` from modules. The raw `fswtch-sys` types are an internal (`pub(crate)`) implementation detail and are not part of the public API.

## Build

Default builds use the bundled FreeSWITCH headers from `fswtch-src`:

```sh
cargo check -p fswtch-sys
cargo check -p fswtch --examples
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets
```

The default `bundled` feature only generates Rust bindings from packaged headers. It does not compile or statically link FreeSWITCH.

To generate bindings from a configured local FreeSWITCH install:

```sh
FREESWITCH_INCLUDE_DIR=/usr/include/freeswitch \
  cargo check -p fswtch-sys --no-default-features --features bindgen
```

If link metadata is not available through `pkg-config`, set the library directory explicitly:

```sh
FREESWITCH_LIB_DIR=/usr/lib/freeswitch cargo build
```

Set `FREESWITCH_NO_PKG_CONFIG=1` to disable `pkg-config` probing.

## Module Shape

A minimal module exports a FreeSWITCH load callback:

```rust
fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}
```

Use `module_load!` to create the typed load callback and register one or more APIs:

```rust
fswtch::module_load! {
    fn switch_module_load(module) for "mod_hello" {
        fswtch::log_info("mod_hello", "loading module");
        module.api(
            "fswtch_hello",
            "prints a Rust greeting",
            "fswtch_hello",
            hello_api,
        )
    }
}
```

Examples use `fswtch::log_info` and `fswtch::log_error`, which route through FreeSWITCH logging.

## Examples

All examples live in `crates/fswtch/examples` and are compiled as FreeSWITCH modules.

Basic module and API patterns:

- `mod_hello`: minimal API command.
- `mod_api_suite`: multiple API commands in one module.
- `mod_stream_tools`: stream responses and command argument parsing.
- `mod_lifecycle`: load, runtime, and shutdown callbacks.

Operational and integration patterns:

- `mod_async_job_queue`: background worker queue with bounded result history.
- `mod_event_sink`: JSON-to-custom-event bridge.
- `mod_http_webhook`: queued plain HTTP webhook delivery.
- `mod_registration_check`: async registration validation and custom event emission.
- `mod_rate_limiter`: token-bucket style API rate limiting with bounded cardinality.
- `mod_metrics`: Prometheus-style metrics output with bounded cardinality.
- `mod_config_xml`: FreeSWITCH XML config loading and reload.
- `mod_cdr_enricher`: CDR JSON enrichment and custom event emission.

FreeSWITCH interface skeletons:

- `mod_app_playback_control`: dialplan application interface that answers and plays a supplied target.
- `mod_media_bug_meter`: media bug application that counts observed read/write-stream audio frames.
- `mod_endpoint_skeleton`: endpoint interface registration skeleton.
- `mod_chatbot_bridge`: chat application interface that emits chatbot bridge events.

AI and media integration:

- `mod_remote_vad`: async websocket VAD worker with custom event reporting.
- `mod_local_ai_bridge`: local ASR/TTS integration boundary plus OpenAI Responses API NLP calls.

## Local AI Example

`mod_local_ai_bridge` exposes:

- `fswtch_local_ai_status`
- `fswtch_local_asr <pcm16le-file>`
- `fswtch_local_tts <text>`
- `fswtch_local_nlp <prompt>`
- `fswtch_local_nlp_sync <prompt>`

Environment variables:

- `FSWTCH_ASR_ONNX`: local ASR ONNX model path.
- `FSWTCH_TTS_ONNX`: local TTS ONNX model path.
- `OPENAI_API_KEY`: enables OpenAI NLP calls.
- `OPENAI_MODEL`: defaults to `gpt-5.1`.
- `OPENAI_BASE_URL`: defaults to `https://api.openai.com/v1`.
- `FSWTCH_AI_ALLOW_MOCK=1`: allows smoke-test fallback behavior when models or API credentials are absent.

For production, do not set `FSWTCH_AI_ALLOW_MOCK`; provide real model paths and API credentials. The example isolates the ORT boundary, but real ASR/TTS inference still needs the tensor contracts for the chosen ONNX models.

## Production Notes

The examples are production-oriented examples, not drop-in production services. Before deploying a module, review:

- Session lifetime and locking for any work that touches a live `switch_core_session_t` outside the original callback.
- Backpressure and queue limits for background work.
- Timeout and retry policy for network integrations.
- Secret handling for API keys and webhook credentials.
- Cardinality limits for metrics, rate limiters, and per-call state.
- Cleanup and ownership rules for FreeSWITCH events, media bugs, XML roots, and allocated user data.
- Real model initialization and tensor validation for ORT-backed ASR/TTS.

Unsafe blocks are kept small and local to FFI operations. Public unsafe APIs in the wrapper should document a `# Safety` contract.

## Repository Layout

- `crates/fswtch`: wrapper API and compile-checked Rust module examples.
- `crates/fswtch-sys`: raw generated FreeSWITCH bindings and bindgen build script.
- `crates/fswtch-src`: packaged FreeSWITCH headers.

The vendored FreeSWITCH trees are third-party inputs. Avoid reformatting or refactoring them unless intentionally updating vendored FreeSWITCH content.
