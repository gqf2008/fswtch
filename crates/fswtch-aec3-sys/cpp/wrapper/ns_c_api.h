#ifndef FSWTCH_NS_C_API_H
#define FSWTCH_NS_C_API_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to a WebRTC NoiseSuppressor + its reusable AudioBuffer. */
typedef struct fswtch_ns fswtch_ns_t;

/*
 * Creates a noise suppressor. `level`: 0=6 dB, 1=12 dB (default), 2=18 dB, 3=21 dB of
 * suppression. `sample_rate_hz` must be 8000/16000/48000. NULL on bad args.
 */
fswtch_ns_t* fswtch_ns_create(int32_t level,
                               int32_t sample_rate_hz,
                               size_t num_channels);

void fswtch_ns_destroy(fswtch_ns_t* ns);

/*
 * Suppresses noise in one 10 ms interleaved int16 frame in place (analyzes then processes).
 * `frame` must be (sample_rate_hz/100 * num_channels) samples; `num_channels` must match create().
 * Returns 0 on success, 1 null arg, 2 channel mismatch, -1 C++ exception.
 */
int32_t fswtch_ns_process(fswtch_ns_t* ns, int16_t* frame, size_t num_channels);

#ifdef __cplusplus
}
#endif

#endif /* FSWTCH_NS_C_API_H */
