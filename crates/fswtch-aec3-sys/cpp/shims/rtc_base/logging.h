// fswtch-aec3 shim: minimal reimplementation of rtc_base/logging.h
//
// AEC3 uses RTC_LOG(LS_INFO) << "...", RTC_LOG_V(severity_var) << "...",
// RTC_LOG_IF(...), and RTC_DLOG(...). This shim turns them into no-ops that
// still type-check the `<<` chain (an eater). Nothing is printed; the smoke
// tests don't depend on log output and this keeps the test runs quiet.

#ifndef RTC_BASE_LOGGING_H_
#define RTC_BASE_LOGGING_H_

#include <cstddef>
#include <string>

namespace webrtc {

// Severity constants used as RTC_LOG(...) arguments.
enum LoggingSeverity {
  LS_VERBOSE,
  LS_INFO,
  LS_WARNING,
  LS_ERROR,
  LS_NONE,
};

// Eater for the `<<` chain. Accepts any type.
class WebrtcLogStream {
 public:
  template <typename T>
  WebrtcLogStream& operator<<(const T&) { return *this; }
};

}  // namespace webrtc

#define RTC_LOG(sev) ::webrtc::WebrtcLogStream()
#define RTC_LOG_V(sev) ::webrtc::WebrtcLogStream()
#define RTC_LOG_IF(sev, condition) ::webrtc::WebrtcLogStream()
#define RTC_DLOG(sev) ::webrtc::WebrtcLogStream()
#define RTC_DLOG_IF(sev, condition) ::webrtc::WebrtcLogStream()
#define RTC_LOG_TAG(sev, tag) ::webrtc::WebrtcLogStream()

#endif  // RTC_BASE_LOGGING_H_
