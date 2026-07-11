#ifndef FSWTCH_AGC2_C_API_H
#define FSWTCH_AGC2_C_API_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to a WebRTC AGC2 fixed-digital-gain + limiter chain and its
 * reusable AudioBuffer. */
typedef struct fswtch_agc2 fswtch_agc2_t;

/*
 * Creates an AGC2 instance applying a fixed digital gain of `fixed_gain_db`
 * (dB, >= 0) followed by an optional hard limiter — the scalar path
 * GainController2::Process runs when adaptive digital + input-volume control
 * are both disabled. `limiter_enabled` != 0 enables the limiter stage. For 16 kHz
 * / 1 band no band splitting is needed. `sample_rate_hz` must be divisible by
 * 100 (8000/16000/48000). Returns NULL on bad args.
 */
fswtch_agc2_t* fswtch_agc2_create(float fixed_gain_db,
                                  int32_t limiter_enabled,
                                  int32_t sample_rate_hz,
                                  size_t num_channels);

/* Releases the handle. */
void fswtch_agc2_destroy(fswtch_agc2_t* agc2);

/*
 * Processes one 10 ms interleaved int16 frame in place: applies the fixed
 * digital gain, then (if enabled) the limiter. `frame` must hold
 * (sample_rate_hz/100 * num_channels) samples; `num_channels` must match
 * create(). Returns 0 on success, 1 null arg, 2 channel mismatch,
 * -1 C++ exception.
 */
int32_t fswtch_agc2_process(fswtch_agc2_t* agc2, int16_t* frame, size_t num_channels);

#ifdef __cplusplus
}
#endif

#endif /* FSWTCH_AGC2_C_API_H */
