// fswtch-apm shim: minimal reimplementation of rtc_base/string_utils.h
//
// apm_data_dumper.h includes this. Let me check what's actually used and
// provide a small surface: rtc::ToString and a few helpers. (Kept minimal;
// extend only if the compiler demands more.)

#ifndef RTC_BASE_STRING_UTILS_H_
#define RTC_BASE_STRING_UTILS_H_

#include <cstdint>
#include <cstdlib>
#include <string>

namespace rtc {

// Returns the number of characters (excluding NUL) that would be written to
// `buffer` for the given value, like snprintf. Used by upstream rtc::ToString
// internals; provided here for compatibility.
template <typename T>
inline size_t ToString(T value, char* buffer, size_t size) {
  return std::snprintf(buffer, size, "%d", static_cast<int>(value));
}

inline std::string ToString(int value) { return std::to_string(value); }
inline std::string ToString(long value) { return std::to_string(value); }
inline std::string ToString(long long value) { return std::to_string(value); }
inline std::string ToString(unsigned value) { return std::to_string(value); }
inline std::string ToString(unsigned long value) { return std::to_string(value); }
inline std::string ToString(unsigned long long value) {
  return std::to_string(value);
}
inline std::string ToString(float value) { return std::to_string(value); }
inline std::string ToString(double value) { return std::to_string(value); }

}  // namespace rtc

#endif  // RTC_BASE_STRING_UTILS_H_
