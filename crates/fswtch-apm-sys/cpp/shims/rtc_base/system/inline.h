// fswtch-apm shim: rtc_base/system/inline.h
//
// Provides RTC_INLINE / RTC_FORCE_INLINE / RTC_NOINLINE attributes. Not used
// by the vendored AEC3 closure currently, but provided for completeness so
// any transitive include resolves.

#ifndef RTC_BASE_SYSTEM_INLINE_H_
#define RTC_BASE_SYSTEM_INLINE_H_

#define RTC_INLINE inline
#define RTC_FORCE_INLINE inline
#define RTC_NOINLINE

#endif  // RTC_BASE_SYSTEM_INLINE_H_
