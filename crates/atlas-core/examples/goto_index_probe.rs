//! Probe the same streaming local index used by the finder, without opening a window.
use atlas_core::search::nucleo_pool::NucleoPool;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 3,
        "usage: goto_index_probe QUERY ROOT EXPECTED_PATH [TIMEOUT_SECONDS] [scoped]"
    );
    let root = PathBuf::from(&args[1]);
    let expected = PathBuf::from(&args[2]);
    anyhow::ensure!(
        expected.is_dir(),
        "expected folder does not exist: {}",
        expected.display()
    );
    let timeout = args.get(3).map_or(Ok(15_u64), |value| value.parse())?;
    let started = Instant::now();
    let mut pool = NucleoPool::new();
    if args.get(4).is_some_and(|mode| mode == "scoped") {
        atlas_core::fs::index::build_index_scoped(root, pool.injector());
    } else {
        atlas_core::fs::index::build_index(root, pool.injector());
    }
    pool.set_pattern(&args[0]);
    loop {
        pool.tick();
        let results = pool.results(100);
        if let Some(rank) = results.iter().position(|entry| entry.path == expected) {
            println!(
                "FOUND {} at rank {} in {:.3}s ({} indexed entries)",
                expected.display(),
                rank + 1,
                started.elapsed().as_secs_f64(),
                pool.nucleo.snapshot().item_count()
            );
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(timeout) {
            for entry in results.iter().take(8) {
                println!("{}", entry.path.display());
            }
            anyhow::bail!(
                "folder absent from first 100 matches after {timeout}s ({} indexed entries)",
                pool.nucleo.snapshot().item_count()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
