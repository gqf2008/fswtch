# fswtch

[![Crates.io Version](https://img.shields.io/crates/v/fswtch)](https://crates.io/crates/fswtch)
[![fswtch-sys](https://img.shields.io/crates/v/fswtch-sys?label=fswtch-sys)](https://crates.io/crates/fswtch-sys)

Rust bindings and helper APIs for writing FreeSWITCH modules.

This workspace is intentionally split into three crates:

- `fswtch`: safe-ish helpers for module exports, module interface creation, API command registration, stream writes, status conversion, and example logging.
- `fswtch-sys`: raw FreeSWITCH ABI bindings generated with bindgen.
- `fswtch-src`: packaged FreeSWITCH headers used by default bundled builds.

## Wrapper API

The `fswtch` crate provides a small higher-level layer over the raw FreeSWITCH ABI. It focuses on the parts that every module needs first:

- `module_exports!` declares the exported FreeSWITCH module table.
- `Module::create` builds the loader-owned module interface from the raw load callback arguments.
- `Module::add_api` registers API commands with static names, descriptions, syntax strings, and callbacks.
- `Stream` wraps `switch_stream_handle_t` for byte and string responses.
- `Status`, `SwitchError`, and `status_to_result` convert common FreeSWITCH status handling into Rust `Result` values.
- `LogLevel`, `log`, and convenience helpers such as `log_info`, `log_warning`, `log_error`, and `log_debug1` through `log_debug10` route module logs through FreeSWITCH logging.

The wrapper does not try to hide the full ABI yet. Examples use `fswtch::sys` directly where FreeSWITCH exposes interfaces that still need raw pointer setup, such as dialplan applications, media bugs, endpoint skeletons, chat interfaces, XML config, and custom events. Keep those raw calls narrow, document the callback and ownership assumptions, and prefer adding focused helpers to `fswtch` when the same unsafe pattern appears in more than one module.

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

## Docker Smoke Test

The repository includes a full smoke image that builds FreeSWITCH, builds every Rust example as a `cdylib`, installs the modules, starts FreeSWITCH, and verifies APIs through `fs_cli`.

```sh
docker build -t fswtch-freeswitch-smoke .
docker run --rm fswtch-freeswitch-smoke
```

Podman works too:

```sh
podman build -t fswtch-freeswitch-smoke .
podman run --rm fswtch-freeswitch-smoke
```

Successful output ends with:

```text
all fswtch example module checks passed
```

The smoke script enables `FSWTCH_AI_ALLOW_MOCK=1` so the local AI example can run without model files or OpenAI credentials.

## Module Shape

A minimal module exports a FreeSWITCH load callback:

```rust
fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}
```

Inside `switch_module_load`, create a `Module` from FreeSWITCH's raw load arguments, then register one or more APIs:

```rust
let module = match unsafe { fswtch::Module::create(module_interface, pool, c"mod_hello") } {
    Ok(module) => module,
    Err(error) => return error.0,
};

if let Err(error) = unsafe {
    module.add_api(
        c"rust_hello",
        c"prints a Rust greeting",
        c"rust_hello",
        hello_api,
    )
} {
    return error.0;
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
- `mod_media_bug_meter`: media bug application that counts observed read-stream audio frames.
- `mod_endpoint_skeleton`: endpoint interface registration skeleton.
- `mod_chatbot_bridge`: chat application interface that emits chatbot bridge events.

AI and media integration:

- `mod_remote_vad`: async websocket VAD worker with custom event reporting.
- `mod_local_ai_bridge`: local ASR/TTS integration boundary plus OpenAI Responses API NLP calls.

## Local AI Example

`mod_local_ai_bridge` exposes:

- `rust_local_ai_status`
- `rust_local_asr <pcm16le-file>`
- `rust_local_tts <text>`
- `rust_local_nlp <prompt>`
- `rust_local_nlp_sync <prompt>`

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
- `docker/fswtch`: smoke-test FreeSWITCH config and verification script.
- `Dockerfile`: full FreeSWITCH smoke-test image.
- `freeswitch/`: vendored upstream FreeSWITCH source context.

The vendored FreeSWITCH trees are third-party inputs. Avoid reformatting or refactoring them unless intentionally updating vendored FreeSWITCH content.

## Publishing

Publish crates in dependency order:

```sh
cargo publish -p fswtch-src
cargo publish -p fswtch-sys
cargo publish -p fswtch
```

Before publishing, run the focused Rust checks above and the Docker smoke test when example behavior changed.
