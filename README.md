# clash-verge-tui

A terminal client for the [mihomo](https://github.com/MetaCubeX/mihomo) core
(Clash.Meta). It manages subscription profiles, generates and overrides the
runtime configuration, and talks to a running core to select nodes, watch
connections and logs, toggle rules and measure latency — from the keyboard,
without leaving the terminal.

```console
$ clash-verge-tui              # the interactive interface
$ clash-verge-tui status       # one line about the current state
$ clash-verge-tui doctor       # what is installed, what is missing, what the core supports
```

## What it does

**Profiles.** Local and remote profiles with a merge chain, so a base
subscription can be layered under your own overrides without editing anything
by hand. Profile documents are kept exactly as the subscription delivered them
and only interpreted at generation time, so an unknown field or a key order you
did not expect survives a round trip instead of being silently rewritten.

**Configuration.** A declarative override layer — set a path, remove a path,
prepend or append to a list, merge with a per-array strategy, or append a rule
that is inserted *before* the terminal `MATCH` rather than after it. Every
generated document is validated before it is written, and the previous one is
snapshotted, so a configuration that the core refuses can always be undone.

**Subscriptions.** Three-tier fetching: direct, then through the core, then
through the system proxy — so an update works on a machine whose only route to
the internet is the proxy it is updating. `subscription-userinfo` is parsed so
the interface can show what is left.

**The running core.** Select nodes, pin a selection, run latency and
DNS tests, list and close connections, watch traffic and memory, follow the
log stream, toggle rules, update rule providers, switch mode, and start, stop,
restart, upgrade or garbage-collect the core itself.

**Two front ends, one implementation.** The interactive interface and the
command line are both thin shells over the same `Service` facade, so they
cannot disagree about what a profile chain means or how an apply is sequenced.

## Requirements

- Rust **1.88** or newer. This is not a guess: it is the floor `ratatui 0.30`
  declares, and CI builds the workspace against exactly that version.
- The [mihomo](https://github.com/MetaCubeX/mihomo) core, somewhere on `PATH`,
  at `<home>/core/mihomo`, or named by `CVT_CORE` or by the `core.binary`
  setting. Only mihomo is supported — the Clash and Clash Premium cores are
  not, and the API client is written against mihomo's actual responses rather
  than against the documented ones.
- A terminal. The interface needs one; every command works without.

## Install

```console
$ git clone https://github.com/retort-hubbub/clash-verge-tui
$ cd clash-verge-tui
$ cargo build --release
$ ./target/release/clash-verge-tui --help
```

## Where things live

The home directory defaults to the platform configuration directory and can be
overridden with `--home` or `CVT_HOME`. Everything the program owns is in one
tree, so removing it removes every trace:

```
<home>/
├── cvt.yaml               your preferences
├── profiles.yaml          the profile index: what is current, and the chain
├── profiles/              one document per profile, byte-for-byte as fetched
├── overrides/             your override and merge documents
├── runtime/
│   ├── config.yaml        the generated configuration; the core is started with -f this
│   ├── config.previous.yaml
│   └── snapshots/         the last 20 generated documents
├── core/
│   ├── mihomo             where a core binary is looked for
│   └── work/              the core's own working directory, passed as -d
└── logs/                  core.log and app.log
```

An existing `clash-verge-rev` installation can be imported from the interface
or with `clash-verge-tui profiles import`, and `doctor` will point at the
homes it found.

## Documentation

- [`docs/CLI.md`](docs/CLI.md) — every subcommand, every flag, the `--json`
  schema names, and the exit codes.
- [`docs/OVERRIDE-FORMAT.md`](docs/OVERRIDE-FORMAT.md) — how to write an
  override, a merge or a sequence patch, and what each of them cannot do.
- [`docs/DIAGNOSTICS.md`](docs/DIAGNOSTICS.md) — every code the validator
  produces, what it means, and what to do about it.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — how the three crates fit
  together and why.
- [`docs/adr/`](docs/adr/) — the decisions behind the design, with the
  alternatives that were rejected.
- [`docs/FEATURE-COVERAGE.md`](docs/FEATURE-COVERAGE.md) — what this project
  does about each feature of `clash-verge-rev`, including the ones it does not
  implement and why.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — the GitFlow workflow, commit
  conventions, and the architecture rules that are not negotiable.

## Tests

```console
$ cargo test --workspace
```

The suite is layered on purpose. The library's model, merge, path and profile
code is covered by property tests over generated documents, because the
interesting failures there are invariants — a round trip that is not lossless,
a merge that is not idempotent, an edit that reports failure but mutates
anyway. The core API client is covered against a hand-rolled fake controller
that answers with byte sequences captured from a real mihomo, because the
documented API and the real one differ in ways that matter. On top of that is
an environment-gated check that replays the same expectations against a real
core:

```console
$ CVT_LIVE_CONTROLLER='127.0.0.1:9090|your-secret' \
  CVT_LIVE_LOG=/path/to/core.log \
  cargo test -p cvt-core --test live_controller -- --test-threads=1 --nocapture
```

The counterexamples every adversarial review has found are kept as **passing
tests** in `tests/recheck*.rs` rather than deleted, so each defect that was
once real has a reproduction attached and regressing it is a visible act.

## Relationship to other projects

[clash-verge-rev](https://github.com/clash-verge-rev/clash-verge-rev) is a
Tauri desktop application for the same core, and this project borrows its
profile model and file layout on purpose — being able to point at an existing
`clash-verge-rev` home and import it is worth more than a novel layout.
[clashtui](https://github.com/JohanChane/clashtui) is an earlier TUI for the
same problem; its existence is why this one is written as a library with a
thin TUI over it rather than as a TUI with logic inside it.

This is an **independent implementation**, not a fork of either, and it shares
no code with them.

## Licence

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
