// fswtch-aec3 shim: api/audio/audio_processing.h  (replaces the real 36KB root)
//
// The real AudioProcessing is a vast abstract APM root that drags in ref_count,
// scoped_refptr, task_queue, rtc_event_log, audio_view, channel_buffer and
// audio_processing_statistics. The vendored AEC3 closure + the real APM helper
// files (audio_buffer, splitting_filter) only need two surface types from it:
//
//   * `webrtc::StreamConfig` — (sample_rate_hz, num_channels) with
//     `num_frames() == sample_rate_hz / 100`; used by AudioBuffer::CopyFrom/
//     CopyTo.
//   * `webrtc::AudioProcessing::Config::Pipeline::DownmixMethod` enum
//     (kAverageChannels / kUseFirstChannel / kAdaptive) — a member type of
//     AudioBuffer.
//
// This shim provides exactly those. Nothing else the AEC3 closure references
// from this header.

#ifndef API_AUDIO_AUDIO_PROCESSING_H_
#define API_AUDIO_AUDIO_PROCESSING_H_

#include <cstddef>

namespace webrtc {

// Minimal AudioProcessing root: only the Config::Pipeline::DownmixMethod enum
// is referenced (as a member type of AudioBuffer).
class AudioProcessing {
 public:
  struct Config {
    struct Pipeline {
      enum class DownmixMethod {
        kAverageChannels,  // Average across channels.
        kUseFirstChannel,  // Use the first channel.
        kAdaptive          // Adaptively choose how to downmix.
      };
    };
  };
};

// Stream configuration describing the rate/channels of a capture or render
// stream. `num_frames()` is the 10 ms frame length = floor(rate/100).
class StreamConfig {
 public:
  StreamConfig(int sample_rate_hz = 0,  // NOLINT(runtime/explicit)
               size_t num_channels = 0)
      : sample_rate_hz_(sample_rate_hz),
        num_channels_(num_channels),
        num_frames_(GetFrameSize(sample_rate_hz)) {}

  int sample_rate_hz() const { return sample_rate_hz_; }
  size_t num_channels() const { return num_channels_; }
  // Returns floor(sample_rate_hz/100): the number of samples per channel.
  size_t num_frames() const { return num_frames_; }

  void set_sample_rate_hz(int value) {
    sample_rate_hz_ = value;
    num_frames_ = GetFrameSize(value);
  }
  void set_num_channels(size_t value) { num_channels_ = value; }

  static int GetFrameSize(int sample_rate_hz) { return sample_rate_hz / 100; }

 private:
  int sample_rate_hz_;
  size_t num_channels_;
  size_t num_frames_;
};

}  // namespace webrtc

#endif  // API_AUDIO_AUDIO_PROCESSING_H_
