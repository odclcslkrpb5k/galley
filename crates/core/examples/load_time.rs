//! How long `load` takes per file: `cargo run --release -p gbk-markdown-core --example load_time
//! -- FILE...`. The loader runs while GTK starts (docs/plan.md §4.4), so this has to stay well
//! under GTK's start-up time to stay off the critical path.
#![expect(
    clippy::print_stdout,
    reason = "a measurement tool whose output is its report"
)]

use std::time::{Duration, Instant};

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(bytes) = std::fs::read(&path) else {
            println!("{path}: cannot read");
            continue;
        };
        let mut times: Vec<Duration> = (0..15)
            .map(|_| {
                let started = Instant::now();
                let doc = gbk_markdown_core::load(&bytes);
                let elapsed = started.elapsed();
                std::hint::black_box(doc);
                elapsed
            })
            .collect();
        times.sort_unstable();
        let doc = gbk_markdown_core::load(&bytes);
        let median = times.get(times.len() / 2).copied().unwrap_or_default();
        let min = times.first().copied().unwrap_or_default();
        println!(
            "{path}: {} KiB, {} leaves, load min {:.1} ms median {:.1} ms",
            bytes.len() / 1024,
            doc.leaves().len(),
            min.as_secs_f64() * 1000.0,
            median.as_secs_f64() * 1000.0,
        );
    }
}
