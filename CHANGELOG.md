# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `--timeout` and `--concurrency` are read in one place and held to the
  ceilings the settings are. `test.concurrency: 600` was refused with "would
  exhaust file descriptors" while `--concurrency 600` reached
  `buffer_unordered`; then `test urls` kept its own copy of the two flags and
  did it again; and `--timeout 32768` was accepted by every command while the
  settings refused it with "the core parses this as an int16". The two flags are
  one struct now, flattened into every command that takes them, and resolved by
  one function — because a ceiling that reaches three commands out of four is
  the pattern this project has now fixed five times.
- `copy_file` compares *files*, not paths. Two names for one inode — a hard
  link, which is what `cp -al` leaves behind — passed the self-copy guard,
  because the paths really are different, and `std::fs::copy` then truncated the
  file before reading it. A restore emptied the document it was asked to put
  back.
- The name a backup falls back to when a second has a thousand of them keeps
  the sequence it parses back to. `-overflow` read as sequence 1 — the bare
  timestamp's — so the two collided and their order went back to `read_dir`'s,
  which is the thing the sequence was added to stop.
- `backup create` reports the timestamp and the entry count of the backup it
  took, rather than the epoch and zero.

### Added

- `cvt unlock` — whether the exit can reach the services people actually ask
  about. A latency number says a socket opened; it does not say whether the
  other end will serve you, and a node can be fast, in the right country, and
  on a range the streaming services have already blocked. Four services that
  genuinely answer differently by region are asked through the core's proxy,
  and **the evidence is printed beside every verdict**: a tool that says
  `Netflix: unlocked` and cannot show why is one whose answer cannot be
  checked, and these probes are wrong often enough — services change their
  pages, a probe URL that worked last month may answer a login wall today —
  that seeing the response is the difference between a reading and a guess. An
  answer that says nothing either way is reported as `unknown` rather than
  rounded to one of the other two.
- `cvt geo` — the address and location the traffic comes out at, asked *through
  the core's own proxy port* so the answer describes the tunnel rather than the
  machine. `--direct` asks the same question without the proxy, and the pair is
  the point: a node can be fast and in the wrong country. Three sources are
  tried in order, because they are other people's services and one being down
  is not a reason for this command to have nothing to say.

### Fixed

- `wait_for_document` bounds every call and polls its groups together. The
  deadline was checked *between* `client.group()` calls, and that call carries
  the client's own timeout — at least five seconds from the settings — so a core
  that answered `/version` and hung `/group` held an `apply` for about ten
  seconds against a five-second budget. And waiting on each group in turn gave
  the first one that never appeared the whole budget, so the groups after it
  were never asked about. It is the same pair of mistakes the selection replay
  was fixed for three times, and the sixth review found this fourth instance by
  reading rather than running.

## [0.8.0] - 2026-09-26

### Fixed

- `apply` waits for the configuration it wrote to be *live* before reporting
  success. `/version` answering means the process is up, not that it has this
  document: a reload rebuilds the groups in the background, and the very next
  command failed with `no group named PROXY` after an apply that exited 0.
- `--concurrency` is held to the same ceiling as `test.concurrency` in the
  settings. `--concurrency 600` went straight to `buffer_unordered` while the
  same number in `cvt.yaml` was refused with "would exhaust file descriptors" —
  one limit, two answers, and the one that got through was the one nothing
  validated. The check moved into `node_options`, the one function every
  latency command reads its flags through, because the guard had already been
  put in the two commands somebody remembered and the review found the third
  and then the fourth.
- A directory reached through a symbolic link is not copied into a backup, and
  a restore refuses to write through one. Reading one copies files from
  wherever the link points — a test put a `secret.yaml` in somebody else's
  directory and it arrived in the backup — and writing one puts files there.
  The two directions answer differently on purpose: a source that is a link is
  skipped, because a user with `profiles/` on another disk should still be able
  to take a backup, and a destination that is a link is refused, because a
  restore that silently writes outside the home is not recoverable.
- `cvt test urls` reports its rows in the order the URLs are configured in.
  `buffer_unordered` yields as each measurement finishes, so the rows came back
  in completion order — a report whose rows move between runs is one nobody can
  diff.
- `cvt test urls --node typo` emits the report with every row carrying the
  reason, and exits non-zero. It used to print `reachable: 0` and exit 0, which
  is the honest answer for a node that exists and reaches nothing — the two are
  worth telling apart, and only one is worth retrying. The report is still
  printed so a `--json` consumer parses one shape whatever happens.
- `cvt proxies test --url typo` refuses a value that is neither a URL nor a
  configured target, as `cvt test delay` already did.
- `backup()` could prune the backup it had just taken, and return a path to a
  directory that no longer existed — which is what `restore` builds its safety
  copy with, so restoring the wrong backup was not undoable. Pruning is now
  ordered by `(second, sequence)` and never through the new directory.
- The selection replay polls the groups that have not come back *together*
  rather than waiting on each in turn. Six groups a subscription has removed
  cost six waits, which spends the whole budget, and the seventh choice — the
  one that would have worked — is never reached.
- `--url https://example.com/` is a URL even when a test target is *named*
  `https://example.com/`. A name that shadows the thing it looks like is the one
  way a name could fetch somewhere the user did not ask for.
- A partial `test:` block in `cvt.yaml` is loadable again. Adding `urls` put
  the new entry struct in front of `TestSettings` and took the section's
  `#[serde(default, deny_unknown_fields)]` with it, so
  `test: {timeout_ms: 3000}` was refused with "missing field `url`". Found by
  the sixth review before it shipped.
- `prune_backups` kept a different set depending on the order backups were
  made in. Two backups in the same second share a timestamp, and sorting on
  the timestamp alone left their order to `read_dir`.
- A symlink in `backups/` is no longer listed as a backup — `is_dir` follows
  the link — and is removed rather than followed when the directory is pruned.
- A restore no longer writes *through* a symlink. `std::fs::copy` opens the
  destination for writing, which follows it, so a home whose
  `profiles/L1.yaml` was a link into a dotfiles directory had that file
  overwritten by a restore.

### Added

- `docs/DIAGNOSTICS.md`, and two tests that keep it honest: every code the
  validator constructs must appear in the reachability table and in the
  documentation, and neither may list a code that no longer exists.

### Changed

- `every_code_the_validator_produces_is_in_the_table` is new, and it found two
  codes that were produced and had never been checked for reachability
  (`E-RULE-MALFORMED`, `W-RULE-KIND`).
- One assertion in `invariants.rs` was `!codes.contains(&"E-MATCH-WITH-PAYLOAD")`
  — a code constructed nowhere in the library, so it was true of every input
  and checked nothing. Replaced by a severity assertion, which then failed and
  showed that the document it tested named a policy that does not exist.

## [0.7.0] - 2026-09-26

### Added

- `test.urls` names the URLs a node can be measured against, and `cvt test urls
  --node N` measures every one through a node. A delay says a socket opened to
  one host — the same host for every node — so a node that cannot reach
  anything useful still reports a healthy number. Naming the three places
  people actually ask about (`google`, `github`, `youtube`) answers the
  question they are asking instead.
- `--url` accepts one of those names wherever it accepts a URL. A value that is
  neither is refused with the list in the message, because fetching a typo
  fails and looks like a node problem rather than a mistake.
- `cvt test urls --list`, and `--json` shapes `cvt.test.urls.v1` and
  `cvt.test.targets.v1`.

### Note

The default `youtube` target is `https://www.youtube.com/robots.txt` rather
than the front page: a robots file is a few hundred bytes, it is served over
the same TLS connection, and fetching a video page to measure latency would be
a strange thing for a monitoring tool to do.

## [0.6.0] - 2026-09-26

A fifth adversarial review, of the newest work. It found twelve problems: eight
in what it was sent to check, and four more that opened while it ran, because
the author was fixing the first eight in the working tree beside it.

Two of them are worth the space.

**The hot-reload path could never work.** The generated configuration lives in
`<home>/runtime/`, the core is started with `-d <home>/core/work`, and mihomo
refuses any `path` that is not under its own home — `400 path is not subpath of
home directory or SAFE_PATHS`. So `--mode hot` always failed, and `--mode auto`,
the default, always fell back to a **restart**: the one thing the reload design
exists to avoid, because a restart drops every live connection. It survived four
reviews of the API client because the contract test serves a fake controller,
and a fake controller cannot tell an allowed path from a refused one. The fix is
`SAFE_PATHS` on the child, verified against the real core.

**A restore could empty the home it was restoring.** `std::fs::copy(x, x)`
truncates: the destination is opened for writing before a byte is read, and the
call reports success having written nothing. `restore(<home>)` therefore left
the index, the settings and every document 0 bytes long, and exited 0. It is
refused now, and the per-file copy refuses to copy a file onto itself whatever
calls it.

### Added

- `cvt backup create|list|restore`, over the profiles, the profile index, the
  settings and the overrides. A restore is additive and keeps the state it
  replaces.

### Fixed

- The hot reload now tells the core which directory the document is in, so the
  API path works and an apply no longer restarts the core.
- `Service::restore` refuses a source that is the destination.
- The replay waits for a group with a budget of its own *and* a total: one
  group that no longer exists no longer spends the whole budget and starves
  every choice after it, and a reload window longer than one group's budget no
  longer loses the choice.
- `client.select` is inside the budget like every read; a core that answers
  reads and never answers writes used to cost five seconds against a two-second
  budget.
- A pinned `url-test` or `fallback` group is confirmed by its `fixed` field. A
  pin that had taken was reported as a failure, because `now` is not where a
  pinned group reports it.
- An empty group name in an imported index no longer sends the replay after
  `/group/`.
- A `selected` entry from the reference project no longer makes the whole index
  unreadable: both fields are optional there, and `now: null` is what it writes
  for a group nobody has chosen in.
- The sanitised fallback name is injective — `a/b` and `a.b` no longer share one
  document.
- `E-CIDR-FAMILY` is **removed**. It was introduced by 0.4.1's own fix and
  rejects rules the core loads and activates: `IP-CIDR,2001:db8::/32,DIRECT`
  appears in `GET /rules` on a running core.
- `E-GROUP-EMPTY`: a group with neither `proxies` nor `use` is refused by the
  core and was accepted here.
- `cvt proxies select` reports whether the choice was recorded, in `--json`
  (`recorded`) and on stderr, instead of only under `-v`.
- A rolled-back apply no longer replays the choices of the configuration that
  failed to apply.

## [0.5.0] - 2026-09-26

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

## [0.4.1] - 2026-09-26

A fourth adversarial review checked 0.4.0's own code and found eleven
problems. All of them are closed, and its 30 tests are kept
(`crates/cvt-core/tests/recheck3.rs`).

The one worth naming is the fourth instance of the same pattern: the guard on
a document's file name covered the field the previous review had named and not
the arm of the same expression next to it, so an index entry that *omits*
`file` carried a hostile `uid` into a path — a subscription refresh could write
over a file outside `profiles/`, and `remove` could delete one. Both arms go
through the same guard now, and the fallback no longer keeps the uid verbatim.

### Fixed

- `PrfItem::file_name`'s uid-derived fallback was unvalidated (above).
- `start_core` rotated the live log before the "already running" check, so a
  *refused* start moved the log of the core that was still running, which then
  wrote into `.1` with nothing left to create `core.log`.
- The `path&a=b` URL repair was dead code for its own shape: that shape is a
  valid URL, so validating first never reached the repair. The shape is
  detected now, narrowly — no query yet, and an `=` after the first `&`.
- `prune_logs` deleted `core.log.0`, `.01` and `.+1`, none of which this module
  writes, and returned at the first file it could not remove, leaving the pass
  half done in an order-dependent way.
- `logs.keep` was capped only on the way to disk, so `cvt.yaml` could hold a
  hundred million and make every start probe that many paths.
- A symlinked log was rotated by renaming the *link*, so the bytes it pointed
  at stayed put and the log destination silently stopped receiving output. It
  is copied and truncated now.
- Two catch-alls in one `prepend` are refused, as the same patch written as an
  `append` already was.
- `IP-SUFFIX` and `SRC-IP-SUFFIX` were not checked, though the core refuses a
  bare payload for all five CIDR-shaped kinds; and `W-CIDR-NO-PREFIX` said
  "mihomo assumes a full-length mask", which the core contradicts — the rule
  does not load. It is `E-CIDR-NO-PREFIX` and an error.
- `name_from_url` decoded *after* choosing a segment, so
  `%2e%2e%2f…%2fpasswd` came through as `../../etc/passwd`; it also offered the
  host as a name for a URL with no path.
- A quoted `filename` was cut at a `;` inside the quotes — and the unquoted
  RFC 5987 spelling, which is the one that form actually uses, stopped being
  read at all.

## [0.4.0] - 2026-09-26

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

## [0.3.0] - 2026-09-26

A third adversarial review checked the fixes from the second and found ten more
problems with them, including the other half of a path traversal the second had
found and two regressions those fixes had introduced. All ten are closed, and
its tests are kept (`crates/cvt-core/tests/recheck2.rs`).

### Added

- `core.external_controller` and `core.secret`, and two rows on the settings
  screen for them. The control plane is the application's, so a subscription
  update cannot overwrite it and an imported bundle cannot redirect it.
- `W-RULE-KIND` and `E-RULE-MALFORMED` report a rule type this build does not
  know, and a line that is not a rule at all.
- `docs/FEATURE-COVERAGE.md`: what this project does about each feature of
  `clash-verge-rev`, including the eighteen it does not implement and why.
- `crates/cvt-core/tests/recheck.rs` and `recheck2.rs`: two independent
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

## [0.2.0] - 2026-09-26

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

[Unreleased]: https://github.com/retort-hubbub/clash-verge-tui/compare/v0.8.0...develop
[0.8.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.8.0
[0.7.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.7.0
[0.6.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.6.0
[0.5.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.5.0
[0.4.1]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.4.1
[0.4.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.4.0
[0.3.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.3.0
[0.2.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.2.0
[0.1.0]: https://github.com/retort-hubbub/clash-verge-tui/releases/tag/v0.1.0
