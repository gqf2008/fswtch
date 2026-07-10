// fswtch-aec3 shim: minimal reimplementation of rtc_base/race_checker.h
//
// EchoCanceller3 holds a `RaceChecker capture_race_checker_` and annotates
// members with RTC_GUARDED_BY(capture_race_checker_). Race checking is a
// best-effort debug aid; in this scalar/embedded build it is a no-op.

#ifndef RTC_BASE_RACE_CHECKER_H_
#define RTC_BASE_RACE_CHECKER_H_

namespace webrtc {

class RaceChecker {
 public:
  RaceChecker() = default;
  RaceChecker(const RaceChecker&) = delete;
  RaceChecker& operator=(const RaceChecker&) = delete;
  // Used by RTC_(D)CHECK_RUN_SERIALIZED — always "acquired" (no-op).
  bool Acquire() const { return true; }
  ~RaceChecker() = default;
};

}  // namespace webrtc

#define RTC_CHECK_RUNS_SERIALIZED(race_checker) (void)0
#define RTC_DCHECK_RUNS_SERIALIZED(race_checker) (void)0

#endif  // RTC_BASE_RACE_CHECKER_H_
