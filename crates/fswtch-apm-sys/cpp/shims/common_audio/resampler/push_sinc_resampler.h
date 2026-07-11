// fswtch-apm shim: common_audio/resampler/push_sinc_resampler.h
//
// The real PushSincResampler wraps SincResampler (a large sinc-kernel
// subsystem) for push-based sample-rate conversion. AudioBuffer holds
// `std::unique_ptr<PushSincResampler>` per channel and calls
// `Resample(src, src_frames, dst, dst_frames)`. This shim provides a minimal
// resampler that copies `min(src_frames, dst_frames)` samples and zero-fills
// any remainder — enough to compile/link and run the smoke tests. A real
// resampling path can be layered back in if a future phase drives AEC3 at
// mismatched rates; at matched rates (the C-API default) Resample is a 1:1 copy.

#ifndef COMMON_AUDIO_RESAMPLER_PUSH_SINC_RESAMPLER_H_
#define COMMON_AUDIO_RESAMPLER_PUSH_SINC_RESAMPLER_H_

#include <cstddef>
#include <cstdint>

namespace webrtc {

class PushSincResampler {
 public:
  PushSincResampler(size_t source_frames, size_t destination_frames)
      : source_frames_(source_frames),
        destination_frames_(destination_frames) {}
  ~PushSincResampler() = default;
  PushSincResampler(const PushSincResampler&) = delete;
  PushSincResampler& operator=(const PushSincResampler&) = delete;

  size_t Resample(const int16_t* source,
                  size_t source_frames,
                  int16_t* destination,
                  size_t destination_frames) {
    size_t n = source_frames < destination_frames ? source_frames
                                                   : destination_frames;
    for (size_t i = 0; i < n; ++i) {
      destination[i] = source[i];
    }
    for (size_t i = n; i < destination_frames; ++i) {
      destination[i] = 0;
    }
    return destination_frames;
  }

  size_t Resample(const float* source,
                  size_t source_frames,
                  float* destination,
                  size_t destination_frames) {
    size_t n = source_frames < destination_frames ? source_frames
                                                   : destination_frames;
    for (size_t i = 0; i < n; ++i) {
      destination[i] = source[i];
    }
    for (size_t i = n; i < destination_frames; ++i) {
      destination[i] = 0.0f;
    }
    return destination_frames;
  }

 private:
  size_t source_frames_;
  size_t destination_frames_;
};

}  // namespace webrtc

#endif  // COMMON_AUDIO_RESAMPLER_PUSH_SINC_RESAMPLER_H_
