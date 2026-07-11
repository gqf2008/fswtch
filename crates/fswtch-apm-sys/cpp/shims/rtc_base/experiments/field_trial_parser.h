// fswtch-apm shim: minimal reimplementation of rtc_base/experiments/field_trial_parser.h
//
// AEC3's AdjustConfig uses `FieldTrialParameter<double>("", default)` /
// `FieldTrialParameter<int>("key", default)` then `ParseFieldTrial({&p}, str)`
// then `p.Get()`. With the (shim) FieldTrialsView returning "" for every
// Lookup(), ParseFieldTrial finds nothing to parse and every parameter keeps
// its default value — i.e. the default AEC3 config is used unchanged. This
// shim implements the standard "Key:Value/Key:Value" parsing so it would also
// handle real trial strings, but the no-trials path is what matters here.

#ifndef RTC_BASE_EXPERIMENTS_FIELD_TRIAL_PARSER_H_
#define RTC_BASE_EXPERIMENTS_FIELD_TRIAL_PARSER_H_

#include <cstdint>
#include <cstdlib>
#include <initializer_list>
#include <optional>
#include <string>
#include <vector>

#include "absl/strings/string_view.h"

namespace webrtc {

// Primary template: unknown type → no parse (returns nullopt).
template <typename T>
inline std::optional<T> ParseTypedParameter(absl::string_view) {
  return std::nullopt;
}
template <>
inline std::optional<double> ParseTypedParameter<double>(absl::string_view str) {
  if (str.empty()) return std::nullopt;
  try {
    size_t end = 0;
    double v = std::stod(std::string(str), &end);
    if (end == 0) return std::nullopt;
    return v;
  } catch (...) {
    return std::nullopt;
  }
}
template <>
inline std::optional<float> ParseTypedParameter<float>(absl::string_view str) {
  auto v = ParseTypedParameter<double>(str);
  if (v) return static_cast<float>(*v);
  return std::nullopt;
}
template <>
inline std::optional<int> ParseTypedParameter<int>(absl::string_view str) {
  if (str.empty()) return std::nullopt;
  try {
    size_t end = 0;
    int v = static_cast<int>(std::stoi(std::string(str), &end, 0));
    if (end == 0) return std::nullopt;
    return v;
  } catch (...) {
    return std::nullopt;
  }
}
template <>
inline std::optional<unsigned> ParseTypedParameter<unsigned>(absl::string_view str) {
  if (str.empty()) return std::nullopt;
  try {
    size_t end = 0;
    unsigned v = static_cast<unsigned>(std::stoul(std::string(str), &end, 0));
    if (end == 0) return std::nullopt;
    return v;
  } catch (...) {
    return std::nullopt;
  }
}
template <>
inline std::optional<bool> ParseTypedParameter<bool>(absl::string_view str) {
  if (str == "true" || str == "1" || str == "True") return true;
  if (str == "false" || str == "0" || str == "False") return false;
  return std::nullopt;
}

class FieldTrialParameterInterface {
 public:
  explicit FieldTrialParameterInterface(absl::string_view key) : key_(key) {}
  virtual ~FieldTrialParameterInterface() = default;

  const std::string& Key() const { return key_; }

  friend void ParseFieldTrial(
      std::initializer_list<FieldTrialParameterInterface*> fields,
      absl::string_view trial_string);

  void MarkAsUsed() { used_ = true; }

  virtual bool Parse(std::optional<std::string> str_value) = 0;
  virtual void ParseDone() {}

  std::vector<FieldTrialParameterInterface*> sub_parameters_;

 private:
  std::string key_;
  bool used_ = false;
};

template <typename T>
class FieldTrialParameter : public FieldTrialParameterInterface {
 public:
  FieldTrialParameter(absl::string_view key, T default_value)
      : FieldTrialParameterInterface(key), value_(default_value) {}

  T Get() const { return value_; }
  operator T() const { return value_; }

  bool Parse(std::optional<std::string> str_value) override {
    if (!str_value) {
      return false;
    }
    std::optional<T> parsed = ParseTypedParameter<T>(*str_value);
    if (parsed) {
      value_ = *parsed;
      return true;
    }
    return false;
  }

 private:
  T value_;
};

// Parses `trial_string` of the form "Key1:Value1/Key2:Value2/..." and, for
// each field whose Key() matches a parsed key, calls field->Parse(value).
// Defined inline here so no separate .cc is needed and no undefined reference
// to ParseFieldTrial remains.
inline void ParseFieldTrial(
    std::initializer_list<FieldTrialParameterInterface*> fields,
    absl::string_view trial_string) {
  if (trial_string.empty()) {
    return;
  }
  const std::string s(trial_string);
  size_t pos = 0;
  while (pos < s.size()) {
    size_t sep = s.find('/', pos);
    size_t seg_end = (sep == std::string::npos) ? s.size() : sep;
    std::string segment = s.substr(pos, seg_end - pos);
    pos = (sep == std::string::npos) ? s.size() : sep + 1;
    size_t colon = segment.find(':');
    std::string key =
        (colon == std::string::npos) ? segment : segment.substr(0, colon);
    std::string value =
        (colon == std::string::npos) ? std::string() : segment.substr(colon + 1);
    for (FieldTrialParameterInterface* field : fields) {
      if (field->Key() == key) {
        field->Parse(std::optional<std::string>(value));
        field->MarkAsUsed();
      }
    }
  }
  for (FieldTrialParameterInterface* field : fields) {
    field->ParseDone();
  }
}

}  // namespace webrtc

#endif  // RTC_BASE_EXPERIMENTS_FIELD_TRIAL_PARSER_H_
