// fswtch-apm shim: minimal reimplementation of rtc_base/strings/string_builder.h
//
// Provide rtc::StringBuilder with the small surface the AEC3 closure uses
// (<<, Append, str()). If a call site needs a method not present, extend this
// file — but start minimal and let the compiler guide.

#ifndef RTC_BASE_STRINGS_STRING_BUILDER_H_
#define RTC_BASE_STRINGS_STRING_BUILDER_H_

#include <cstdint>
#include <string>
#include <utility>

#include "absl/strings/string_view.h"

namespace rtc {

class StringBuilder {
 public:
  StringBuilder() = default;
  explicit StringBuilder(absl::string_view s) : str_(s) {}

  StringBuilder& operator<<(char v) { str_ += v; return *this; }
  StringBuilder& operator<<(absl::string_view v) { str_.append(v.data(), v.size()); return *this; }
  StringBuilder& operator<<(const char* v) { if (v) str_ += v; return *this; }
  StringBuilder& operator<<(const std::string& v) { str_ += v; return *this; }
  StringBuilder& operator<<(int v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(unsigned v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(long v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(long long v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(unsigned long v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(unsigned long long v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(float v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(double v) { str_ += std::to_string(v); return *this; }
  StringBuilder& operator<<(bool v) { str_ += (v ? "true" : "false"); return *this; }

  StringBuilder& Append(absl::string_view v) { str_.append(v.data(), v.size()); return *this; }

  const std::string& str() const { return str_; }
  std::string MoveResult() { return std::move(str_); }
  absl::string_view str_view() const { return str_; }

  void Clear() { str_.clear(); }
  size_t size() const { return str_.size(); }

 private:
  std::string str_;
};

}  // namespace rtc

// fswtch-apm shim addition for AGC2: the vendored agc2/interpolated_gain_curve
// translation unit uses `StringBuilder` unqualified from within `namespace
// webrtc` (the upstream rtc_base/strings/string_builder.h declares the class in
// `namespace webrtc`). Expose it there as an alias so both `rtc::StringBuilder`
// and `webrtc::StringBuilder` (and the unqualified form inside namespace webrtc)
// resolve to the same type.
namespace webrtc {
using ::rtc::StringBuilder;
}  // namespace webrtc

#endif  // RTC_BASE_STRINGS_STRING_BUILDER_H_
