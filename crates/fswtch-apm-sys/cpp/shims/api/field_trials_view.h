// fswtch-apm shim: api/field_trials_view.h
//
// Abstract field-trials interface. AEC3 queries `field_trials.Lookup(name)`,
// `.IsEnabled(name)`, `.IsDisabled(name)`. The no-trials default (Lookup
// returns "") yields IsEnabled()==false and IsDisabled()==false for every key,
// so AEC3 selects the traditional/default config path — exactly what this
// scalar, no-neural build wants.

#ifndef API_FIELD_TRIALS_VIEW_H_
#define API_FIELD_TRIALS_VIEW_H_

#include <memory>
#include <string>

#include "absl/strings/string_view.h"

namespace webrtc {

class FieldTrialsView {
 public:
  virtual ~FieldTrialsView() = default;

  // Returns the raw value of the named field trial (empty when not set).
  virtual std::string Lookup(absl::string_view key) const { return ""; }

  // Non-virtual helpers mirroring upstream: a key is "enabled"/"disabled" when
  // its Lookup value starts with "Enabled"/"Disabled".
  bool IsEnabled(absl::string_view key) const {
    return StartsWith(Lookup(key), "Enabled");
  }
  bool IsDisabled(absl::string_view key) const {
    return StartsWith(Lookup(key), "Disabled");
  }

  virtual bool IsTest() const { return false; }
  virtual std::unique_ptr<FieldTrialsView> CreateCopy() const { return nullptr; }

 private:
  static bool StartsWith(const std::string& s, const char* prefix) {
    return s.rfind(prefix, 0) == 0;
  }
};

}  // namespace webrtc

#endif  // API_FIELD_TRIALS_VIEW_H_
