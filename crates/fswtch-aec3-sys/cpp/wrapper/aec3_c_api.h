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

#ifdef __cplusplus
}
#endif

#endif /* FSWTCH_AEC3_C_API_H */
