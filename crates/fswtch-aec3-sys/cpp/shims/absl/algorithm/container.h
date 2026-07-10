// fswtch-aec3 shim: absl/algorithm/container.h
//
// The real-vendored `api/audio/audio_view.h` calls `absl::c_fill(view, 0)` in
// `DeinterleavedView::Clear()`. Provide just the subset of container algorithms
// the vendored closure uses. (Start with c_fill; extend if the compiler asks
// for more.)

#ifndef ABSL_ALGORITHM_CONTAINER_H_
#define ABSL_ALGORITHM_CONTAINER_H_

#include <algorithm>
#include <iterator>

namespace absl {

template <typename C, typename T>
void c_fill(C& c, const T& value) {
  std::fill(std::begin(c), std::end(c), value);
}

}  // namespace absl

#endif  // ABSL_ALGORITHM_CONTAINER_H_
