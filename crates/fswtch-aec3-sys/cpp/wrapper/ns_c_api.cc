#include "ns_c_api.h"

#include "api/audio/audio_processing.h"  // StreamConfig (minimal shim)
#include "modules/audio_processing/audio_buffer.h"
#include "modules/audio_processing/ns/noise_suppressor.h"
#include "modules/audio_processing/ns/ns_config.h"

namespace {

// One NoiseSuppressor + a reusable AudioBuffer/StreamConfig. Member order == ctor init order.
struct NsHandle {
  webrtc::NoiseSuppressor ns;
  webrtc::AudioBuffer buf;
  webrtc::StreamConfig cfg;

  NsHandle(const webrtc::NsConfig& config, int sample_rate_hz, size_t num_channels)
      : ns(config,
           static_cast<size_t>(sample_rate_hz),
           num_channels),
        buf(static_cast<size_t>(sample_rate_hz),
            num_channels,
            static_cast<size_t>(sample_rate_hz),
            num_channels,
            static_cast<size_t>(sample_rate_hz),
            num_channels),
        cfg(sample_rate_hz, num_channels) {}
};

webrtc::NsConfig make_ns_config(int32_t level) {
  using SL = webrtc::NsConfig::SuppressionLevel;
  webrtc::NsConfig c;
  switch (level) {
    case 0: c.target_level = SL::k6dB; break;
    case 1: c.target_level = SL::k12dB; break;
    case 2: c.target_level = SL::k18dB; break;
    case 3: c.target_level = SL::k21dB; break;
    default: c.target_level = SL::k12dB; break;
  }
  return c;
}

}  // namespace

fswtch_ns_t* fswtch_ns_create(int32_t level,
                              int32_t sample_rate_hz,
                              size_t num_channels) {
  if (sample_rate_hz <= 0 || num_channels == 0)
    return nullptr;
  try {
    return reinterpret_cast<fswtch_ns_t*>(new NsHandle(
        make_ns_config(level), static_cast<int>(sample_rate_hz), num_channels));
  } catch (...) {
    return nullptr;
  }
}

void fswtch_ns_destroy(fswtch_ns_t* ns) {
  delete reinterpret_cast<NsHandle*>(ns);
}

int32_t fswtch_ns_process(fswtch_ns_t* ns, int16_t* frame, size_t num_channels) {
  if (ns == nullptr || frame == nullptr)
    return 1;
  auto* h = reinterpret_cast<NsHandle*>(ns);
  if (num_channels != h->cfg.num_channels())
    return 2;
  try {
    // Load interleaved int16 -> float. At 1 band (16 kHz) NS reads the full-band data via the
    // AudioBuffer::split_bands_const fallback (no SplitIntoFrequencyBands needed). Analyze
    // estimates noise; Process applies the suppression; write back.
    h->buf.CopyFrom(frame, h->cfg);
    h->ns.Analyze(h->buf);
    h->ns.Process(&h->buf);
    h->buf.CopyTo(h->cfg, frame);
  } catch (...) {
    return -1;
  }
  return 0;
}
