# fswtch

[![Crates.io Version](https://img.shields.io/crates/v/fswtch)](https://crates.io/crates/fswtch)
[![Crates.io Version](https://img.shields.io/crates/v/fswtch-sys)](https://crates.io/crates/fswtch-sys)

Rust bindings for writing FreeSWITCH modules.

This workspace is split into three crates:

- `fswtch-sys`: raw FreeSWITCH ABI bindings. By default it enables `bundled`, runs bindgen, and generates bindings from the packaged FreeSWITCH headers.
- `fswtch`: higher-level helpers for module exports, module interface creation, API command registration, and stream writes.
- `fswtch-src`: vendored FreeSWITCH headers used by `fswtch-sys` for default bundled bindgen builds.

## Header Discovery

By default, `fswtch` and `fswtch-sys` generate bindings from the vendored headers:

```sh
cargo check -p fswtch-sys
cargo check -p fswtch
```

This default `bundled` feature is for generating Rust bindings from the vendored headers. It does not compile or statically link FreeSWITCH itself.

To generate bindings from a configured FreeSWITCH installation instead, disable default features and enable `bindgen`:

```sh
FREESWITCH_INCLUDE_DIR=/usr/include/freeswitch cargo check -p fswtch-sys --no-default-features --features bindgen
```

If FreeSWITCH is installed with `pkg-config`, `fswtch-sys` will also try the `freeswitch` package for link metadata. You can override linking with:

```sh
FREESWITCH_LIB_DIR=/usr/lib/freeswitch cargo build
```

The vendored `freeswitch/` tree is useful source context, but it does not include generated config headers until FreeSWITCH has been configured.

## Docker Smoke Test

The repository includes a Docker image that builds FreeSWITCH, builds the Rust example modules, installs them into the FreeSWITCH module directory, starts FreeSWITCH, and verifies the module APIs through `fs_cli`.

```sh
docker build -t fswtch-freeswitch-smoke .
docker run --rm fswtch-freeswitch-smoke
```

If you use Podman:

```sh
podman build -t fswtch-freeswitch-smoke .
podman run --rm fswtch-freeswitch-smoke
```

Successful output ends with:

```text
all fswtch example module checks passed
```

## Publishing

Publish crates in dependency order:

```sh
cargo publish -p fswtch-src
cargo publish -p fswtch-sys
cargo publish -p fswtch
```

For dry-runs before the first upload of a new version, `fswtch-sys` and `fswtch` will not fully resolve until their registry dependencies already exist. Start by dry-running and publishing `fswtch-src`.

## Module Skeleton

See [crates/fswtch/examples/mod_hello.rs](crates/fswtch/examples/mod_hello.rs) for the current Rust module shape:

```rust
fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}
```

Inside `switch_module_load`, create a `Module` from the raw FreeSWITCH load arguments and register APIs with `Module::add_api`.

Additional compile-checked examples:

- [mod_api_suite.rs](crates/fswtch/examples/mod_api_suite.rs): registers several API commands from one module.
- [mod_lifecycle.rs](crates/fswtch/examples/mod_lifecycle.rs): exports load, runtime, and shutdown callbacks.
- [mod_registration_check.rs](crates/fswtch/examples/mod_registration_check.rs): queues an asynchronous registration check, parses a pretend JSON response, and fires a custom FreeSWITCH event.
- [mod_remote_vad.rs](crates/fswtch/examples/mod_remote_vad.rs): connects to a remote websocket VAD service, streams party audio frames during a call, parses JSON responses, and emits custom events.
- [mod_stream_tools.rs](crates/fswtch/examples/mod_stream_tools.rs): writes structured responses to FreeSWITCH streams and parses command arguments.
