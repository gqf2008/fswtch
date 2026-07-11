#include "hpf_c_api.h"

#include "api/audio/audio_processing.h"  // StreamConfig (minimal shim)
#include "modules/audio_processing/audio_buffer.h"
#include "modules/audio_processing/high_pass_filter.h"

namespace {

// One HighPassFilter + a reusable AudioBuffer/StreamConfig. Member order == ctor
// init order; none of the members depend on another.
struct HpfHandle {
  webrtc::HighPassFilter hpf;
  webrtc::AudioBuffer buf;
  webrtc::StreamConfig cfg;

  HpfHandle(int sample_rate_hz, size_t num_channels)
      : hpf(sample_rate_hz, num_channels),
        buf(static_cast<size_t>(sample_rate_hz),
            num_channels,
            static_cast<size_t>(sample_rate_hz),
            num_channels,
            static_cast<size_t>(sample_rate_hz),
            num_channels),
        cfg(sample_rate_hz, num_channels) {}
};

}  // namespace

fswtch_hpf_t* fswtch_hpf_create(int32_t sample_rate_hz, size_t num_channels) {
  if (sample_rate_hz <= 0 || num_channels == 0)
    return nullptr;
  try {
    return reinterpret_cast<fswtch_hpf_t*>(
        new HpfHandle(static_cast<int>(sample_rate_hz), num_channels));
  } catch (...) {
    return nullptr;
  }
}

void fswtch_hpf_destroy(fswtch_hpf_t* hpf) {
  delete reinterpret_cast<HpfHandle*>(hpf);
}

int32_t fswtch_hpf_process(fswtch_hpf_t* hpf, int16_t* frame, size_t num_channels) {
  if (hpf == nullptr || frame == nullptr)
    return 1;
  auto* h = reinterpret_cast<HpfHandle*>(hpf);
  if (num_channels != h->cfg.num_channels())
    return 2;
  try {
    // Load interleaved int16 -> float full-band; HPF on full-band data (no split needed
    // at 1 band; HighPassFilter::Process(audio, /*use_split_band_data=*/false) reads
    // `audio->channels()`); write back.
    h->buf.CopyFrom(frame, h->cfg);
    h->hpf.Process(&h->buf, /*use_split_band_data=*/false);
    h->buf.CopyTo(h->cfg, frame);
  } catch (...) {
    return -1;
  }
  return 0;
}
