//! Micro-benchmarks for the `signal-mod` hot paths and the shutdown
//! sequence.
//!
//! Run with:
//!
//! ```text
//! cargo bench --bench shutdown_bench
//! ```
//!
//! The README "Performance" table is produced from these benches; the
//! machine and conditions are recorded next to it. Rerun locally to
//! validate on your hardware.

use std::future::Future;
use std::hint::black_box;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Waker};
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

use signal_mod::{Coordinator, ShutdownReason, Signal, SignalSet, hook_from_fn};

fn bench_token_is_initiated(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    c.bench_function("token::is_initiated (uninitiated)", |b| {
        b.iter(|| black_box(&token).is_initiated());
    });
}

fn bench_token_reason(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    let token = coord.token();
    c.bench_function("token::reason (initiated)", |b| {
        b.iter(|| black_box(&token).reason());
    });
}

fn bench_trigger_first_time(c: &mut Criterion) {
    c.bench_function("trigger::first (transition)", |b| {
        b.iter_batched(
            || {
                let coord = Coordinator::builder().build();
                let trigger = coord.trigger();
                (coord, trigger)
            },
            |(coord, trigger)| {
                black_box(trigger.trigger(ShutdownReason::Requested));
                coord
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_trigger_redundant(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    let trigger = coord.trigger();
    assert!(trigger.trigger(ShutdownReason::Requested));
    c.bench_function("trigger::redundant (already initiated)", |b| {
        b.iter(|| black_box(&trigger).trigger(ShutdownReason::Requested));
    });
}

fn bench_clone_token(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    c.bench_function("token::clone", |b| {
        b.iter(|| black_box(&token).clone());
    });
}

fn bench_wait_future_ready(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    let token = coord.token();
    let mut cx = Context::from_waker(Waker::noop());
    c.bench_function("token::wait poll (initiated)", |b| {
        b.iter(|| {
            let mut fut = black_box(&token).wait();
            black_box(Pin::new(&mut fut).poll(&mut cx))
        });
    });
}

fn bench_run_hooks(c: &mut Criterion) {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut group = c.benchmark_group("run_hooks");
    for size in [1_usize, 4, 16, 64] {
        group.bench_function(format!("hooks={size}"), |b| {
            b.iter_batched(
                || {
                    let mut builder =
                        Coordinator::builder().graceful_timeout(Duration::from_secs(10));
                    for i in 0..size {
                        let c = Arc::clone(&counter);
                        builder = builder.hook(hook_from_fn(
                            format!("hook-{i}"),
                            i32::try_from(i).unwrap_or(i32::MAX),
                            move |_| {
                                c.fetch_add(1, Ordering::Relaxed);
                            },
                        ));
                    }
                    let coord = builder.build();
                    // Let the standby worker thread start and park, as it
                    // has long before shutdown in a real program.
                    std::thread::sleep(Duration::from_millis(2));
                    coord
                },
                |coord| {
                    black_box(coord.run_hooks(ShutdownReason::Requested));
                    coord
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn bench_statistics(c: &mut Criterion) {
    let mut builder = Coordinator::builder();
    for i in 0..16 {
        builder = builder.hook(hook_from_fn(format!("hook-{i}"), i, |_| {}));
    }
    let coord = builder.build();
    c.bench_function("coordinator::statistics (16 hooks)", |b| {
        b.iter(|| black_box(&coord).statistics());
    });
}

fn bench_signal_set_iter(c: &mut Criterion) {
    let set = SignalSet::all();
    c.bench_function("SignalSet::iter (all 7)", |b| {
        b.iter(|| {
            let mut last = Signal::Terminate;
            for sig in black_box(set) {
                last = sig;
            }
            last
        });
    });
}

fn bench_subscription_delivery(c: &mut Criterion) {
    let coord = Coordinator::builder()
        .signals(SignalSet::empty())
        .notify_signals(SignalSet::from(Signal::User1))
        .build();
    let mut sub = coord.subscribe();
    c.bench_function("subscription deliver + try_recv (1 subscriber)", |b| {
        b.iter(|| {
            black_box(coord.simulate_signal(Signal::User1));
            black_box(sub.try_recv())
        });
    });
}

fn bench_wait_blocking_timeout_short(c: &mut Criterion) {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    c.bench_function("wait_blocking_timeout (1us, not initiated)", |b| {
        b.iter(|| black_box(&token).wait_blocking_timeout(Duration::from_micros(1)));
    });
}

criterion_group!(
    benches,
    bench_token_is_initiated,
    bench_token_reason,
    bench_trigger_first_time,
    bench_trigger_redundant,
    bench_clone_token,
    bench_wait_future_ready,
    bench_run_hooks,
    bench_statistics,
    bench_signal_set_iter,
    bench_subscription_delivery,
    bench_wait_blocking_timeout_short,
);
criterion_main!(benches);
