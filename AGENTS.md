# AGENTS.md

## Scope

These instructions apply to the whole repository.

This is a Rust workspace for FreeSWITCH module bindings. The maintained code is primarily under:

- `crates/fswtch`: safe-ish helper API and compile-checked Rust module examples.
- `crates/fswtch-sys`: raw FreeSWITCH ABI bindings and bindgen build script.
- `crates/fswtch-src`: packaged FreeSWITCH headers used by default bundled builds.
- `docker/fswtch`: smoke-test configuration and verification script.
- `Dockerfile`: full FreeSWITCH smoke-test image.

The root `freeswitch/` tree and `crates/fswtch-src/freeswitch/` contain vendored upstream FreeSWITCH sources/headers. Treat them as third-party inputs. Do not reformat or refactor vendored files unless the task explicitly requires changing vendored FreeSWITCH content.

## Build And Check Commands

Prefer focused checks while developing:

```sh
cargo check -p fswtch-sys
cargo check -p fswtch
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets
```

For bindings against a local configured FreeSWITCH install:

```sh
FREESWITCH_INCLUDE_DIR=/usr/include/freeswitch cargo check -p fswtch-sys --no-default-features --features bindgen
```

If link metadata is not available through `pkg-config`, use:

```sh
FREESWITCH_LIB_DIR=/usr/lib/freeswitch cargo build
```

The end-to-end smoke test is Docker/Podman based and can be slow because it builds FreeSWITCH:

```sh
docker build -t fswtch-freeswitch-smoke .
docker run --rm fswtch-freeswitch-smoke
```

Successful smoke output ends with:

```text
all fswtch example module checks passed
```

## Coding Guidelines

- Keep Rust code formatted with `cargo fmt`.
- Preserve workspace lints in `Cargo.toml`: `unsafe_op_in_unsafe_fn = "deny"` and Clippy `missing_safety_doc = "deny"`.
- For public unsafe functions, document the safety contract with a `# Safety` section.
- Keep unsafe blocks small and local to the FFI operation they justify.
- Prefer `NonNull`, `CStr`, and explicit status conversion helpers over unchecked raw pointer handling in the higher-level `fswtch` crate.
- Do not hand-edit generated bindgen output in `OUT_DIR`; update `crates/fswtch-sys/build.rs` or the relevant headers instead.
- Avoid broad allowlists in bindgen unless the public ABI surface intentionally expands.

## FreeSWITCH Binding Notes

- Default builds use the `bundled` feature, which generates bindings from headers packaged by `fswtch-src`.
- The bundled feature does not compile or statically link FreeSWITCH itself.
- `FREESWITCH_INCLUDE_DIR` must point at configured FreeSWITCH headers when using non-bundled bindgen; the build script expects `switch_am_config.h` to exist there.
- `FREESWITCH_NO_PKG_CONFIG=1` disables `pkg-config` probing in `fswtch-sys`.

## Examples And Smoke Coverage

The `crates/fswtch/examples/*.rs` files are compiled as `cdylib` FreeSWITCH modules. When adding or changing example module behavior, update `docker/fswtch/bin/verify-fswtch-examples` so the Docker smoke test verifies the expected `fs_cli` API response.

Keep Docker config minimal. The smoke image intentionally enables only the FreeSWITCH modules required for the Rust examples and event socket verification.

## Publishing

Publish crates in dependency order:

```sh
cargo publish -p fswtch-src
cargo publish -p fswtch-sys
cargo publish -p fswtch
```

Before publishing, run the focused Rust checks and, when behavior changed, the Docker smoke test.
