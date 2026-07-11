// fswtch-apm shim: minimal reimplementation of rtc_base/thread_annotations.h
//
// All clang thread-safety attributes are no-ops here. AEC3 annotates members
// like `RTC_GUARDED_BY(race_checker_)`; this expands to nothing.

#ifndef RTC_BASE_THREAD_ANNOTATIONS_H_
#define RTC_BASE_THREAD_ANNOTATIONS_H_

#define RTC_GUARDED_BY(x)
#define RTC_PT_GUARDED_BY(x)
#define RTC_ACQUIRED_BEFORE(x)
#define RTC_ACQUIRED_AFTER(x)
#define RTC_EXCLUSIVE_LOCKS_REQUIRED(...)
#define RTC_SHARED_LOCKS_REQUIRED(...)
#define RTC_LOCKABLE
#define RTC_SCOPED_LOCKABLE
#define RTC_EXCLUSIVE_LOCK_FUNCTION(...)
#define RTC_SHARED_LOCK_FUNCTION(...)
#define RTC_EXCLUSIVE_TRYLOCK_FUNCTION(...)
#define RTC_SHARED_TRYLOCK_FUNCTION(...)
#define RTC_UNLOCK_FUNCTION(...)
#define RTC_LOCK_RETURNED(x)
#define RTC_LOCKS_EXCLUDED(...)
#define RTC_ASSERT_EXCLUSIVE_LOCK(...)
#define RTC_NO_THREAD_SAFETY_ANALYSIS
#define RTC_THREAD_ANNOTATION_ATTRIBUTE(x)

#endif  // RTC_BASE_THREAD_ANNOTATIONS_H_
