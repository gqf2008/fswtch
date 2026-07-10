// fswtch-aec3 shim: modules/audio_processing/capture_mixer/capture_mixer.h
//
// The real CaptureMixer is a small subsystem (audio_content_analyzer +
// channel_content_remixer + remixing_logic) that downmixes/upmixes capture
// channels. AudioBuffer holds it by value as `CaptureMixer capture_mixer_;`
// and calls `capture_mixer_.Mix(num_output_channels, channel0_span,
// channel1_span)`. This shim provides a minimal Mix that keeps channel0 and
// averages into channel1 / copies as needed — sufficient to compile and link;
// the embedded AEC3 build does not exercise multi-channel remix paths via the
// C API in Phase 2.

#ifndef MODULES_AUDIO_PROCESSING_CAPTURE_MIXER_CAPTURE_MIXER_H_
#define MODULES_AUDIO_PROCESSING_CAPTURE_MIXER_CAPTURE_MIXER_H_

#include <cstddef>
#include <span>

namespace webrtc {

class CaptureMixer {
 public:
  explicit CaptureMixer(size_t num_samples_per_channel)
      : num_samples_per_channel_(num_samples_per_channel) {}
  CaptureMixer(const CaptureMixer&) = delete;
  CaptureMixer& operator=(const CaptureMixer&) = delete;
  ~CaptureMixer() = default;

  // Minimal remix: with a single output channel, average channel0/channel1
  // into channel0; otherwise leave both channels intact. No-ops on the spans'
  // contents beyond that — sufficient for the scalar/no-remix build.
  void Mix(size_t num_output_channels,
           std::span<float> channel0,
           std::span<float> channel1) {
    if (num_output_channels == 1 && channel0.size() == channel1.size()) {
      for (size_t i = 0; i < channel0.size(); ++i) {
        channel0[i] = 0.5f * (channel0[i] + channel1[i]);
      }
    }
  }

 private:
  size_t num_samples_per_channel_;
};

}  // namespace webrtc

#endif  // MODULES_AUDIO_PROCESSING_CAPTURE_MIXER_CAPTURE_MIXER_H_
