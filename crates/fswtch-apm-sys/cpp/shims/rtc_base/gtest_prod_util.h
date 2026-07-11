// fswtch-apm shim: minimal reimplementation of rtc_base/gtest_prod_util.h
//
// AEC3 helper code (e.g. audio_buffer.h) uses
// `FRIEND_TEST_ALL_PREFIXES(Suite, Case)` to grant gtest fixture friendship.
// There is no gtest in this build, so the macro expands to nothing.

#ifndef RTC_BASE_GTEST_PROD_UTIL_H_
#define RTC_BASE_GTEST_PROD_UTIL_H_

#define FRIEND_TEST_ALL_PREFIXES(test_suite_name, test_name)
#define FRIEND_TEST(test_suite_name, test_name)

#endif  // RTC_BASE_GTEST_PROD_UTIL_H_
