//! A frame's independent work split across cores. A frame that follows the
//! TUI's sleep between ticks runs on cooled cores, several times slower than
//! the same frame run back to back (`examples/pacing.rs`'s paced and hot
//! runs), so work a frame can split, it splits, on threads that all start at
//! once. Each takes the next item as it finishes the last, the calling thread
//! among them: Apple silicon's efficiency cores finish a static share last,
//! and Apple's advice is to "distribute tasks dynamically" over more tasks
//! than cores
//! (<https://developer.apple.com/documentation/apple-silicon/tuning-your-code-s-performance-for-apple-silicon>).
//! Not rayon: its pool wakes a sleeping worker per job it pushes, and "may
//! fail to wake any new threads" (rayon-core 1.13 `sleep/mod.rs:223-230`), so
//! after a sleep a frame's work can wait on a few cold workers.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The cores this process may run on; one where the platform can't say,
/// as wasm32 can't, and one instrumented for coverage, where a frame's work
/// ran slower split across threads than on one: the work stays on the calling
/// thread there.
pub fn cores() -> usize {
    if cfg!(coverage) {
        1
    } else {
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    }
}

/// `f` of each of `items`, in order, on up to `threads` threads, this one
/// among them; on this thread alone when that is one.
pub fn map<T: Sync, U: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> U + Sync) -> Vec<U> {
    let threads = threads.min(items.len()).max(1);
    if threads == 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else {
                return done;
            };
            done.push((i, f(item)));
        }
    };
    let mut out: Vec<Option<U>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    std::thread::scope(|s| {
        let helpers: Vec<_> = (1..threads).map(|_| s.spawn(work)).collect();
        let mine = work();
        let theirs = helpers.into_iter().flat_map(|helper| {
            helper
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
        for (i, u) in theirs.chain(mine) {
            out[i] = Some(u);
        }
    });
    // Each index is taken once, so every slot is filled.
    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split or not, the results come back in the items' order, one each.
    #[test]
    fn a_split_keeps_the_items_order() {
        let items: Vec<u32> = (0..37).collect();
        for threads in [0, 1, 2, 3, 8, 64] {
            assert_eq!(
                map(&items, threads, |&i| i * 2),
                items.iter().map(|&i| i * 2).collect::<Vec<_>>(),
                "{threads} threads"
            );
        }
        assert!(map(&[] as &[u32], 4, |&i| i).is_empty());
    }

    /// Each of the threads asked for runs the work, this one among them.
    #[test]
    fn a_split_runs_on_the_threads_asked_for() {
        use std::time::{Duration, Instant};
        let items: Vec<u32> = (0..8).collect();
        // The first four meet before any goes on, so four threads hold them;
        // the deadline turns a split that can't meet into a failure, not a hang.
        let met = AtomicUsize::new(0);
        let until = Instant::now() + Duration::from_secs(5);
        let ids: std::collections::HashSet<_> = map(&items, 4, |&i| {
            if i < 4 {
                met.fetch_add(1, Ordering::Relaxed);
                while met.load(Ordering::Relaxed) < 4 && Instant::now() < until {
                    std::thread::yield_now();
                }
            }
            std::thread::current().id()
        })
        .into_iter()
        .collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&std::thread::current().id()));
    }

    /// Instrumented for coverage, a frame's work stays on this thread.
    #[test]
    #[cfg(coverage)]
    fn an_instrumented_frame_splits_nothing() {
        assert_eq!(cores(), 1);
    }

    /// A slow item holds back no other: the threads free take them all.
    #[test]
    fn a_slow_item_holds_back_no_other() {
        use std::time::{Duration, Instant};
        let items: Vec<u32> = (0..8).collect();
        let others = AtomicUsize::new(0);
        let ran = map(&items, 2, |&i| {
            if i == 0 {
                let until = Instant::now() + Duration::from_secs(5);
                while others.load(Ordering::Relaxed) < items.len() - 1 && Instant::now() < until {
                    std::thread::yield_now();
                }
            } else {
                others.fetch_add(1, Ordering::Relaxed);
            }
            std::thread::current().id()
        });
        assert!(
            ran[1..].iter().all(|id| *id != ran[0]),
            "the slow item's thread took no other"
        );
    }
}
