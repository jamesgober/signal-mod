<h1 align="center">
    <strong>mod-signal</strong>
    <br>
    <sup><sub>CROSS-PLATFORM OS SIGNAL HANDLING FOR RUST</sub></sup>
</h1>

<p align="center">
    <a href="https://crates.io/crates/mod-signal"><img alt="crates.io" src="https://img.shields.io/crates/v/mod-signal.svg"></a>
    <a href="https://crates.io/crates/mod-signal"><img alt="downloads" src="https://img.shields.io/crates/d/mod-signal.svg"></a>
    <a href="https://docs.rs/mod-signal"><img alt="docs.rs" src="https://docs.rs/mod-signal/badge.svg"></a>
    <a href="https://github.com/jamesgober/mod-signal/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jamesgober/mod-signal/actions/workflows/ci.yml/badge.svg"></a>
</p>

<p align="center">
    One API for SIGTERM / SIGINT / SIGHUP and Windows equivalents, with priority-ordered graceful shutdown hooks.
</p>

---

## Status

Status: ACTIVE - planning + initial scaffold. Public API not stable. See `.dev/ROADMAP.md` for the path to 1.0.

This repository is published primarily to reserve the crate name and
to establish the project scaffolding. Implementation work proceeds on
the schedule documented in `.dev/ROADMAP.md`.

## What it does

Unified OS signal handling for Rust. SIGTERM / SIGINT / SIGHUP / SIGPIPE and Windows equivalents through one API. Graceful shutdown orchestration with priority ordering, timeout enforcement, and hook chaining. Replaces the patchwork of ctrlc + signal-hook with a clean runtime-agnostic substrate.

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](LICENSE)
for the full text.

Copyright (C) 2026 James Gober.
