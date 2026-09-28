# 0001 — Rust, as a three-crate workspace with a one-way dependency

**Status:** accepted

## Context

The program has three kinds of code in it, and they have very different
testing stories.

There is logic that is pure and worth getting exactly right: how a
configuration document is represented, merged, diffed and validated. There is
code whose correctness depends on a foreign system — the core's HTTP and
WebSocket API, its process lifecycle, its on-disk formats. And there is a
terminal interface, where almost everything is presentation and the failure
modes are "the layout is wrong" rather than "your network is broken".

If those three live in one crate, every test drags in the others. A test for
whether a merge is idempotent has to link a TUI toolkit and an HTTP client,
and nothing stops a helper written for the interface from quietly doing file
I/O inside the code that decides what a user's configuration means.

## Decision

Three crates in one workspace, with the dependency direction enforced by the
manifest:

```
cvt (binary)  ->  cvt-tui  ->  cvt-core
```

- `cvt-core` owns configuration and shared application operations. Models,
  validators and document transformations operate on values. Filesystem and
  network adapters include `paths`, `settings`, `profile::{store,source}`,
  `enhance::pipeline` and `mihomo`; `Service` coordinates them.
- `cvt-tui` owns interaction state and rendering. State transitions return
  effects. `App::with_settings` accepts initialized settings without I/O;
  `App::new` is a convenience loader. See [0005](0005-pure-state-machine-with-effects.md).
- `cvt` owns CLI parsing, output and the adapters that execute TUI effects.

A crate that needs something from a layer above it does not get a dependency
edge; the thing moves down.

## Consequences

- Pure transformations can be tested without a terminal or core. The crate
  also contains I/O adapters, so explicit paths and fixtures remain necessary.
- The layering is visible in `cargo tree` and in any diff, so a violation is
  caught in review rather than discovered later.
- A change to the library's public API touches all three crates, and an
  internal refactor sometimes needs a matching visibility change. This is the
  cost, and it is paid deliberately.
- Rust typechecks the shared protocol across crates. Event ordering, I/O
  behavior and rendering still require verification at those boundaries.

## Alternatives considered

**A single crate with modules.** Simpler to start and no visibility changes
needed for refactors. Rejected because the module boundary would be advisory:
nothing would stop the interface from calling the file system, and the
property tests would link the whole world.

**Two crates: `cvt-core` plus a combined binary.** Would have kept the library
pure while making the interface untestable without a terminal, which is the
specific problem the effect vocabulary exists to avoid.

**A separate crate per concern — `cvt-config`, `cvt-api`, `cvt-profile`.**
More precise, and the reason it was rejected is practical: several of these
would be small enough that the manifest overhead and the visibility churn
outweigh the boundary, and the pure/impure split — which is the split that
actually predicts testability — is preserved by `cvt-core`'s module layout
anyway.
