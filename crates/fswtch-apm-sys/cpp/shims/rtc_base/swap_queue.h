// fswtch-apm shim: minimal reimplementation of rtc_base/swap_queue.h
//
// A single-producer/single-consumer fixed-size queue that moves items by
// std::swap(). EchoCanceller3 instantiates
// `SwapQueue<std::vector<std::vector<std::vector<float>>>,
//            Aec3RenderQueueItemVerifier>`
// and calls `Insert(&x)` / `Remove(&x)` / `Clear()`. The verifier is a
// caller-supplied functor `bool operator()(const T&) const`.
//
// This implementation follows the upstream API (two template params with the
// second defaulting to a trivial verifier; four constructors; Insert/Remove/
// Clear) and uses std::atomic for the producer/consumer indices.

#ifndef RTC_BASE_SWAP_QUEUE_H_
#define RTC_BASE_SWAP_QUEUE_H_

#include <atomic>
#include <cstddef>
#include <utility>
#include <vector>

#include "rtc_base/checks.h"

namespace webrtc {

// Default verifier: accepts everything.
template <typename T>
class SwapQueueItemVerifier {
 public:
  bool operator()(const T&) const { return true; }
};

template <typename T, typename QueueItemVerifier = SwapQueueItemVerifier<T>>
class SwapQueue {
 public:
  explicit SwapQueue(size_t capacity) : SwapQueue(capacity, QueueItemVerifier()) {}

  SwapQueue(size_t capacity, const QueueItemVerifier& verifier)
      : verifier_(verifier), queue_(capacity) {}

  SwapQueue(size_t capacity, const T& prototype)
      : SwapQueue(capacity, prototype, QueueItemVerifier()) {}

  SwapQueue(size_t capacity,
            const T& prototype,
            const QueueItemVerifier& verifier)
      : verifier_(verifier), queue_(capacity, prototype) {
    RTC_DCHECK_GT(capacity, 0u);
  }

  SwapQueue(const SwapQueue&) = delete;
  SwapQueue& operator=(const SwapQueue&) = delete;

  // Moves *input into the queue by swapping it with an "empty" slot. Returns
  // false if the queue was full (item not inserted).
  bool Insert(T* input) {
    RTC_DCHECK(input);
    RTC_DCHECK(verifier_(*input));
    const size_t num = num_elements_.load(std::memory_order_relaxed);
    if (num >= queue_.size()) {
      return false;
    }
    using std::swap;
    swap(*input, queue_[next_write_index_]);
    next_write_index_ = (next_write_index_ + 1) % queue_.size();
    num_elements_.fetch_add(1, std::memory_order_acq_rel);
    return true;
  }

  // Moves the frontmost "full" item out by swapping it with *output. Returns
  // false if the queue was empty.
  bool Remove(T* output) {
    RTC_DCHECK(output);
    RTC_DCHECK(verifier_(*output));
    const size_t num = num_elements_.load(std::memory_order_relaxed);
    if (num == 0) {
      return false;
    }
    using std::swap;
    swap(*output, queue_[next_read_index_]);
    next_read_index_ = (next_read_index_ + 1) % queue_.size();
    num_elements_.fetch_sub(1, std::memory_order_acq_rel);
    return true;
  }

  // Drops all pending items. Only safe to call when there is no concurrent
  // producer/consumer activity.
  void Clear() {
    next_read_index_ = 0;
    next_write_index_ = 0;
    num_elements_.store(0, std::memory_order_relaxed);
  }

  size_t Size() const {
    return num_elements_.load(std::memory_order_relaxed);
  }

 private:
  QueueItemVerifier verifier_;
  std::vector<T> queue_;
  size_t next_read_index_ = 0;
  size_t next_write_index_ = 0;
  std::atomic<size_t> num_elements_{0};
};

}  // namespace webrtc

#endif  // RTC_BASE_SWAP_QUEUE_H_
