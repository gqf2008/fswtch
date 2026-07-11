// fswtch-apm shim: system_wrappers/include/metrics.h
//
// AEC3 instruments a few code paths with RTC_HISTOGRAM_* macros (counts,
// enumeration, boolean). These feed UMA-style telemetry in the full build.
// For this embedded scalar build they are no-ops. The macros are defined so
// the instrumented call sites compile; the "histogram" expression is discarded.

#ifndef SYSTEM_WRAPPERS_INCLUDE_METRICS_H_
#define SYSTEM_WRAPPERS_INCLUDE_METRICS_H_

#include <cstddef>
#include <cstdint>

#include "absl/strings/string_view.h"

// fswtch-apm shim addition for AGC2: the vendored agc2/interpolated_gain_curve
// translation unit references the metrics API directly (not only via the
// RTC_HISTOGRAM_* macros below) — `metrics::Histogram*` members and the
// `HistogramFactoryGetCounts` / `HistogramAdd` calls. In the full build these
// feed UMA-style telemetry; for this embedded scalar build they are no-ops
// (the factory returns nullptr, Add is empty). The Histogram type is opaque —
// only pointer members hold it.
namespace webrtc {
namespace metrics {
class Histogram;
inline Histogram* HistogramFactoryGetCounts(absl::string_view /*name*/,
                                             int /*min*/,
                                             int /*max*/,
                                             int /*bucket_count*/) {
  return nullptr;
}
inline void HistogramAdd(Histogram* /*histogram_pointer*/, int /*sample*/) {}
}  // namespace metrics
}  // namespace webrtc


#define RTC_HISTOGRAM_COUNTS_LINEAR(name, sample, min, max, bucket_count) \
  do {                                                                      \
    (void)(name);                                                           \
    (void)(sample);                                                         \
    (void)(min);                                                            \
    (void)(max);                                                            \
    (void)(bucket_count);                                                   \
  } while (0)

#define RTC_HISTOGRAM_COUNTS(name, sample, min, max, bucket_count) \
  RTC_HISTOGRAM_COUNTS_LINEAR(name, sample, min, max, bucket_count)

#define RTC_HISTOGRAM_ENUMERATION(name, sample, boundary) \
  do {                                                    \
    (void)(name);                                         \
    (void)(sample);                                       \
    (void)(boundary);                                     \
  } while (0)

#define RTC_HISTOGRAM_BOOLEAN(name, sample) \
  do {                                       \
    (void)(name);                            \
    (void)(sample);                          \
  } while (0)

#define RTC_HISTOGRAM_PERCENTAGE(name, percentage) \
  RTC_HISTOGRAM_COUNTS_LINEAR(name, percentage, 0, 100, 50)

// Helpers that some call sites use to define a histogram sample variable.
#define RTC_HISTOGRAM_COMMON(name, sample) \
  do {                                       \
    (void)(name);                            \
    (void)(sample);                          \
  } while (0)

#endif  // SYSTEM_WRAPPERS_INCLUDE_METRICS_H_
