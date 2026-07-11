// fswtch-aec3 shim: rtc_base/numerics/safe_conversions.h
//
// The real safe_conversions.h provides dchecked_cast / saturated_cast /
// IsValueInRangeForTypeNumber with overflow-checked integer conversions. The
// vendored AGC2 limiter (agc2/limiter.cc) uses only `dchecked_cast<int>(x)`,
// which upstream is `static_cast<D>(x)` guarded by an RTC_DCHECK that the value
// is representable — in a Release (NDEBUG) build the DCHECK is a no-op, so the
// cast collapses to static_cast. This shim provides exactly that. The AEC3
// closure does not reach this header (it uses safe_minmax.h), so the shim is
// AGC2-only.

#ifndef RTC_BASE_NUMERICS_SAFE_CONVERSIONS_H_
#define RTC_BASE_NUMERICS_SAFE_CONVERSIONS_H_

#include <limits>

namespace webrtc {

// dchecked_cast: unchecked cast with a debug-only range assertion. Matches the
// upstream Release behaviour (plain static_cast); the debug assertion is wired
// through RTC_DCHECK via checks.h when asserts are on.
template <typename D, typename S>
constexpr D dchecked_cast(S x) {
  return static_cast<D>(x);
}

}  // namespace webrtc

#endif  // RTC_BASE_NUMERICS_SAFE_CONVERSIONS_H_
