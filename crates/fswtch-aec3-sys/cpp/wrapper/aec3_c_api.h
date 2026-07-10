#ifndef FSWTCH_AEC3_C_API_H
#define FSWTCH_AEC3_C_API_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Status codes returned by the processing functions. 0 == success. */
#define FSWTCH_AEC3_OK           0
#define FSWTCH_AEC3_E_NULL_ARG   1
#define FSWTCH_AEC3_E_CHANNELS   2
#define FSWTCH_AEC3_E_EXCEPTION (-1)

/* Opaque handle to a WebRTC EchoCanceller3 instance and its reusable buffers. */
typedef struct fswtch_aec3 fswtch_aec3_t;

/* Phase 0 smoke: returns the version of the AEC3 C ABI this wrapper exposes. */
int32_t fswtch_aec3_api_version(void);

/* Phase 1 smoke: runs one forward Ooura 128-point FFT over a zero buffer. */
int32_t fswtch_aec3_ooura_smoke(void);

/*
 * Creates an EchoCanceller3 with the default AEC3 config. The neural residual
 * echo estimator is disabled (constructed with neural=nullptr -> traditional
 * residual-echo-estimator path).
 *
 * `sample_rate_hz` must be an AEC3-supported rate (8000/16000/32000/48000). The
 * 16 kHz / 1-band path is the recommended default: no band splitting, so the
 * QMF/resampler stubs are never exercised. `num_render_channels` is the far-end
 * (loudspeaker) channel count; `num_capture_channels` the near-end (mic) count.
 *
 * Returns NULL on allocation failure or unsupported configuration.
 */
fswtch_aec3_t* fswtch_aec3_create(int32_t sample_rate_hz,
                                  size_t num_render_channels,
                                  size_t num_capture_channels);

/* Releases the handle. (Destroy + recreate is the only "reset"; EchoControl has no Reset().) */
void fswtch_aec3_destroy(fswtch_aec3_t* aec);

/*
 * Feeds one 10 ms far-end (loudspeaker) render frame to the canceller.
 * `render` is interleaved int16_t of exactly (sample_rate_hz/100 * num_channels)
 * samples. `num_channels` must equal the render channel count passed to create().
 * AnalyzeRender is the only method documented as concurrency-safe with the capture
 * side; all capture-side calls must be serialized by the caller.
 */
int32_t fswtch_aec3_analyze_render(fswtch_aec3_t* aec,
                                   const int16_t* render,
                                   size_t num_channels);

/*
 * Processes one 10 ms near-end (microphone) capture frame in place: analyzes
 * saturation then removes echo, writing the cleaned samples back into `capture`.
 * `capture` is interleaved int16_t of (sample_rate_hz/100 * num_channels) samples.
 * `level_change` is non-zero if the capture gain is known to have changed since
 * the last frame (toggles AEC3's filter divergence protection).
 */
int32_t fswtch_aec3_process_capture(fswtch_aec3_t* aec,
                                    int16_t* capture,
                                    size_t num_channels,
                                    int32_t level_change);

/* Sets an external estimate of the render->capture audio buffer delay, in ms. */
void fswtch_aec3_set_audio_buffer_delay(fswtch_aec3_t* aec, int32_t delay_ms);

/* Returns 1 if the canceller is actively processing, 0 otherwise. */
int32_t fswtch_aec3_active_processing(const fswtch_aec3_t* aec);

/* Writes current metrics; any out-pointer may be NULL to skip that field. */
void fswtch_aec3_get_metrics(const fswtch_aec3_t* aec,
                             double* echo_return_loss,
                             double* echo_return_loss_enhancement,
                             int32_t* delay_ms);

#ifdef __cplusplus
}
#endif

#endif /* FSWTCH_AEC3_C_API_H */
