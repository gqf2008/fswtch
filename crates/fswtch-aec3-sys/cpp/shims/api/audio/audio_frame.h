// fswtch-aec3 shim: api/audio/audio_frame.h (stub)
//
// The real api/audio/audio_frame.h is a large root that drags in
// channel_layout.h, rtp_packet_infos.h, ref_count, etc. The only vendored AGC2
// translation unit that #includes it — agc2/fixed_digital_level_estimator.cc —
// does not reference any AudioFrame symbol (it uses DeinterleavedView from
// audio_view.h only). GainController2.cc (not vendored in the scalar AGC2
// build) likewise #includes it without use. This empty stub lets those includes
// resolve without the heavy fan-out. `SampleRateToDefaultChannelSize()` (the
// one helper the AGC2 wrapper would need) is not declared here: the wrapper
// computes `sample_rate_hz / 100` inline (the exact upstream definition) since
// the AGC2 handle calls the Limiter ctor directly.

#ifndef API_AUDIO_AUDIO_FRAME_H_
#define API_AUDIO_AUDIO_FRAME_H_

#include <cstddef>

namespace webrtc {

// Number of 10 ms audio buffers per second. Used by
// agc2/fixed_digital_level_estimator.cc to convert a sample count back to a
// sample rate for an ApmDataDumper field. Matches the upstream definition in
// api/audio/audio_frame.h (also the divisor in SampleRateToDefaultChannelSize).
constexpr size_t kDefaultAudioBuffersPerSec = 100;

}  // namespace webrtc

#endif  // API_AUDIO_AUDIO_FRAME_H_
