# fswtch-apm porting record

Documents the FFI-bridge port of WebRTC AEC3 into the `fswtch` Rust workspace. Audience: auditors
checking the port + future maintainers tracing the build decisions.

## Source

- Upstream: WebRTC AEC3 from `maitrungduc1410/webrtc` (a full GN-only mirror of official WebRTC).
- Path: `modules/audio_processing/aec3/` (the DSP) + `common_audio/third_party/ooura/fft_size_128/`
  (the 128-pt FFT) + a small set of real `modules/audio_processing/` + `common_audio/` helpers.
- The fork is GN-only (no standalone CMake); it depends on the Chromium `build/` tree via
  `gclient sync`. This port does **not** use GN — it builds a curated closure with CMake.

## Strategy: FFI bridge (hybrid shim)

Per the user's decision, the AEC3 **DSP is kept real** (vendored verbatim, zero divergence from
upstream) and only the **non-DSP utility layer** is shimmed (minimal `std`/`atomic`/`fmt`
reimplementations). This matches the proven standalone-AEC3 approach (e.g. `shichaog/WebRTC-audio-
processing`) and keeps the closure small + self-contained (no `depot_tools`, no abseil build, no
protobuf).

```
fswtch-apm-sys/cpp/
├── CMakeLists.txt            # builds the static lib; include order [cpp/, cpp/shims/]
├── wrapper/aec3_c_api.{h,cc} # thin C ABI over webrtc::EchoCanceller3 + AudioBuffer
├── modules/audio_processing/aec3/   # REAL: 57 .cc + 62 .h (DSP, verbatim)
├── common_audio/third_party/ooura/fft_size_128/  # REAL: Ooura 128-FFT (scalar)
├── common_audio/{channel_buffer,audio_util,include/audio_util}.cc/.h + api/audio/audio_view.h  # REAL: self-contained containers
├── modules/audio_processing/{audio_buffer,high_pass_filter,splitting_filter,three_band_filter_bank,render_queue_item_verifier,logging/apm_data_dumper,utility/cascaded_biquad_filter}.{cc,h} + api/audio/{echo_canceller3_config,echo_control,neural_residual_echo_estimator}.h/.cc + api/environment/environment.h  # REAL APM helpers
├── rtc_base/system/{arch.h, ...}     # REAL (small): arch.h, cpu_info.h (header-only)
└── shims/                            # MINIMAL reimplementation of the utility layer
    ├── rtc_base/{checks,logging,swap_queue,race_checker,thread_annotations,gtest_prod_util,string_utils,strings/string_builder,numerics/safe_minmax,experiments/field_trial_parser,system/{inline,rtc_export,unused}}.h
    ├── absl/{strings/string_view,base/nullability,algorithm/container}.h
    ├── api/{field_trials_view,audio/audio_processing,environment/environment}.h   # minimal roots
    ├── system_wrappers/include/metrics.h
    └── modules/audio_processing/capture_mixer/capture_mixer.h + common_audio/{resampler/push_sinc_resampler,signal_processing/include/signal_processing_library}.h  # functional stubs
```

Include search order is `[cpp/, cpp/shims/]`: real vendored headers win; the shim tree is the
fallback for the utility layer. Each shim mirrors the upstream include path so no vendored file's
`#include`s are edited.

## Key decisions

- **C++ standard 17 → 20.** The vendored AEC3 `.cc` use `std::span` / `<numbers>` (C++20). Staying
  at 17 would require editing real DSP files (out of policy). Debian bookworm's g++ 12 supports it.
- **`WEBRTC_APM_DEBUG_DUMP=0`.** Disables `ApmDataDumper`'s WavWriter/raw-file dump path, so
  `common_audio/wav_file.h` is never needed (no shim).
- **Neural residual echo estimator skipped.** The `aec3/neural_residual_echo_estimator/` impl
  (protobuf + model runtime) is not vendored. `EchoCanceller3` is constructed with
  `NeuralResidualEchoEstimator* = nullptr`, selecting the traditional residual-echo path. The
  abstract header `api/audio/neural_residual_echo_estimator.h` is protobuf-free and vendored real.
- **Scalar build.** `WEBRTC_ENABLE_AVX2` / `WEBRTC_HAS_NEON` / `MIPS_FPU_LE` are left undefined;
  on aarch64 `arch.h` defines only `WEBRTC_ARCH_ARM_FAMILY`, so the SIMD dispatch (`#if
  defined(WEBRTC_ARCH_X86_FAMILY)` / `WEBRTC_HAS_NEON`) is excluded and the portable C++ scalar
  paths compile. `*_avx2.cc` / `*_neon.cc` are not vendored.
- **Minimal root shims (fan-out elimination).** The real `api/audio/audio_processing.h` (36 KB)
  and `api/environment/environment.h` are replaced by minimal shims providing only what the DSP
  uses (`StreamConfig` + `AudioProcessing::Config::Pipeline::DownmixMethod`; `Environment` holding
  a `const FieldTrialsView&` with `field_trials()`). This removed the need for `ref_count`,
  `scoped_refptr`, `task_queue_*`, `rtc_event_log`, `clock`, `audio_processing_statistics`.
- **CMake `install(TARGETS fswtch_apm ARCHIVE DESTINATION .)`.** The `cmake` crate runs
  `cmake --build . --target install` with `CMAKE_INSTALL_PREFIX` = the path it returns as the
  link-search dir; installing the archive to the prefix root makes `cargo:rustc-link-search=<dst>`
  find `libfswtch_apm.a`.
- **`SplitIntoFrequencyBands` / `MergeFrequencyBands` guarded on `num_bands() > 1`.** `AudioBuffer`
  only creates `splitting_filter_` when `num_bands > 1`, but `SplitIntoFrequencyBands()`
  unconditionally derefs it → null deref (SIGSEGV) at 16 kHz / 1 band. The real WebRTC APM never
  calls split at 1 band (AEC3 reads `data_` via the `split_bands_const` fallback); the C wrapper
  matches that. This was the one real bug found in Phase 3.
- **`common_audio/audio_util.cc` vendored** in Phase 3 — `audio_buffer.cc::CopyFrom/CopyTo`
  reference `S16ToFloatS16`/`FloatS16ToS16` (array versions, defined in `audio_util.cc`). Static-
  lib creation tolerates the undefined symbol; it only surfaced at the final test-binary link.

## Functional stubs (low risk; revisit if needed)

`common_audio/resampler/push_sinc_resampler.h` and the QMF
`common_audio/signal_processing/include/signal_processing_library.h` (`WebRtcSpl_AnalysisQMF` /
`SynthesisQMF` / etc.) are functional stubs (copy / interleave). They are sufficient for
compile/link/smoke. At **matched rates** the resampler is a 1:1 copy (correct); **16 kHz** (1
band) and **48 kHz** (real `three_band_filter_bank`) avoid QMF entirely. **32 kHz** (2-band QMF)
is not supported until these are replaced with the real implementations.

## Verification (Phase 5)

`cargo test -p fswtch-apm --lib ::cancels_a_real_echo` feeds deterministic broadband-noise render
+ a delayed (64-sample / 4 ms) echo as capture for 3 s (1.5 s warmup) and asserts post-convergence
ERLE > 15 dB. Observed **~67 dB** on aarch64 (pure echo, no near-end → near-full cancellation).
A broken wrapper (out-of-sync render/capture) would stay ~0 dB. Cross-platform variance is large
(67 dB observed → >15 dB bar is safe).

## Caveats / open items

- **x86_64 SSE2.** On x86_64 `arch.h` defines `WEBRTC_ARCH_X86_FAMILY`; ooura's SSE2 path
  (`cft1st_128_SSE2` etc., defined in `ooura_fft_sse2.cc`) would be referenced but is not vendored
  → link error. The Docker smoke runs `linux/arm64` on this host (no x86 issue); for x86_64 either
  vendor `ooura_fft_sse2.cc` + `ooura_fft_tables_neon_sse2.h` or force-scalar via an `arch.h`
  shim.
- **macOS version-mismatch link warnings** — fixed (review #6): `CMakeLists.txt` sets
  `CMAKE_OSX_DEPLOYMENT_TARGET "11.0"` on APPLE so cmake objects target the same macOS as the
  final Rust link; the "object built for newer macOS" warnings are gone.
- **Docker** — removed entirely (review #1/#3): the AGENTS.md Docker convention, the `Dockerfile`
  + `docker/fswtch/` infra (cmake apt + mod_aec3/mod_apm build + `modules.conf.xml` autoload +
  `verify-fswtch-examples`), and the README Docker section are all gone. APM modules are
  verified locally in the running FreeSWITCH (`mod_aec3`→`aec3 ok erle=67.2db`,
  `mod_apm`→`apm ok erle=58.2db`) — see `USAGE.md`.

## Phase summary (commits on `feat/aec3`)

| Phase | Commit | What |
|-------|--------|------|
| 0 | `6f52ce4` | Scaffold `fswtch-apm-sys` + `fswtch-apm`; cmake→bindgen→Rust pipeline; smoke. |
| 1 | `e6ec062` | Vendored Ooura 128-FFT (scalar); closure method proven (no abseil/logging). |
| 2 | `d1803c9` | AEC3 closure converged (hybrid shim + real DSP); `libfswtch_apm.a` links. |
| 3 | `91c61c8` | Thin C ABI (`create`/`analyze_render`/`process_capture`/…/`destroy`); split/merge guard fix; real AEC3 runs. |
| 4 | `ca75cf2` | Safe Rust wrapper (`NonNull`/`Drop`/`# Safety`) + 9 unit tests. |
| 5 | `df4afc2` | Functional equivalence test (`cancels_a_real_echo`, ~67 dB ERLE). |
| 6 | `d7b253e` | `mod_aec3` FreeSWITCH example + Docker wiring (dev-verified; full smoke pending). |
