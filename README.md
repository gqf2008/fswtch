# fswtch

[![Crates.io Version](https://img.shields.io/crates/v/fswtch)](https://crates.io/crates/fswtch)
[![Crates.io Version](https://img.shields.io/crates/v/fswtch-sys)](https://crates.io/crates/fswtch-sys)

Rust bindings for writing FreeSWITCH modules.

This workspace is split into two crates:

- `fswtch-sys`: raw FreeSWITCH ABI bindings. By default it exposes a small handwritten module ABI that builds without a configured FreeSWITCH source tree. Enable the `bindgen` feature to generate broader bindings from `switch.h`.
- `fswtch`: higher-level helpers for module exports, module interface creation, API command registration, and stream writes.
- `fswtch-src`: vendored FreeSWITCH headers used by `fswtch-sys` when the `bundled` feature is enabled.

## Header Discovery

For generated bindings, point the build at configured FreeSWITCH headers:

```sh
FREESWITCH_INCLUDE_DIR=/usr/include/freeswitch cargo check -p fswtch-sys --features bindgen
```

If FreeSWITCH is installed with `pkg-config`, `fswtch-sys` will also try the `freeswitch` package for link metadata. You can override linking with:

```sh
FREESWITCH_LIB_DIR=/usr/lib/freeswitch cargo build
```

The vendored `freeswitch/` tree is useful source context, but it does not include generated config headers until FreeSWITCH has been configured.

The `bundled` feature enables bindgen and points it at the packaged `fswtch-src` vendored headers:

```sh
cargo check -p fswtch-sys --features bundled
cargo check -p fswtch --features bundled
```

This feature is for generating Rust bindings from the vendored headers. It does not compile or statically link FreeSWITCH itself.

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
