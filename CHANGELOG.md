# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing yet.

## [0.1.0] - 2026-09-25

### Added

- Workspace scaffold: `cvt-core`, `cvt-tui`, `cvt` binary.
- `cvt-core::paths` — XDG-compliant on-disk layout with `CVT_HOME`/`--home`
  overrides, atomic file writes, and detection of existing `clash-verge-rev`
  installations for import.
- `cvt-core::model` — lossless configuration document with typed accessors,
  plus proxy, group and rule models that preserve unknown fields.
- `cvt-core::model::rule` — rule parser that understands nested logical rules
  and payload-less `MATCH` rules.
- `cvt-core::validate` — whole-document pre-flight validation with stable
  diagnostic codes, including checks for dangling policies, dead rules behind a
  terminal rule, relay cycles and the fake-IP ULA trap.
- `cvt-core::enhance::path` — declarative path expressions for config edits,
  written so that an edit is all-or-nothing.
- `cvt-core::enhance::merge` — deep merge with per-array strategies
  (`replace`, `append`, `prepend`, `union`) and the directive vocabulary that
  `clash-verge-rev` override documents use.
- `cvt-core::enhance::diff` — a bounded structural diff that reports added,
  removed, changed and reordered entries.
- `cvt-core::enhance::overlay` — declarative overlays that insert an appended
  rule before the terminal `MATCH` rather than after it.
- `cvt-core::enhance::pipeline` — generate, commit, snapshot and roll back the
  runtime configuration, keeping the last 20 snapshots.
- `cvt-core::profile::source` — subscription fetching with a three-tier
  fallback (direct, then through the core, then through the system proxy),
  subscription-userinfo parsing, and size and encoding limits.
- `cvt-core::profile::store` — the profile index, the profile chain, and
  import from an existing `clash-verge-rev` home.
- `cvt-core::mihomo` — a client for the core's REST and WebSocket API,
  including the supervisor that starts, validates and stops the process.
- `cvt-core::service` — the facade that turns all of the above into the
  operations a user asks for, including the reload decision tree.
- `cvt-tui` — the terminal interface: theme, key map, table and log state, and
  a screen per tab, all rendered as pure functions of application state.
- `cvt-tui::app` — the interaction as data: 61 actions, an overlay stack and a
  status line, driven entirely by an effect vocabulary so that the state
  machine itself performs no I/O and can be tested without a terminal.
- `cvt` — a command line over the same facade the interface uses, with JSON
  output on every read command and documented exit codes (`docs/CLI.md`).
- `cvt-tui::run` — the session: raw mode, the alternate screen, a panic hook
  that gives both back, and a loop that draws on any backend and reads any
  event stream, so a session can be tested without a terminal.
- The interactive interface itself, launched by running `clash-verge-tui` with
  no subcommand. Every action the interface offers is performed through the
  same `Service` the command line uses.
- `docs/ARCHITECTURE.md` and seven decision records, including the one
  `model/config.rs` has been citing since the document type was written.
- `deny.toml` — the dependency policy, whose allowed-license set was
  enumerated from the lockfile rather than guessed.
- `docs/VERIFICATION-REPORT.md` — an independent adversarial review, including
  the findings it could not substantiate and the areas it did not cover.
- `CONTRIBUTING.md` documenting the GitFlow workflow and architecture rules.

### Fixed

- `mihomo::client::Client::probe` answered "is the upgrade endpoint present?"
  by *calling* it, which starts a geodata download; it now asks with a method
  the route is not registered for and runs no handler.
- `Service::reload` reported "there are no snapshots to restore" instead of the
  reason a restart failed, hiding a missing core binary and a document the core
  had rejected behind rollback bookkeeping.
- `validate` called the built-in `GLOBAL` and `PASS-RULE` policies dangling, so
  a rule targeting either — which the core accepts and runs — was rejected.
- `Overlay::default()` did not match an overlay parsed from an empty document,
  and the derived default appended rules after a terminal `MATCH`, where they
  can never match.
- A sequence patch reported its lengths in the order `(new -> old)`, so a patch
  that added a rule read as if it had removed one.
- `cargo deny` was configured to run in CI with no configuration, which could
  not have passed, and the declared MSRV was three versions below what the
  dependency graph requires.

[Unreleased]: https://github.com/retort-hubbub/clash-verge-tui/compare/v0.1.0...develop
[0.1.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.1.0
