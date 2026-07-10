// fswtch-aec3 shim: api/environment/environment.h  (replaces the real 36KB root)
//
// The real Environment bundles references to Clock, RtcEventLog, TaskQueueFactory
// and FieldTrialsView, dragging in a huge fan-out. The AEC3 closure only ever
// reads `env.field_trials()` (and stores a copy of `env`); nothing in the
// vendored AEC3 .cc/.h touches `env.clock()` or the other utilities. This shim
// provides a minimal Environment that holds a `const FieldTrialsView&` and
// exposes `field_trials()`. It is copy-constructible (so `const Environment env_`
// member-init `env_(env)` works) but not copy-assignable — matching the
// by-value-storage semantics the real type documents.

#ifndef API_ENVIRONMENT_ENVIRONMENT_H_
#define API_ENVIRONMENT_ENVIRONMENT_H_

#include "api/field_trials_view.h"

namespace webrtc {

class Environment {
 public:
  explicit Environment(const FieldTrialsView& field_trials)
      : field_trials_(field_trials) {}

  // Copyable (reference member → copy-constructible, not assignable). This is
  // sufficient for `const Environment env_;` members initialized from a param.
  Environment(const Environment&) = default;
  Environment& operator=(const Environment&) = delete;

  const FieldTrialsView& field_trials() const { return field_trials_; }

 private:
  const FieldTrialsView& field_trials_;
};

}  // namespace webrtc

#endif  // API_ENVIRONMENT_ENVIRONMENT_H_
