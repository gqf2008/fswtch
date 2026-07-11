#include "agc2_c_api.h"

#include "api/audio/audio_processing.h"  // StreamConfig (minimal shim)
#include "common_audio/include/audio_util.h"  // DbToRatio
#include "modules/audio_processing/agc2/gain_applier.h"
#include "modules/audio_processing/agc2/limiter.h"
#include "modules/audio_processing/audio_buffer.h"
#include "modules/audio_processing/logging/apm_data_dumper.h"

namespace {

// One AGC2 fixed-gain + limiter chain plus a reusable AudioBuffer/StreamConfig.
//
// This is the exact scalar path GainController2::Process runs when adaptive
// digital and input-volume control are both disabled:
//   fixed_gain_applier_.ApplyGain(float_frame);   // the fixed digital gain
//   limiter_.Process(float_frame);                // the hard limiter
// (see modules/audio_processing/gain_controller2.cc, Process()). gain_controller2.cc
// is NOT compiled into this build: its ctor and Process() contain *runtime*
// if-blocks (not #ifdef) that reference the adaptive classes —
// SpeechLevelEstimator::Create, AdaptiveDigitalGainController, the RNN-backed
// VoiceActivityDetectorWrapper, NoiseLevelEstimator, SaturationProtector and
// InputVolumeController. Excluding those .cc leaves undefined symbols (and the
// rlib produced by `cargo build -p fswtch-apm-sys` does not link, so the break
// would surface only in the downstream Rust crate); including them pulls the
// agc2/rnn_vad neural closure. Calling the two leaf stages directly yields the
// identical scalar DSP without that dependency.
//
// Member declaration order == ctor init order. `limiter_` stores
// `&data_dumper_`, so `data_dumper_` must be constructed first; `buf_`/`cfg_`
// are independent of the others.
struct Agc2Handle {
  webrtc::ApmDataDumper data_dumper_;
  webrtc::GainApplier fixed_gain_applier_;
  webrtc::Limiter limiter_;
  webrtc::AudioBuffer buf_;
  webrtc::StreamConfig cfg_;
  const bool limiter_enabled_;

  Agc2Handle(float fixed_gain_db,
             bool limiter_enabled,
             int sample_rate_hz,
             size_t num_channels)
      : data_dumper_(0),
        fixed_gain_applier_(/*hard_clip_samples=*/false,
                            webrtc::DbToRatio(fixed_gain_db)),
        // SampleRateToDefaultChannelSize(rate) == rate / 100 (samples per 10 ms
        // frame); computed inline to avoid vendoring the heavy audio_frame.h.
        limiter_(&data_dumper_,
                 static_cast<size_t>(sample_rate_hz) / 100,
                 "Agc2"),
        buf_(static_cast<size_t>(sample_rate_hz),
             num_channels,
             static_cast<size_t>(sample_rate_hz),
             num_channels,
             static_cast<size_t>(sample_rate_hz),
             num_channels),
        cfg_(sample_rate_hz, num_channels),
        limiter_enabled_(limiter_enabled) {}
};

}  // namespace

fswtch_agc2_t* fswtch_agc2_create(float fixed_gain_db,
                                  int32_t limiter_enabled,
                                  int32_t sample_rate_hz,
                                  size_t num_channels) {
  if (sample_rate_hz <= 0 || num_channels == 0)
    return nullptr;
  try {
    return reinterpret_cast<fswtch_agc2_t*>(
        new Agc2Handle(fixed_gain_db,
                      limiter_enabled != 0,
                      static_cast<int>(sample_rate_hz),
                      num_channels));
  } catch (...) {
    return nullptr;
  }
}

void fswtch_agc2_destroy(fswtch_agc2_t* agc2) {
  delete reinterpret_cast<Agc2Handle*>(agc2);
}

int32_t fswtch_agc2_process(fswtch_agc2_t* agc2, int16_t* frame, size_t num_channels) {
  if (agc2 == nullptr || frame == nullptr)
    return 1;
  auto* h = reinterpret_cast<Agc2Handle*>(agc2);
  if (num_channels != h->cfg_.num_channels())
    return 2;
  try {
    // Load interleaved int16 -> float full-band. At 1 band (16 kHz) no split is
    // needed: the fixed-gain applier and the limiter both operate on the
    // full-band DeinterleavedView exposed by AudioBuffer::view(). GainController2
    // runs the same two stages (adaptive off); here they are called directly,
    // skipping the (disabled) adaptive/VAD/noise-level bookkeeping.
    h->buf_.CopyFrom(frame, h->cfg_);
    webrtc::DeinterleavedView<float> v = h->buf_.view();
    h->fixed_gain_applier_.ApplyGain(v);
    if (h->limiter_enabled_)
      h->limiter_.Process(v);
    h->buf_.CopyTo(h->cfg_, frame);
  } catch (...) {
    return -1;
  }
  return 0;
}
