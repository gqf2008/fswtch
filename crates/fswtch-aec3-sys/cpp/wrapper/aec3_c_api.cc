#include "aec3_c_api.h"

#include "common_audio/third_party/ooura/fft_size_128/ooura_fft.h"

int32_t fswtch_aec3_api_version(void) {
    return 1;
}

int32_t fswtch_aec3_ooura_smoke(void) {
    // Scalar mode: `sse2_available=false` resolves to the portable C path on non-x86/non-NEON
    // builds (see the dispatch in ooura_fft.cc). One forward FFT over a zero buffer exercises the
    // vendored closure end to end without touching cpu_info or any SIMD symbol.
    webrtc::OouraFft fft(false);
    float buf[128] = {};
    fft.Fft(buf);
    return 1;
}
