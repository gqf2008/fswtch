// fswtch-apm shim: common_audio/signal_processing/include/signal_processing_library.h
//
// The real header aggregates the entire WebRTC signal-processing API (~1600
// lines + many transitive headers). The vendored AEC3 closure only calls two
// functions from it (via splitting_filter.cc): WebRtcSpl_AnalysisQMF and
// WebRtcSpl_SynthesisQMF, the 2-band quadrature-mirror split/combine used for
// 16 kHz↔(8+8) band processing. This shim declares+defines those two (inline,
// so no separate .cc is needed and there are no undefined references) with a
// trivial even/odd deinterleave/interleave in place of the real all-pass QMF
// filter. The AEC3 48 kHz path uses three_band_filter_bank (real, untouched);
// the 2-band path is not exercised by the Phase 2 smoke tests.

#ifndef COMMON_AUDIO_SIGNAL_PROCESSING_INCLUDE_SIGNAL_PROCESSING_LIBRARY_H_
#define COMMON_AUDIO_SIGNAL_PROCESSING_INCLUDE_SIGNAL_PROCESSING_LIBRARY_H_

#include <cstddef>
#include <cstdint>

namespace webrtc {

inline void WebRtcSpl_AnalysisQMF(const float* in_data,
                                  size_t in_data_length,
                                  float* low_band,
                                  float* high_band,
                                  float* filter_state1,
                                  float* filter_state2) {
  const size_t band_length = in_data_length / 2;
  for (size_t i = 0; i < band_length; ++i) {
    low_band[i] = in_data[2 * i];
    high_band[i] = in_data[2 * i + 1];
  }
}

inline void WebRtcSpl_SynthesisQMF(const float* low_band,
                                   const float* high_band,
                                   size_t band_length,
                                   float* out_data,
                                   float* filter_state1,
                                   float* filter_state2) {
  for (size_t i = 0; i < band_length; ++i) {
    out_data[2 * i] = low_band[i];
    out_data[2 * i + 1] = high_band[i];
  }
}

}  // namespace webrtc

#endif  // COMMON_AUDIO_SIGNAL_PROCESSING_INCLUDE_SIGNAL_PROCESSING_LIBRARY_H_
