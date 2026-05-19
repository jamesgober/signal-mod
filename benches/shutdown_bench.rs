//! Realistic micro-benchmarks for the `signal-mod` hot paths.
//!
//! Run with:
//!
//! ```text
//! cargo bench --bench shutdown_bench
//! ```
//!
//! Numbers in `docs/API.md` Performance section are derived from
//! these benches on the reference platform; rerun locally to validate
//! on yours.

use std::sync::Arc;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, Signal, SignalSet};

fn bench_token_is_initiated(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    c.bench_function("token::is_initiated (uninitiated)", |b| {
        b.iter(|| black_box(token.is_initiated()));
    });
}

fn bench_trigger_first_time(c: &mut Criterion) {
    c.bench_function("trigger::first (transition)", |b| {
        b.iter_with_setup(
            || Coordinator::builder().build(),
            |coord| {
                let trig = coord.trigger();
                black_box(trig.trigger(ShutdownReason::Requested));
            },
        );
    });
}

fn bench_trigger_redundant(c: &mut Criterion) {
    c.bench_function("trigger::redundant (already initiated)", |b| {
        let coord = Coordinator::builder().build();
        let trig = coord.trigger();
        assert!(trig.trigger(ShutdownReason::Requested));
        b.iter(|| black_box(trig.trigger(ShutdownReason::Requested)));
    });
}

fn bench_clone_token(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    c.bench_function("token::clone", |b| {
        b.iter(|| black_box(token.clone()));
    });
}

fn bench_run_hooks(c: &mut Criterion) {
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut group = c.benchmark_group("run_hooks");
    for size in [1usize, 4, 16, 64] {
        let coord = {
            let mut builder = Coordinator::builder().graceful_timeout(Duration::from_secs(10));
            for i in 0..size {
                let c = Arc::clone(&counter);
                builder = builder.hook(hook_from_fn(
                    format!("hook-{i}"),
                    i32::try_from(i).unwrap_or(i32::MAX),
                    move |_| {
                        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    },
                ));
            }
            builder.build()
        };

        group.bench_function(format!("hooks={size}"), |b| {
            b.iter(|| {
                let count = coord.run_hooks(black_box(ShutdownReason::Requested));
                black_box(count);
            });
        });
    }
    group.finish();
}

fn bench_signal_set_iter(c: &mut Criterion) {
    c.bench_function("SignalSet::iter (all 7)", |b| {
        let set = SignalSet::all();
        b.iter(|| {
            let mut last = Signal::Terminate;
            for sig in set {
                last = sig;
            }
            black_box(last);
        });
    });
}

fn bench_wait_blocking_timeout_short(c: &mut Criterion) {
    c.bench_function("wait_blocking_timeout (1us, not initiated)", |b| {
        let coord = Coordinator::builder().build();
        let token = coord.token();
        b.iter(|| {
            let observed = token.wait_blocking_timeout(Duration::from_micros(1));
            black_box(observed);
        });
    });
}

criterion_group!(
    benches,
    bench_token_is_initiated,
    bench_trigger_first_time,
    bench_trigger_redundant,
    bench_clone_token,
    bench_run_hooks,
    bench_signal_set_iter,
    bench_wait_blocking_timeout_short,
);
criterion_main!(benches);
