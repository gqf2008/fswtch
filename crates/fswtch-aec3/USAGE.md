# fswtch-aec3

Safe Rust bindings over WebRTC **AEC3** (acoustic echo cancellation), built as an FFI bridge over
the vendored WebRTC C++ and wrapped in the `fswtch` FFI style (`NonNull` + `Drop` + `# Safety`).

> Echo cancellation removes the far-end (loudspeaker) signal that leaks into the near-end
> microphone. Feed what the caller hears (`render`) and what the mic picks up (`capture`); AEC3
> returns the cleaned `capture`.

## Crate layout

| Crate | Role |
|-------|------|
| `fswtch-aec3-sys` | Builds the vendored AEC3 C++ (`crates/fswtch-aec3-sys/cpp/`) via CMake into `libfswtch_aec3.a`, runs `bindgen` on the thin C ABI in `cpp/wrapper/aec3_c_api.h`, and links the C++ runtime. Raw `extern "C"` bindings live here. |
| `fswtch-aec3` | Safe, owned wrapper ([`EchoCanceller3`](crate::EchoCanceller3)) over the raw ABI. This is the crate to depend on. |

The build is **scalar / portable** (no AVX2/NEON), **neural residual echo estimator disabled**, and
the **default AEC3 config** is used (no per-field `Config` builder yet).

## Quick start

```rust
use fswtch_aec3::{EchoCanceller3, Aec3Error};

const RATE: i32 = 16_000;       // recommended: 16 kHz / 1 band
const CH:   usize = 1;         // mono
const FRAME: usize = (RATE as usize) / 100 * CH; // 160 samples = one 10 ms frame

let mut aec = EchoCanceller3::new(RATE, CH, CH)?;   // default config, neural off

// Per 10 ms tick (e.g. FreeSWITCH media-bug callback at 50 Hz on 20 ms frames → two ticks):
aec.analyze_render(&render_frame, CH)?;             // far-end (loudspeaker)
aec.process_capture(&mut capture_frame, CH, false)?; // near-end (mic) — echo removed in place
// capture_frame now holds the de-echoed mic signal.
```

`render_frame` / `capture_frame` are interleaved `i16` (FreeSWITCH `SLIN16`) of exactly
`rate/100 * num_channels` samples. Rust validates the length + channel count **before** crossing
the FFI boundary (the C ABI only sees a raw pointer).

## Key concepts

- **One 10 ms frame per call.** AEC3 processes `sample_rate_hz / 100` samples per channel per call.
  FreeSWITCH hands 20 ms SLIN frames — split into two 10 ms calls (see `mod_aec3` example).
- **`render` = far-end** (audio written to the channel = played to the caller = the echo source).
  **`capture` = near-end** (mic). `process_capture` modifies `capture` in place.
- **`level_change`**: set `true` when the capture gain is known to have changed since the last
  frame (toggles AEC3's filter-divergence protection). `false` is correct for steady gain.
- **Sample rates:** `8000` / `16000` / `48000` are supported. **`16000` is recommended** (1 band →
  no band splitting; the QMF/resampler stubs are never exercised). `48000` uses the real
  `three_band_filter_bank`. **`32000` is not supported yet** (2-band QMF is a stub — see
  porting-docs).
- **Concurrency:** `AnalyzeRender` is the only method safe to call concurrently with the capture
  side. All capture-side calls (`process_capture`, `set_delay`, …) must be serialized by the
  caller — `EchoCanceller3` is `!Send` / `!Sync`.
- **Reset:** there is no `reset()`; destroy + recreate the handle.

## Errors

[`Aec3Error`](crate::Aec3Error) covers `InvalidArg`, `ChannelMismatch`, `InvalidFrameLength`,
`Exception` (a C++ exception crossed the FFI boundary), `CreateFailed`, and `Unknown(code)`.
The C ABI's status codes (`0`/`1`/`2`/`-1`) map onto these.

## FreeSWITCH integration (`mod_aec3`)

The `mod_aec3` example (`crates/fswtch-aec3/examples/mod_aec3.rs`) is a loadable FreeSWITCH
`cdylib` module exposing:

- **`rust_aec3_smoke` API** — runs the real AEC3 on a synthetic echo in-process and prints
  `aec3 ok rate=16000 erle=<dB>`. Proves the module loads + the AEC3 C++ links/runs inside the
  FreeSWITCH process. (Asserted by the Docker smoke.)
- **`rust_aec3` dialplan application** — attaches a media bug (`WRITE_STREAM` = render,
  `READ_REPLACE` = capture), splits 20 ms SLIN into 10 ms AEC3 ticks, and writes the de-echoed
  capture back. Lazily creates the canceller at the first frame's rate; any error or unsupported
  rate falls through to passthrough so a call is never crashed.

Build it where a FreeSWITCH lib is available (`FREESWITCH_LIB_DIR` / pkg-config, or the Docker
smoke image):

```sh
FREESWITCH_INCLUDE_DIR=/usr/include/freeswitch \
FREESWITCH_LIB_DIR=/usr/lib/freeswitch \
  cargo build -p fswtch-aec3 --example mod_aec3 --release
# → target/release/examples/libmod_aec3.so  (copy to FreeSWITCH's mod/ dir, autoload, load)
```

Docker smoke (builds FreeSWITCH + the example + runs the API checks):

```sh
docker build -t fswtch-freeswitch-smoke .
docker run --rm fswtch-freeswitch-smoke     # ends with: all fswtch example module checks passed
```

## Build from a non-bundled FreeSWITCH

`fswtch-aec3-sys` only needs the AEC3 C++ tree (vendored under `cpp/`); it does **not** depend on
FreeSWITCH. The `mod_aec3` *example* additionally links FreeSWITCH via `fswtch`/`fswtch-sys` —
provide `FREESWITCH_INCLUDE_DIR` + `FREESWITCH_LIB_DIR` (or pkg-config) for it.

## Known limitations

- **Scalar only** — AVX2/NEON SIMD paths are not compiled (portable C++ scalar). On x86_64 the
  ooura SSE2 path (`ooura_fft_sse2.cc`) is not vendored; the scalar path is used. SIMD enablement
  is deferred.
- **Neural residual echo estimator disabled** — the constructor receives `nullptr`, selecting the
  traditional residual-echo-estimator path (no protobuf / model runtime needed).
- **`32000` Hz unsupported** — the 2-band QMF split (`WebRtcSpl_AnalysisQMF`) is a functional stub
  (copy). Use `16000` (default) or `48000`.
- **No `Config` builder yet** — `EchoCanceller3::new` uses the default `EchoCanceller3Config`.
  Field-level tuning (`EchoCanceller3Config::filter.delay.headroom_samples`, etc.) is a future
  addition.
- **`!Send` / `!Sync`** — capture-side calls must be serialized (one thread per handle).

## Verification

- `cargo test -p fswtch-aec3 --lib` — 9 unit tests, incl. `cancels_a_real_echo` (a synthetic
  broadband-noise echo is cancelled to **~67 dB ERLE** on aarch64, proving the wrapper feeds
  render/capture in sync).
- `cargo check --example mod_aec3` — type-checks the FreeSWITCH module without linking.
- The full Docker smoke (above) is the end-to-end gate (pending a running Docker daemon in this
  environment).

See `porting-docs/porting-record.md` for the FFI/shim strategy, vendored-file manifest, and the
key build decisions (C++20, `WEBRTC_APM_DEBUG_DUMP=0`, the split/merge guard fix, etc.).
