#ifndef FSWTCH_HPF_C_API_H
#define FSWTCH_HPF_C_API_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to a WebRTC HighPassFilter + its reusable AudioBuffer. */
typedef struct fswtch_hpf fswtch_hpf_t;

/* Creates a high-pass filter for `sample_rate_hz` / `num_channels`. NULL on bad args. */
fswtch_hpf_t* fswtch_hpf_create(int32_t sample_rate_hz, size_t num_channels);

/* Releases the handle. */
void fswtch_hpf_destroy(fswtch_hpf_t* hpf);

/*
 * High-pass filters one 10 ms interleaved int16 frame in place. `frame` must be
 * (sample_rate_hz/100 * num_channels) samples; `num_channels` must match create().
 * Returns 0 on success, 1 null arg, 2 channel mismatch, -1 C++ exception.
 */
int32_t fswtch_hpf_process(fswtch_hpf_t* hpf, int16_t* frame, size_t num_channels);

#ifdef __cplusplus
}
#endif

#endif /* FSWTCH_HPF_C_API_H */
