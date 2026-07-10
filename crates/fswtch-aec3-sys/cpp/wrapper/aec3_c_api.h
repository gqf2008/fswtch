#ifndef FSWTCH_AEC3_C_API_H
#define FSWTCH_AEC3_C_API_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Phase 0 smoke entrypoint. Returns the version of the AEC3 C ABI this wrapper
 * exposes. The full AEC3 surface (create / analyze_render / process_capture / ...)
 * is layered in once the C++ closure is vendored.
 */
int32_t fswtch_aec3_api_version(void);

/*
 * Phase 1 smoke: constructs the vendored Ooura 128-point FFT in scalar (portable) mode and runs
 * one forward transform over a zero buffer. Proves the ooura C++ closure compiles and links into
 * the static lib that the Rust side binds. The full AEC3 surface is layered in later phases.
 */
int32_t fswtch_aec3_ooura_smoke(void);

#ifdef __cplusplus
}
#endif

#endif /* FSWTCH_AEC3_C_API_H */
