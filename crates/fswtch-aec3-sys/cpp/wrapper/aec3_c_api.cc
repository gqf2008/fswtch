#include "aec3_c_api.h"

#include <optional>

#include "common_audio/third_party/ooura/fft_size_128/ooura_fft.h"

#include "api/audio/echo_canceller3_config.h"
#include "api/audio/echo_control.h"
#include "api/audio/neural_residual_echo_estimator.h"
#include "api/environment/environment.h"
#include "api/field_trials_view.h"
#include "modules/audio_processing/aec3/echo_canceller3.h"
#include "modules/audio_processing/audio_buffer.h"

namespace {

// One EchoCanceller3 instance plus the reusable render/capture AudioBuffers it
// operates on. Member declaration order == construction order, which matters:
// `field_trials_` must outlive `env_` (Environment stores a const ref to it),
// and `env_`/`config_` must exist before `aec_` is constructed. FieldTrialsView
// (the shim) has no pure-virtuals, so it is concrete and instantiable directly.
struct Aec3Handle {
  webrtc::FieldTrialsView field_trials_;
  webrtc::Environment env_;
  webrtc::EchoCanceller3Config config_;
  webrtc::EchoCanceller3 aec_;
  webrtc::AudioBuffer render_buf_;
  webrtc::AudioBuffer capture_buf_;
  webrtc::StreamConfig render_cfg_;
  webrtc::StreamConfig capture_cfg_;
  const int sample_rate_hz_;

  Aec3Handle(int sample_rate_hz,
             size_t num_render_channels,
             size_t num_capture_channels)
      : field_trials_(),
        env_(field_trials_),
        config_(),
        aec_(env_,
             config_,
             std::nullopt,  // no per-multichannel config
             nullptr,       // neural residual echo estimator disabled
             sample_rate_hz,
             num_render_channels,
             num_capture_channels),
        render_buf_(static_cast<size_t>(sample_rate_hz),
                    num_render_channels,
                    static_cast<size_t>(sample_rate_hz),
                    num_render_channels,
                    static_cast<size_t>(sample_rate_hz),
                    num_render_channels),
        capture_buf_(static_cast<size_t>(sample_rate_hz),
                     num_capture_channels,
                     static_cast<size_t>(sample_rate_hz),
                     num_capture_channels,
                     static_cast<size_t>(sample_rate_hz),
                     num_capture_channels),
        render_cfg_(sample_rate_hz, num_render_channels),
        capture_cfg_(sample_rate_hz, num_capture_channels),
        sample_rate_hz_(sample_rate_hz) {}
};

}  // namespace

int32_t fswtch_aec3_api_version(void) {
  return 1;
}

int32_t fswtch_aec3_ooura_smoke(void) {
  // Scalar mode: `sse2_available=false` resolves to the portable C path on
  // non-x86/non-NEON builds (see the dispatch in ooura_fft.cc).
  webrtc::OouraFft fft(false);
  float buf[128] = {};
  fft.Fft(buf);
  return 1;
}

fswtch_aec3_t* fswtch_aec3_create(int32_t sample_rate_hz,
                                  size_t num_render_channels,
                                  size_t num_capture_channels) {
  if (sample_rate_hz <= 0 || num_render_channels == 0 || num_capture_channels == 0)
    return nullptr;
  try {
    auto* h = new Aec3Handle(static_cast<int>(sample_rate_hz),
                            num_render_channels, num_capture_channels);
    return reinterpret_cast<fswtch_aec3_t*>(h);
  } catch (...) {
    return nullptr;
  }
}

void fswtch_aec3_destroy(fswtch_aec3_t* aec) {
  delete reinterpret_cast<Aec3Handle*>(aec);
}

int32_t fswtch_aec3_analyze_render(fswtch_aec3_t* aec,
                                   const int16_t* render,
                                   size_t num_channels) {
  if (aec == nullptr || render == nullptr)
    return FSWTCH_AEC3_E_NULL_ARG;
  auto* h = reinterpret_cast<Aec3Handle*>(aec);
  if (num_channels != h->render_cfg_.num_channels())
    return FSWTCH_AEC3_E_CHANNELS;
  try {
    // CopyFrom loads interleaved int16 -> deinterleaved float. Split is only valid
    // when num_bands > 1: AudioBuffer only creates `splitting_filter_` then, and
    // SplitIntoFrequencyBands unconditionally derefs it. At 1 band (16 kHz) AEC3
    // reads `data_` directly via the `split_bands_const` fallback, so skip split.
    h->render_buf_.CopyFrom(render, h->render_cfg_);
    if (h->render_buf_.num_bands() > 1)
      h->render_buf_.SplitIntoFrequencyBands();
    h->aec_.AnalyzeRender(&h->render_buf_);
  } catch (...) {
    return FSWTCH_AEC3_E_EXCEPTION;
  }
  return FSWTCH_AEC3_OK;
}

int32_t fswtch_aec3_process_capture(fswtch_aec3_t* aec,
                                    int16_t* capture,
                                    size_t num_channels,
                                    int32_t level_change) {
  if (aec == nullptr || capture == nullptr)
    return FSWTCH_AEC3_E_NULL_ARG;
  auto* h = reinterpret_cast<Aec3Handle*>(aec);
  if (num_channels != h->capture_cfg_.num_channels())
    return FSWTCH_AEC3_E_CHANNELS;
  try {
    // Full per-frame capture path APM uses: load -> split (only if >1 band) ->
    // analyze saturation -> remove echo -> merge (only if >1 band) -> write back.
    // At 16 kHz / 1 band, split/merge are skipped (see analyze_render) and the
    // cleaned full-band signal is written straight back into `capture`.
    h->capture_buf_.CopyFrom(capture, h->capture_cfg_);
    if (h->capture_buf_.num_bands() > 1)
      h->capture_buf_.SplitIntoFrequencyBands();
    h->aec_.AnalyzeCapture(&h->capture_buf_);
    h->aec_.ProcessCapture(&h->capture_buf_, level_change != 0);
    if (h->capture_buf_.num_bands() > 1)
      h->capture_buf_.MergeFrequencyBands();
    h->capture_buf_.CopyTo(h->capture_cfg_, capture);
  } catch (...) {
    return FSWTCH_AEC3_E_EXCEPTION;
  }
  return FSWTCH_AEC3_OK;
}

void fswtch_aec3_set_audio_buffer_delay(fswtch_aec3_t* aec, int32_t delay_ms) {
  if (aec == nullptr)
    return;
  reinterpret_cast<Aec3Handle*>(aec)->aec_.SetAudioBufferDelay(
      static_cast<int>(delay_ms));
}

int32_t fswtch_aec3_active_processing(const fswtch_aec3_t* aec) {
  if (aec == nullptr)
    return 0;
  return reinterpret_cast<const Aec3Handle*>(aec)->aec_.ActiveProcessing() ? 1
                                                                           : 0;
}

void fswtch_aec3_get_metrics(const fswtch_aec3_t* aec,
                             double* echo_return_loss,
                             double* echo_return_loss_enhancement,
                             int32_t* delay_ms) {
  if (aec == nullptr) {
    if (echo_return_loss) *echo_return_loss = 0.0;
    if (echo_return_loss_enhancement) *echo_return_loss_enhancement = 0.0;
    if (delay_ms) *delay_ms = 0;
    return;
  }
  webrtc::EchoControl::Metrics m =
      reinterpret_cast<const Aec3Handle*>(aec)->aec_.GetMetrics();
  if (echo_return_loss) *echo_return_loss = m.echo_return_loss;
  if (echo_return_loss_enhancement)
    *echo_return_loss_enhancement = m.echo_return_loss_enhancement;
  if (delay_ms) *delay_ms = static_cast<int32_t>(m.delay_ms);
}
