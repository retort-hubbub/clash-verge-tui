# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-09-26

### Added

- The node chosen in a group is remembered on the profile and replayed after
  every apply. A reload rebuilds every group, so without this the choice lasts
  until the next configuration change — which, for a subscription that updates
  on a schedule, is not long.
- `cvt proxies select` and `unpin`, and the interface's equivalents, record and
  forget the choice. The replay waits for a group to answer again before
  choosing and then confirms the choice took: the core applies a reload in the
  background, and the first version of this replayed into that window, where
  the selection was silently discarded. It cost an afternoon of believing the
  feature worked; the end-to-end check is what caught it.
- The whole replay shares one deadline rather than taking one per group, so a
  profile with many remembered groups and a core that has lost them all cannot
  hold an apply for half a minute.

### Changed

- `ApplyReport` carries `selections_restored`, because a choice is something a
  user made and one that could not be replayed is worth being able to see.

### Log Rotation and Header Names

Three gaps from `docs/FEATURE-COVERAGE.md`, which was written by reading the
code rather than by intent and then turned out to be a work list.

### Added

- Log rotation and pruning, on `logs.max_size_bytes`, `logs.keep` and
  `logs.keep_days`, applied to both the core's log and this program's. It runs
  when the core is started, which is the only moment either file can be moved:
  a running core holds its log open, so renaming it underneath would leave a
  live process writing into a file nothing will read again.
- `PrfItem::home`, recorded from `profile-web-page-url` — http(s) only, because
  the interface shows it. A panel that has stopped sending the header does not
  erase the value: what was true once is worth more than a blank.
- A subscription's suggested name, read from `Content-Disposition` in both the
  RFC 5987 and the older quoted spelling, and adopted by `profiles add` when
  `--name` was not given.
- Repair of a `path&a=b` subscription URL — a query string whose question mark
  a panel forgot, which otherwise answers 404. Attempted only when the URL as
  written fails to validate.

### Changed

- `logs.keep` above 64 is refused. The three log options are on the settings
  screen, where a rule that profiles may not set a value has to be reachable
  from if it is to be usable at all.

### Control Plane Ownership & Audit Fixes

A third adversarial review checked the fixes from the second and found ten more
problems with them, including the other half of a path traversal the second had
found and two regressions those fixes had introduced. All ten are closed, and
its tests are kept (`crates/cvt-core/tests/regression_validation_and_overrides.rs`).

### Added

- `core.external_controller` and `core.secret`, and two rows on the settings
  screen for them. The control plane is the application's, so a subscription
  update cannot overwrite it and an imported bundle cannot redirect it.
- `W-RULE-KIND` and `E-RULE-MALFORMED` report a rule type this build does not
  know, and a line that is not a rule at all.
- `docs/FEATURE-COVERAGE.md`: what this project does about each feature of
  `clash-verge-rev`, including the eighteen it does not implement and why.
- `crates/cvt-core/tests/regression_config_and_profiles.rs` and `regression_validation_and_overrides.rs`: two independent
  adversarial reviews, kept in the build.

### Changed

- The control plane comes from the settings or from a base profile. An
  enhancement that introduces `external-controller`, `secret`,
  `external-controller-cors` and the rest is refused, with a warning naming the
  key and where to put it instead; one that changes what the base declared has
  it put back, also with a warning.
- A prepended catch-all rule takes the existing catch-all's place rather than
  stacking above it, which left two of them and every rule below dead.
- `E-UNREACHABLE-RULES` became `W-UNREACHABLE-RULES`. The core loads a document
  with two catch-alls, and `Service::start_core` refuses to start when any
  error is present — so this was the difference between "a warning" and "your
  core will not start".
- `E-MATCH-WITH-PAYLOAD` became `W-MATCH-WITH-PAYLOAD` in 0.2.0 for the same
  reason.

### Fixed

- `PrfItem::file` was an unvalidated path component, like the uid before it: an
  index carrying `file: ../outside.yaml` made the store write, read and
  *delete* outside `profiles/`, and an import copied a file from outside its
  source directory into one.
- `IP-CIDR6` was reported as a rule type this build does not know. The core
  loads it, and reports it through `GET /rules` as an `IPCIDR` rule — the same
  adapter as `IP-CIDR`, which is why it had no row of the translation table.
- The overlay contradiction check refused working overlays: a `set` reaching
  into a list (`a[0].b`) is how an element is addressed, and one writing an
  *ancestor* of a list was never compared against the value it writes.

## [0.1.1] - 2026-09-26

### Fixed

Everything below came out of an adversarial audit of this codebase by a
separate agent, whose report listed eighteen findings. The report is a working
document about the code rather than part of it, so it is not in this
repository; what *is* here is the outcome. Every counterexample it produced
was preserved as a disabled test at the time, and **none of them is disabled
any more**.

One of these contradicts something already released: 0.1.0 claimed that an
edit's push semantics were atomic, and that claim was false for one class of
input.

- `enhance::path::push` materialised the keys it needed and only then checked
  that the leaf was a list, so a call that reported failure had still changed
  the document. It now settles every checkable thing before the first write.
- `enhance::diff` reported "no changes" for a pure reorder of a named list, and
  derived the set of changed top-level keys from the (capped) list of entries —
  so a large diff under-reported which keys had changed at all.
- `Service::reload` reported rollback bookkeeping instead of the reason a
  restart failed. `validate` called the built-in `GLOBAL` and `PASS-RULE`
  policies dangling, rejecting configurations the core accepts and runs.
- `Overlay::append` left two terminal rules in a list, producing a document the
  validator rejects. `Overlay::default()` disagreed with an overlay parsed from
  an empty document. A `null` nested in a subtree that did not exist yet
  survived the merge instead of deleting.
- A named `remove` deleted one element per application, so two entries
  sharing a name made an overlay a one-shot. Every match goes now. `Overlay`
  also works on a copy, so a refused overlay leaves the document untouched
  rather than half-applied.
- Appending a list holding a rule *and* a catch-all lost one of them while the
  log said both had been added. `type: relay` was accepted although this core
  version removed it; so was `FINAL` as a rule kind, and a bare `MATCH` was
  filled in with a policy nobody wrote. A case-mismatched policy target
  (`MATCH,direct`) passed validation and was refused by the core.
- `SeqPatch::prepend` duplicated a value named twice in one patch, and
  `apply_values` compared the base by name while comparing the patch by
  identity. `Model::parse` discarded everything after the policy on a
  payload-less rule, which also made `E-MATCH-WITH-PAYLOAD` unreachable.
- `ProfileStore::add` accepted a uid already in the index, so two entries
  shared one document; `import_from` overwrote a document no index entry
  owned. `GroupKind` offered a `smart` group type mihomo does not have, and
  the validator suggested it. The wire types rejected `null` for most list
  fields and a numeric port.
- `mihomo::client::Client::probe` answered "is the upgrade endpoint present?"
  by *calling* it, starting a geodata download. A sequence-patch note printed
  its two sizes in the order `(new -> old)`.

### Changed

- An `Overlay` that gives a path a list while a `set` needs it to be a mapping
  is refused when the document is read, instead of applying once and failing
  the next time. A `set` that reaches *into* the list is allowed, because that
  is how an element of one is addressed.
- An append list naming two catch-all rules is refused: only one of them could
  ever run.
- `E-MATCH-WITH-PAYLOAD` became `W-MATCH-WITH-PAYLOAD`. A core accepts a field
  after a payload-less rule's policy and ignores it, so rejecting the line
  would refuse a configuration that loads; the warning says the field does
  nothing. A new `W-RULE-KIND` reports a rule type this build does not know,
  and `E-RULE-MALFORMED` reports a line that is not a rule at all.

### Security

- `ProfileStore::import_from` took the `uid` from another installation and
  used it as a file name, so `uid: "../profiles"` wrote the imported document
  over the profile index itself. A uid that is not one plain path component is
  now replaced, at the import and in `add`.

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

[0.2.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.2.0
[0.1.1]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.1.1
[0.1.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.1.0
