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
typedef struct fswtch_apm fswtch_aec3_t;

/* Phase 0 smoke: returns the version of the AEC3 C ABI this wrapper exposes. */
int32_t fswtch_aec3_api_version(void);

/* Phase 1 smoke: runs one forward Ooura 128-point FFT over a zero buffer. */
int32_t fswtch_aec3_ooura_smoke(void);

/*
 * AEC3 tuning config — a C mirror of the key fields of WebRTC's
 * EchoCanceller3Config. fswtch_aec3_default_config() returns the WebRTC
 * defaults; to tune, call it, copy the result, and override fields. Pass NULL
 * to fswtch_aec3_create to use the defaults unchanged.
 *
 * Filter length is in 64-sample blocks (~4 ms at 16 kHz); it must cover the echo
 * tail. WebRTC default is 13 blocks (~52 ms). Increase for large rooms / long
 * acoustic paths. AEC3 clamps refined_initial/coarse_initial <= refined/coarse.
 */
typedef struct {
    size_t filter_refined_length_blocks;  /* default 13 */
    size_t filter_coarse_length_blocks;   /* default 13 */
    size_t delay_headroom_samples;         /* default 32 */
    float  ep_strength_default_len;        /* default 0.83 — echo-path length prior */
    float  erle_min;                       /* default 1.0  — ERLE estimate floor */
    float  erle_max_l;                      /* default 4.0  — ERLE cap, low bands */
    float  erle_max_h;                      /* default 1.5  — ERLE cap, high bands */
} fswtch_aec3_config_t;

/* Returns a pointer to a static config initialized with the WebRTC AEC3 defaults. */
const fswtch_aec3_config_t* fswtch_aec3_default_config(void);

/*
 * Creates an EchoCanceller3. `config` may be NULL (= WebRTC defaults). The
 * neural residual echo estimator is disabled (neural=nullptr -> traditional
 * residual-echo-estimator path).
 *
 * `sample_rate_hz` must be 8000/16000/48000 (16 kHz recommended; 32 kHz needs
 * the QMF shim, not yet wired). Returns NULL on allocation failure or bad args.
 */
fswtch_aec3_t* fswtch_aec3_create(const fswtch_aec3_config_t* config,
                                  int32_t sample_rate_hz,
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
