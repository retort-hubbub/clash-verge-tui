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

- `cvt-core` is the library. The parts that reason about configuration —
  `model`, `enhance`, `validate`, `profile` — perform no I/O at all: no file
  system, no environment variables, no network, no printing. They take values
  and return values. Where the real world is involved it is confined to
  `mihomo` and `paths`, and the `Service` facade is what sequences it.
- `cvt-tui` depends on `cvt-core` and adds only presentation and interaction.
  Its `App` performs no I/O (see [0005](0005-pure-state-machine-with-effects.md)).
- `cvt` depends on both and holds only argument parsing, output formatting and
  the one place where an effect becomes a real call.

A crate that needs something from a layer above it does not get a dependency
edge; the thing moves down.

## Consequences

- The tests that matter most run in milliseconds and need nothing installed. A
  merge test cannot accidentally read `$HOME`, because the crate has no way to.
- The layering is visible in `cargo tree` and in any diff, so a violation is
  caught in review rather than discovered later.
- A change to the library's public API touches all three crates, and an
  internal refactor sometimes needs a matching visibility change. This is the
  cost, and it is paid deliberately.
- Cross-crate integration is not typechecked. `cvt` and `cvt-tui` are only
  proven to agree by tests at their boundary, which is why the effect contract
  has its own test with a fake executor.

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
