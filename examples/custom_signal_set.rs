//! Building `SignalSet` values.
//!
//! Every constructor is `const`, so sets can live in `const` / `static`
//! items, and sets combine with `|`, `&` and `-`.
//!
//! Run with:
//!
//! ```text
//! cargo run --example custom_signal_set
//! ```

use signal_mod::{Signal, SignalSet};

const RELOAD: SignalSet = SignalSet::empty().with(Signal::Hangup).with(Signal::User1);
const SHUTDOWN: SignalSet = SignalSet::standard().difference(RELOAD);

fn main() {
    println!("presets:");
    println!("  empty    {:?}", SignalSet::empty());
    println!("  graceful {:?}", SignalSet::graceful());
    println!("  standard {:?}", SignalSet::standard());
    println!("  all      {:?}", SignalSet::all());

    println!("\ncomposition:");
    println!("  reload            {RELOAD:?}");
    println!("  shutdown          {SHUTDOWN:?}");
    println!("  shutdown | reload {:?}", SHUTDOWN | RELOAD);
    let collected: SignalSet = [Signal::Quit, Signal::Pipe].into_iter().collect();
    println!("  collected         {collected:?}");

    println!("\nthis platform ({}):", std::env::consts::OS);
    for sig in Signal::ALL {
        println!(
            "  {:<9} number={:<10} exit_code={:<4} deliverable={}",
            format!("{sig:?}"),
            format!("{:?}", sig.unix_number()),
            sig.exit_code(),
            sig.available_on_current_platform(),
        );
    }
}
