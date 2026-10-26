//! Runs the programs of the integration tests in parallel. `oxie run` builds every script in
//! `$OXIE_CACHE/target`, and cargo builds one crate at a time in a target directory, so the
//! programs build in `workers()` cache directories under `target/tmp/oxie-cache`. An item always
//! builds in the same one: every script keeps a build script of its own (about 10 MB) in the
//! target directory, so a program built in each of them in turn takes that space in all of them.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

/// The cache directories, each locked while a program builds in it. The tests of a binary run at
/// the same time and share them, so no more than `workers()` programs build at once.
static CACHES: OnceLock<Vec<Mutex<PathBuf>>> = OnceLock::new();

fn caches() -> &'static [Mutex<PathBuf>] {
    CACHES.get_or_init(|| (0..workers()).map(|i| Mutex::new(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("oxie-cache").join(i.to_string()))).collect())
}

/// Calls `run` on every item, on up to `available_parallelism()` threads, with the directory to
/// pass as `OXIE_CACHE` (the same one for the same item every time), and returns the results in
/// the order of `items`. A panic in `run` is raised again here, the first one in the order of
/// `items`.
pub fn parallel<T: Hash + Sync, R: Send>(items: &[T], run: impl Fn(&T, &Path) -> R + Sync) -> Vec<R> {
    let caches = caches();
    // the items of each cache directory, by index
    let mut groups: Vec<(usize, Vec<usize>)> = (0..caches.len()).map(|c| (c, Vec::new())).collect();
    for (i, item) in items.iter().enumerate() {
        let mut h = DefaultHasher::new();
        item.hash(&mut h);
        groups[(h.finish() % caches.len() as u64) as usize].1.push(i);
    }
    groups.retain(|g| !g.1.is_empty());
    let next = AtomicUsize::new(0);
    let mut results = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..groups.len())
            .map(|_| {
                // named like the test, as the panic messages of the test's own thread are
                let thread = std::thread::Builder::new().name(std::thread::current().name().unwrap_or("test").to_string());
                thread
                    .spawn_scoped(s, || {
                        let mut done = Vec::new();
                        loop {
                            let Some((c, group)) = groups.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                            let cache = caches[*c].lock().unwrap_or_else(PoisonError::into_inner);
                            for &i in group {
                                done.push((i, catch_unwind(AssertUnwindSafe(|| run(&items[i], &cache)))));
                            }
                        }
                        done
                    })
                    .unwrap()
            })
            .collect();
        for h in handles {
            results.extend(h.join().unwrap());
        }
    });
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r.unwrap_or_else(|p| resume_unwind(p))).collect()
}
