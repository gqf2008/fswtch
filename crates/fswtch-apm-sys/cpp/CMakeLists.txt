cmake_minimum_required(VERSION 3.16)
project(fswtch_apm LANGUAGES CXX C)

# C++20: the vendored WebRTC AEC3 code uses std::span / <numbers> (C++20).
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# ---------------------------------------------------------------------------
# Scalar build policy:
#  * Do NOT define WEBRTC_ENABLE_AVX2, WEBRTC_HAS_NEON, or MIPS_FPU_LE.
#  * rtc_base/system/arch.h defines only WEBRTC_ARCH_ARM_FAMILY on aarch64
#    (not WEBRTC_ARCH_X86_FAMILY, not WEBRTC_HAS_NEON), so the AEC3 SIMD
#    dispatch (`#if defined(WEBRTC_ARCH_X86_FAMILY)` / `WEBRTC_HAS_NEON`) is
#    excluded and the portable C++ scalar paths are compiled.
#  * No *_avx2.cc / *_neon.cc are compiled (none are vendored).
#  * WEBRTC_APM_DEBUG_DUMP=0 disables ApmDataDumper's WavWriter/raw-file dump
#    path, so common_audio/wav_file.h is never needed.
#  * The neural residual echo estimator implementation is excluded entirely
#    (not vendored); the abstract header api/audio/neural_residual_echo_estimator.h
#    is. The C wrapper (Phase 3) passes nullptr, selecting the traditional path.
# ---------------------------------------------------------------------------

# All vendored AEC3 DSP translation units (57 non-test .cc). Globbed; any
# *_unittest.cc / *_avx2.cc / *_neon.cc are filtered out defensively.
file(GLOB AEC3_SRCS CONFIGURE_DEPENDS
    ${CMAKE_CURRENT_SOURCE_DIR}/modules/audio_processing/aec3/*.cc
)
list(FILTER AEC3_SRCS EXCLUDE REGEX "_unittest\\.cc$")
list(FILTER AEC3_SRCS EXCLUDE REGEX "_avx2\\.cc$")
list(FILTER AEC3_SRCS EXCLUDE REGEX "_neon\\.cc$")
# Exclude macOS AppleDouble resource-fork files (._*) — they match *.cc but are binary.
list(FILTER AEC3_SRCS EXCLUDE REGEX "/\\._")

# Vendored noise-suppressor translation units (spectral NS, no neural).
file(GLOB NS_SRCS CONFIGURE_DEPENDS
    ${CMAKE_CURRENT_SOURCE_DIR}/modules/audio_processing/ns/*.cc
)
list(FILTER NS_SRCS EXCLUDE REGEX "_unittest\\.cc$")
list(FILTER NS_SRCS EXCLUDE REGEX "/\\._")

# Real APM helper translation units (kept verbatim) + the Ooura 128-FFT and the
# newly-vendored ChannelBuffer container, plus the C wrapper.
# AGC2 (scalar: fixed digital gain + limiter, adaptive/RNN disabled) — the four
# leaf translation units of GainController2's scalar path. gain_controller2.cc
# itself is intentionally NOT compiled (see wrapper/agc2_c_api.cc): its ctor and
# Process() contain runtime if-blocks referencing the adaptive/RNN classes.
set(AEC3_HELPER_SRCS
    wrapper/aec3_c_api.cc
    wrapper/agc2_c_api.cc
    wrapper/hpf_c_api.cc
    wrapper/ns_c_api.cc
    common_audio/third_party/ooura/fft_size_128/ooura_fft.cc
    common_audio/third_party/ooura/fft_size_256/fft4g.cc
    common_audio/channel_buffer.cc
    common_audio/audio_util.cc
    modules/audio_processing/audio_buffer.cc
    modules/audio_processing/high_pass_filter.cc
    modules/audio_processing/splitting_filter.cc
    modules/audio_processing/three_band_filter_bank.cc
    modules/audio_processing/logging/apm_data_dumper.cc
    modules/audio_processing/utility/cascaded_biquad_filter.cc
    api/audio/echo_canceller3_config.cc
    modules/audio_processing/agc2/gain_applier.cc
    modules/audio_processing/agc2/limiter.cc
    modules/audio_processing/agc2/fixed_digital_level_estimator.cc
    modules/audio_processing/agc2/interpolated_gain_curve.cc
)

add_library(fswtch_apm STATIC ${AEC3_SRCS} ${NS_SRCS} ${AEC3_HELPER_SRCS})

# Include search order: real vendored tree first, shim tree as fallback. Real
# AEC3/ooura/helper headers win; the shim tree provides the utility layer
# (rtc_base/absl/system_wrappers) and the two minimal fan-out roots
# (api/audio/audio_processing.h, api/environment/environment.h).
target_include_directories(fswtch_apm PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/shims
)

# Scalar / no-debug-dump / no-neural defines.
target_compile_definitions(fswtch_apm PRIVATE
    WEBRTC_APM_DEBUG_DUMP=0
)
# Release builds get NDEBUG (disables RTC_DCHECK).
target_compile_definitions(fswtch_apm PRIVATE $<$<CONFIG:Release>:NDEBUG>)

# The `cmake` crate runs `cmake --build . --target install` with
# CMAKE_INSTALL_PREFIX set to a path it returns as the link-search dir. Install
# the archive to that prefix root so the static lib lands at
# `<dst>/libfswtch_aec3.a` and `cargo:rustc-link-search=native=<dst>` finds it.
install(TARGETS fswtch_apm ARCHIVE DESTINATION .)
