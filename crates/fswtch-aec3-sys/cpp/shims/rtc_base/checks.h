// fswtch-aec3 shim: minimal reimplementation of rtc_base/checks.h
//
// Provides the RTC_CHECK / RTC_DCHECK family (with EQ/NE/LT/LE/GT/GE variants),
// RTC_FATAL(), RTC_CHECK_NOTREACHED(), RTC_DCHECK_NOTREACHED(), and the
// streaming `<<` form. On failure (RTC_CHECK* / RTC_FATAL / RTC_CHECK_NOTREACHED)
// the process is aborted via std::abort(); RTC_DCHECK* is a no-op in release
// (NDEBUG) builds — matching the upstream semantics where RTC_DCHECK_IS_ON
// tracks assert mode.
//
// This shim never prints the streamed values, only the failing expression +
// file:line. That is sufficient for the vendored AEC3 scalar closure to compile
// and link.

#ifndef RTC_BASE_CHECKS_H_
#define RTC_BASE_CHECKS_H_

#include <cstdio>
#include <cstdlib>

// RTC_DCHECK_IS_ON mirrors upstream: 1 when asserts are enabled, else 0.
#ifndef NDEBUG
#define RTC_DCHECK_IS_ON 1
#else
#define RTC_DCHECK_IS_ON 0
#endif

#define RTC_NORETURN [[noreturn]]

namespace webrtc {

// Eater for the streaming `<<` form of RTC_CHECK/RTC_DCHECK/RTC_FATAL. Each
// `operator<<` is a no-op template accepting any argument; it exists only so
// `RTC_CHECK(x) << "msg" << val` type-checks regardless of `val`'s streamability.
class RTCLogStreamer {
 public:
  template <typename T>
  RTCLogStreamer& operator<<(const T&) {
    return *this;
  }
};

[[noreturn]] inline void rtc_check_fail(const char* file, int line,
                                        const char* expr) {
  std::fprintf(stderr, "RTC_CHECK failed: %s at %s:%d\n", expr, file, line);
  std::abort();
}

}  // namespace webrtc

// ---------------------------------------------------------------------------
// RTC_CHECK: always-on. On failure aborts. Supports `<<` streaming after the
// condition. The condition is evaluated exactly once; when it holds, the
// resulting LogStreamer eats any subsequent `<< value`s (no abort). When it
// fails, rtc_check_fail() ([[noreturn]]) aborts before the `<<` chain runs.
// ---------------------------------------------------------------------------
#define RTC_CHECK(condition)                                        \
  ((!(condition))                                                   \
       ? (::webrtc::rtc_check_fail(__FILE__, __LINE__, #condition),  \
          ::webrtc::RTCLogStreamer())                                \
       : ::webrtc::RTCLogStreamer())

#define RTC_CHECK_OP(name, op, val1, val2) RTC_CHECK((val1)op(val2))

#define RTC_CHECK_EQ(val1, val2) RTC_CHECK_OP(Eq, ==, val1, val2)
#define RTC_CHECK_NE(val1, val2) RTC_CHECK_OP(Ne, !=, val1, val2)
#define RTC_CHECK_LE(val1, val2) RTC_CHECK_OP(Le, <=, val1, val2)
#define RTC_CHECK_LT(val1, val2) RTC_CHECK_OP(Lt, <, val1, val2)
#define RTC_CHECK_GE(val1, val2) RTC_CHECK_OP(Ge, >=, val1, val2)
#define RTC_CHECK_GT(val1, val2) RTC_CHECK_OP(Gt, >, val1, val2)

// ---------------------------------------------------------------------------
// RTC_DCHECK: asserts only when RTC_DCHECK_IS_ON (debug). In release the
// condition is still evaluated once for side effects / unused-var silence, and
// the `<<` chain compiles via RTCLogStreamer.
// ---------------------------------------------------------------------------
#if RTC_DCHECK_IS_ON
#define RTC_DCHECK(condition) RTC_CHECK(condition)
#define RTC_DCHECK_EQ(v1, v2) RTC_CHECK_EQ(v1, v2)
#define RTC_DCHECK_NE(v1, v2) RTC_CHECK_NE(v1, v2)
#define RTC_DCHECK_LE(v1, v2) RTC_CHECK_LE(v1, v2)
#define RTC_DCHECK_LT(v1, v2) RTC_CHECK_LT(v1, v2)
#define RTC_DCHECK_GE(v1, v2) RTC_CHECK_GE(v1, v2)
#define RTC_DCHECK_GT(v1, v2) RTC_CHECK_GT(v1, v2)
#else
#define RTC_DCHECK(condition) ((void)(condition), ::webrtc::RTCLogStreamer())
#define RTC_DCHECK_EQ(v1, v2) ((void)((v1) == (v2)), ::webrtc::RTCLogStreamer())
#define RTC_DCHECK_NE(v1, v2) ((void)((v1) != (v2)), ::webrtc::RTCLogStreamer())
#define RTC_DCHECK_LE(v1, v2) ((void)((v1) <= (v2)), ::webrtc::RTCLogStreamer())
#define RTC_DCHECK_LT(v1, v2) ((void)((v1) < (v2)), ::webrtc::RTCLogStreamer())
#define RTC_DCHECK_GE(v1, v2) ((void)((v1) >= (v2)), ::webrtc::RTCLogStreamer())
#define RTC_DCHECK_GT(v1, v2) ((void)((v1) > (v2)), ::webrtc::RTCLogStreamer())
#endif

#define RTC_UNREACHABLE_CODE_HIT false

// RTC_DCHECK_NOTREACHED: no-op in release, asserts in debug. Supports `<<`.
#define RTC_DCHECK_NOTREACHED() RTC_DCHECK(RTC_UNREACHABLE_CODE_HIT)

// RTC_CHECK_NOTREACHED: always aborts (never returns).
#define RTC_CHECK_NOTREACHED() \
  ::webrtc::rtc_check_fail(__FILE__, __LINE__, "RTC_CHECK_NOTREACHED()")

// RTC_FATAL: always aborts. Supports `<<` streaming (e.g. `RTC_FATAL() << "x"`).
#define RTC_FATAL()                                                 \
  (::webrtc::rtc_check_fail(__FILE__, __LINE__, "RTC_FATAL()"),      \
   ::webrtc::RTCLogStreamer())

// Hardening assert (independent of NDEBUG). Upstream ties it to RTC_HARDENING.
#ifndef RTC_HARDENING
#define RTC_HARDENING 0
#endif
#if RTC_HARDENING
#define RTC_HARDENING_ASSERT(x) RTC_CHECK(x)
#else
#define RTC_HARDENING_ASSERT(x) RTC_DCHECK(x)
#endif

namespace webrtc {
// Performs integer division a/b and asserts the remainder is zero. Defined
// after the RTC_CHECK_* macros so the macro is visible during template-body
// preprocessing.
template <typename T>
inline T CheckedDivExact(T a, T b) {
  RTC_CHECK_EQ(a % b, 0);
  return a / b;
}
}  // namespace webrtc

#endif  // RTC_BASE_CHECKS_H_
