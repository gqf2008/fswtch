// fswtch-apm shim: absl/strings/string_view.h
//
// AEC3 uses absl::string_view as a drop-in for std::string_view. Aliasing the
// std type avoids pulling in abseil. absl::string_view is constructible from
// std::string and const char*, and supports data()/size()/substr() — all of
// which std::string_view provides.

#ifndef ABSL_STRINGS_STRING_VIEW_H_
#define ABSL_STRINGS_STRING_VIEW_H_

#include <string_view>
#include <string>

namespace absl {

using string_view = std::string_view;

}  // namespace absl

#endif  // ABSL_STRINGS_STRING_VIEW_H_
