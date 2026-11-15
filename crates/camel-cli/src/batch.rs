//! Page threads share model weights and coordinate inference parallelism.
use std::{
    sync::Mutex,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
};

use anyhow::Result;

use crate::{host::FsHost, project::LockedComic};

pub(crate) fn worker_count(requested: Option<usize>, pages: usize) -> usize {
    requested.unwrap_or(2).max(1).min(pages.max(1))
}

pub(crate) fn analyze(
    source: &Mutex<camel::comic::ComicSource>,
    pages: usize,
    host: &FsHost,
    engine: &camel_vision::Engine,
    jobs: usize,
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
                    let mut comic = LockedComic(source);
                    while !failed.load(Ordering::Relaxed) {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= pages {
                            break;
                        }
                        if completed[i] {
                            continue;
                        }
                        let result =
                            futures::executor::block_on(camel_project::analyze::analyze_page(
                                host,
                                engine,
                                &mut comic,
                                i + 1,
                                pages,
                            ));
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
            handle.join().map_err(|payload| {
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown panic");
                anyhow::anyhow!("analysis thread panicked: {message}")
            })??;
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
