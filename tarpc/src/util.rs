// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hash},
};
use web_time::{Duration, Instant};

#[cfg(feature = "serde1")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde1")))]
pub mod serde;

/// Extension trait for [Instants](Instant) in the future, i.e. deadlines.
pub trait TimeUntil {
    /// How much time from now until this time is reached.
    fn time_until(&self) -> Duration;
}

impl TimeUntil for Instant {
    fn time_until(&self) -> Duration {
        self.duration_since(Instant::now())
    }
}

/// Collection compaction; configurable `shrink_to_fit`.
pub trait Compact {
    /// Compacts space if the ratio of length : capacity is less than `usage_ratio_threshold`.
    fn compact(&mut self, usage_ratio_threshold: f64);
}

impl<K, V, H> Compact for HashMap<K, V, H>
where
    K: Eq + Hash,
    H: BuildHasher,
{
    fn compact(&mut self, usage_ratio_threshold: f64) {
        let usage_ratio_threshold = usage_ratio_threshold.clamp(f64::MIN_POSITIVE, 1.);
        let cap = f64::max(1000., self.len() as f64 / usage_ratio_threshold);
        self.shrink_to(cap as usize);
    }
}

#[test]
fn test_compact() {
    let mut map = HashMap::with_capacity(2048);
    assert_eq!(map.capacity(), 3584);

    // Make usage ratio 25%
    for i in 0..896 {
        map.insert(format!("k{i}"), "v");
    }

    map.compact(-1.0);
    assert_eq!(map.capacity(), 3584);

    map.compact(0.25);
    assert_eq!(map.capacity(), 3584);

    map.compact(0.50);
    assert_eq!(map.capacity(), 1792);

    map.compact(1.0);
    assert_eq!(map.capacity(), 1792);

    map.compact(2.0);
    assert_eq!(map.capacity(), 1792);
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use gloo_timers::callback::Timeout;
    use std::{
        cmp::Reverse,
        collections::BinaryHeap,
        task::{Poll, Waker},
    };
    use web_time::{Duration, Instant};

    #[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
    pub struct Key {
        index: usize,
    }

    impl Key {
        fn new(index: usize) -> Key {
            Self { index }
        }
    }

    #[derive(Debug)]
    pub struct Expired<T> {
        data: T,
        deadline: Instant,
        key: Key,
    }

    impl<T> Expired<T> {
        pub fn get_ref(&self) -> &T {
            &self.data
        }

        pub fn into_inner(self) -> T {
            self.data
        }
    }

    #[derive(Debug, Default)]
    pub struct DelayQueue<T: Ord> {
        heap: BinaryHeap<Reverse<(Instant, Key, T)>>,
        next: usize,
        timeout: Option<Timeout>,
        waker: Option<Waker>,
    }

    impl<T: Ord + Copy> DelayQueue<T> {
        pub fn insert(&mut self, value: T, timeout: Duration) -> Key {
            let key = Key::new(self.next);
            self.next += 1;
            self.heap
                .push(Reverse((Instant::now() + timeout, key, value)));
            self.set_timeout();
            key
        }

        fn set_timeout(&mut self) {
            if let Some(timeout) = self.timeout.take() {
                timeout.cancel();
            }
            if let Some(&Reverse((deadline, ..))) = self.heap.peek() {
                let now = Instant::now();
                let delay_ms = deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .min(u128::from(u32::MAX)) as u32;
                let waker = self.waker.clone();
                self.timeout = Some(Timeout::new(delay_ms, move || {
                    if let Some(waker) = waker.as_ref() {
                        waker.wake_by_ref();
                    }
                }));
            }
        }

        pub fn remove(&mut self, key: &Key) -> Expired<T> {
            let mut removed: Option<(Instant, Key, T)> = None;
            self.heap.retain(|reverse| {
                let entry = &reverse.0;
                if entry.1 == *key {
                    removed = Some(*entry);
                    false
                } else {
                    true
                }
            });
            match removed {
                Some(removed) => Expired {
                    data: removed.2,
                    deadline: removed.0,
                    key: removed.1,
                },
                None => panic!("invalid key"),
            }
        }

        pub fn poll_expired(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Option<Expired<T>>> {
            self.waker = Some(cx.waker().clone());
            let now = Instant::now();
            if let Some(Reverse((deadline, _, _))) = self.heap.peek() {
                if *deadline <= now {
                    let Reverse((deadline, key, data)) = self.heap.pop().unwrap();
                    self.set_timeout();
                    return Poll::Ready(Some(Expired {
                        data,
                        deadline,
                        key,
                    }));
                }
            }
            Poll::Pending
        }

        pub fn is_empty(&self) -> bool {
            self.heap.is_empty()
        }

        pub fn clear(&mut self) {
            self.timeout.take().map(|timeout| timeout.cancel());
            self.heap.clear();
        }
    }
}
