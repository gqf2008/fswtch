// fswtch-aec3 shim: minimal reimplementation of rtc_base/numerics/safe_minmax.h
//
// AEC3 calls (unqualified, from within namespace webrtc) `SafeClamp(x, lo, hi)`,
// `SafeMin(a, b)`, `SafeMax(a, b)`. These return the clamped/min/max value
// with a result type derived from the common type of the arguments. The
// upstream versions guard against integer overflow when mixing signed/unsigned
// of different widths; this shim uses std::common_type which is correct for
// the homogeneous float/int usages in the AEC3 DSP (e.g.
// `SafeClamp(a, -32768.f, 32767.f)`).

#ifndef RTC_BASE_NUMERICS_SAFE_MINMAX_H_
#define RTC_BASE_NUMERICS_SAFE_MINMAX_H_

#include <algorithm>
#include <type_traits>

namespace webrtc {

template <typename T1, typename T2>
constexpr typename std::common_type<T1, T2>::type SafeMin(T1 a, T2 b) {
  using R = typename std::common_type<T1, T2>::type;
  return a < b ? static_cast<R>(a) : static_cast<R>(b);
}

template <typename T1, typename T2>
constexpr typename std::common_type<T1, T2>::type SafeMax(T1 a, T2 b) {
  using R = typename std::common_type<T1, T2>::type;
  return a > b ? static_cast<R>(a) : static_cast<R>(b);
}

template <typename T1, typename T2, typename T3>
constexpr typename std::common_type<T1, T2, T3>::type SafeClamp(T1 value,
                                                                T2 lo,
                                                                T3 hi) {
  using R = typename std::common_type<T1, T2, T3>::type;
  R v = static_cast<R>(value);
  R l = static_cast<R>(lo);
  R h = static_cast<R>(hi);
  return v < l ? l : (h < v ? h : v);
}

}  // namespace webrtc

#endif  // RTC_BASE_NUMERICS_SAFE_MINMAX_H_
