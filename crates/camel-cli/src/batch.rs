//! Page threads share model weights and coordinate inference parallelism.
use std::{
    path::Path,
    sync::Mutex,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
};

use anyhow::Result;

pub(crate) fn worker_count(requested: Option<usize>, pages: usize) -> usize {
    requested.unwrap_or(2).max(1).min(pages.max(1))
}

pub(crate) fn analyze(
    source: &Mutex<camel::comic::ComicSource>,
    pages: usize,
    output: &Path,
    engine: &camel_vision::Engine,
    jobs: usize,
    quiet: bool,
    completed: &[bool],
) -> Result<()> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    let local = (jobs >= 4).then(|| engine.with_threads(1));
                    let engine = local.as_ref().unwrap_or(engine);
                    while !failed.load(Ordering::Relaxed) {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= pages {
                            break;
                        }
                        if completed[i] {
                            continue;
                        }
                        let result = crate::project::analyze_one(
                            source,
                            output,
                            i + 1,
                            pages,
                            quiet,
                            engine,
                        );
                        if result.is_err() {
                            failed.store(true, Ordering::Relaxed);
                        }
                        result?;
                    }
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("analysis thread panicked"))??;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_defaults_to_two_and_is_limited_to_page_count() {
        assert_eq!(worker_count(None, usize::MAX), 2);
        assert_eq!(worker_count(None, 1), 1);
        assert_eq!(worker_count(Some(12), 3), 3);
        assert_eq!(worker_count(Some(1), 20), 1);
    }
}
