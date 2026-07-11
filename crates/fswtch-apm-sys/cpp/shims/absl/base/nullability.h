// fswtch-apm shim: absl/base/nullability.h
//
// The real abseil header carries nullability annotations (ABSL_ATTRIBUTE_...).
// The vendored AEC3 closure (after the two big roots are shimmed) does not use
// any nullability macros; this file is provided empty so transitive includes
// resolve.

#ifndef ABSL_BASE_NULLABILITY_H_
#define ABSL_BASE_NULLABILITY_H_

// No-op aliases for the common abseil nullability macros, in case any
// transitive include references them. Both the UPPERCASE attribute macros and
// the lowercase annotation macros (used as `absl_nonnull std::unique_ptr<...>`
// return-type prefixes in echo_control.h) expand to nothing.
#define ABSL_ATTRIBUTE_LIFETIME_BOUND
#define ABSL_NULLABLE
#define ABSL_NONNULL
#define absl_nullable
#define absl_nonnull

namespace absl {

}  // namespace absl

#endif  // ABSL_BASE_NULLABILITY_H_
