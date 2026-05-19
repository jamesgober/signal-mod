//! Customizing the `SignalSet`.
//!
//! Shows the three named constructors plus the builder-style
//! `with` / `without` operators. Every `SignalSet` constructor is
//! `const`, so default sets can be embedded in `static` items.
//!
//! Run with:
//!
//! ```text
//! cargo run --example custom_signal_set
//! ```

use signal_mod::{Signal, SignalSet};

const GRACEFUL_PLUS_USR1: SignalSet = SignalSet::graceful().with(Signal::User1);

fn main() {
    println!("preset constructors:");
    print_set("empty", SignalSet::empty());
    print_set("graceful (default)", SignalSet::graceful());
    print_set("standard", SignalSet::standard());
    print_set("all", SignalSet::all());

    println!("\nbuilder-style composition:");
    print_set("graceful + User1", GRACEFUL_PLUS_USR1);
    print_set(
        "standard without Quit",
        SignalSet::standard().without(Signal::Quit),
    );

    println!("\nplatform availability:");
    for sig in Signal::ALL {
        println!(
            "  {sig:?}: available_on_current_platform={}",
            sig.available_on_current_platform()
        );
    }
}

fn print_set(label: &str, set: SignalSet) {
    let names: Vec<String> = set.iter().map(|s| format!("{s:?}")).collect();
    println!("  {label}: [{}]", names.join(", "));
}
