// Umbrella header for bindgen: includes every module's C ABI so a single
// bindgen run produces bindings for the whole WebRTC audio-processing chain
// (AEC3 + HF + NS + AGC2). Add new module headers here as they land.
#ifndef FSWTCH_APM_H
#define FSWTCH_APM_H

#include "aec3_c_api.h"
#include "hpf_c_api.h"
#include "ns_c_api.h"
// #include "agc2_c_api.h"   // added in the AGC2 step

#endif  // FSWTCH_APM_H
