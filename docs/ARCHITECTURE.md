# Architecture

This is a map of the code: what lives where, how a request travels through it,
and which invariants hold at each boundary. The *reasons* behind the design are
in [`adr/`](adr/README.md); this document assumes them and describes the result.

## The shape of the thing

```
cvt (binary)  ->  cvt-tui  ->  cvt-core
```

Strictly one-way. `cvt-core` has no dependency on either front end, and
`cvt-tui` has none on the binary. A crate that needs something from above does
not get a new dependency edge; the thing moves down.

| Crate | Role | Depends on |
|---|---|---|
| `cvt-core` | All the logic: configuration, profiles, the core's API, the process supervisor | — |
| `cvt-tui` | The interaction as data, and the rendering of it | `cvt-core` |
| `cvt` | Argument parsing, output formatting, and the effect executor | `cvt-core`, `cvt-tui` |

## `cvt-core`

### The pure core, and the shell around it

The split that matters inside this crate is not model/view/controller. It is
**code that can only look at values** versus **code that touches the world**.
The first group is where the invariants live and where the property tests
point; the second group is kept as thin as it can be.

| Module | Touches the world? | What it does |
|---|---|---|
| `model` | no | The configuration document, proxies, groups, rules |
| `enhance` | no | Path edits, merging, diffing, overlays, the generation pipeline |
| `validate` | no | Whole-document checks, with a stable code per finding |
| `profile::item` | no | The profile types and the patches that compose them |
| `error` | no | One error enum, with `short()` for a status line |
| `settings` | no | User preferences, with validation |
| `paths` | yes | The home layout, atomic writes, detecting an existing clash-verge home |
| `profile::store` | yes | The profile index and the chain on disk |
| `profile::source` | yes | Subscription fetching, three tiers of fallback |
| `mihomo` | yes | The API client, the event streams, the process supervisor |
| `service` | yes | The facade that sequences all of the above |

`model`, `enhance` and `validate` read no environment variables, open no
files, and print nothing. That is what lets a property test generate a
thousand documents and check an invariant over all of them in under a second.

### The configuration document

`model::Config` is a newtype over an ordered JSON map, not a set of typed
fields. Its job is to be **lossless**: an unknown key, an unknown subtree, or a
key order nobody expected comes out the other side unchanged. Typed accessors
(`port()`, `rules()`, `proxy_groups()`) read the document; they do not define
it. Each rule round-trips byte-exactly through `model::rule`, including
nested logical forms.

The consequence is that mistakes a typed model would have rejected at parse
time are caught later, by `validate`, which reports *every* problem it finds
with a stable code (`E-DANGLING-POLICY`, `E-RELAY-CYCLE`, `W-TERMINAL-NOT-LAST`)
rather than failing on the first. See [ADR 0002](adr/0002-lossless-config-document.md).

### The generation pipeline

```
profiles on disk
      │
      ▼
 profile::store ── resolve the chain ──▶ an ordered list of documents
      │
      ▼
 enhance::merge ── deep merge, per-array strategy ──▶ one document
      │
      ▼
 enhance::overlay ── remove / set / prepend / append ──▶ one document
      │
      ▼
 validate::check ──▶ a report, not an exception
      │
      ▼
 enhance::pipeline::commit ── write runtime/config.yaml, snapshot the old one
      │
      ▼
 Service::reload ── hot reload, else restart, else roll back and restart
```

Three properties are worth noting because they are enforced rather than
intended.

`enhance::path` is atomic: an edit whose path cannot be applied leaves the
document exactly as it was. Getting that right took a read-only pass down the
path before the first write, because a descent that has already materialised
the keys it needs cannot report failure *and* leave nothing behind.

Re-running the pipeline against the same source documents produces the same
configuration. Applying an overlay repeatedly to an already modified document
is usually idempotent, but operations addressing list positions are not: both
`remove: ["proxies[1]"]` and a `set` at a list index combined with a list edit
can act on a different element on the next pass. These positional operations
remain available for one-shot edits; see `enhance::overlay` for the precise
contract.

Appending a rule places it *before* the terminal `MATCH`, because a catch-all
appended after a catch-all is dead code — and appending a rule that is itself
terminal *replaces* the existing catch-all, because two of them is a document
the validator rejects and the second can never fire.

`enhance::pipeline` keeps the last 20 generated documents. That is what makes
`ReloadOutcome::RolledBack` possible, and it is the reason the reload decision
tree can be as aggressive as it is.

### Talking to the core

`mihomo::endpoint` normalises the four ways to reach the API — TCP, TLS, a Unix
socket and a Windows named pipe — into one type.
`mihomo::client` is one typed method per endpoint, against a contract observed
from a real binary rather than the published documentation
([ADR 0004](adr/0004-hand-verified-api-contract.md)). `mihomo::stream` covers
both event transports: WebSocket, and the HTTP newline-delimited fallback,
behind one `recv_timeout` interface with reconnection and a bounded queue.

`mihomo::supervisor` owns the process: locating the binary, checking a
configuration with `mihomo -t` before trusting it, starting it, stopping it
with `SIGTERM` and then `SIGKILL`, and a pid file that records the process's
start time as well as its number — because a recycled pid must not be mistaken
for a running core.

### Subscription updates

`profile::source` fetches in three tiers: direct, then through the core's own
proxy, then through the system proxy. The order is the whole point — the
machine that most needs a subscription update is the one whose only route to
the internet *is* the subscription. Bodies are size-limited, base64 and
plain-text payloads are both accepted, and `subscription-userinfo` is parsed so
the interface can show what is left. `update_all_due` implements the
per-profile interval.

### `service`

The facade, and the only type the two front ends hold. It owns the paths, the
settings, the store and the supervisor, and it exposes *operations* rather than
effects: an `ApplyReport`, a `ReloadOutcome` (`HotReloaded`, `Restarted`,
`RolledBack`), and diagnostics a person can read.

The sequencing of an apply is the most dangerous code in the project and lives
in exactly one place:

1. generate and validate, and refuse to touch anything if the result is broken;
2. try a hot reload, which applies most changes without dropping connections;
3. only if that fails, restart the process;
4. if the core then fails to come up, restore the previous snapshot and start
   again;
5. **wait until the document is live** — `/version` answering means the process
   is up, not that it has *this* configuration, and a reload rebuilds the groups
   in the background;
6. replay the node choices a user made, which the reload has just discarded.

Steps 5 and 6 are the ones a reader is most likely to delete as redundant. Both
were added because a command that ran immediately after a successful `apply`
failed — the first with `no group named PROXY`, the second silently, with the
core still holding the old node.

A reload that cannot be undone reports the failure that caused it — not what
the rollback bookkeeping did — and a machine with no core binary is not treated
as a document worth rolling back, because the previous document would fail in
exactly the same way. See [ADR 0006](adr/0006-one-service-facade.md).

## `cvt-tui`

### Interaction as data

`App` is a state machine and performs no I/O. Interaction is a fold:

```
App::on_event(Event) -> Vec<Effect>      // what happened -> what should happen
App::on_tick()       -> Vec<Effect>      // time passing  -> what should happen
ui::render(frame, &app)                  // state -> a frame
```

`Effect` is the vocabulary of things that need the outside world (~36
variants); `Event` is the vocabulary of things that came back. Everything
I/O-shaped lives in the binary's executor, which turns an `Effect` into a
`Service` call and feeds the outcome back as `Data`, `Done` or `Failed`.
Errors arrive as events and land in the status line; they do not unwind the
loop.

This is what makes 67 state-machine tests and 23 render tests possible without
a terminal, and it is why an operation cannot be sequenced differently in the
two front ends — the interface has no way to sequence anything
([ADR 0005](adr/0005-pure-state-machine-with-effects.md)).

### The rest of the crate

| Module | What it holds |
|---|---|
| `action` | The 61 user-visible actions, with labels for the footer |
| `keys` | The key map, resolved per screen, with a `Context` for overlays |
| `theme` | Semantic colour roles, plus a monochrome theme for a pipe |
| `state` | `Table<T>` with sorting and filtering, `LogBuffer`, `Metrics` |
| `row` | Display rows: how a model value becomes a line on screen |
| `app` | The state machine, the effect vocabulary, the overlay stack |
| `ui` | One module per screen, each a pure function of `&App` into a `Frame` |

Overlay key precedence is structural rather than per-screen: an open prompt
consumes input, so typing `q` into a search box searches for `q`. Selection is
preserved across a refresh by identity — uid, name, id — rather than by index,
because a refresh reorders.

## `cvt`

Argument parsing, output formatting, the exit-code mapping, and the effect
executor. The module structure mirrors the CLI's subcommand groups.

Almost everything is a `Service` call. Two commands are not: `geo` and `unlock`
make their own HTTP requests through the core's proxy port, because the question
they answer — what address the world sees, and whether a service will serve it —
is about the *tunnel* rather than about the core, and the core has no endpoint
that answers it. Both go through `proxied_client` in `commands/mod.rs`, which is
where the proxy port is read and the timeout is checked, so the two cannot drift
apart on either.

Two conventions are load-bearing. `--json` emits a stable schema name on every
read command, so a script can depend on the shape rather than parse the human
output. And exit codes are meaningful rather than binary — 3 for a validation
failure, 4 for an unreachable controller, 5 for a missing core — so a script
can tell "your configuration is wrong" from "the core is not running". The
full table is in `docs/CLI.md` and in `--help`.

## Testing strategy

Five layers, each catching what the one below cannot.

| Layer | Where | What it is for |
|---|---|---|
| Property tests | `cvt-core/src/**.rs`, `tests/invariants.rs` | Invariants over generated input: losslessness, idempotence, atomicity |
| API contract tests | `tests/client_contract.rs` | The client against a fake controller that answers the observed bytes |
| Render tests | `cvt-tui/src/ui/render_tests.rs` | Every screen, empty and populated, at four terminal sizes |
| Live core tests | `tests/live_controller.rs` | The same expectations against a real binary |
| Review counterexamples | `cvt-core/tests/regression_*.rs`, `cvt-tui/tests/regression_*.rs` | Reproductions from independent reviews, grouped by their main subject |

The property layer is the interesting one, because the risk in this program is
not a wrong value but a wrong *property*: a merge that is not idempotent, a
round trip that loses a field, an edit that reports failure and mutates anyway.
Those are stated as properties and searched over generated input.

The last layer exists because the fake controller is written by the same people
who wrote the client, so it can only confirm what they already believe.
`tests/live_controller.rs` is environment-gated and is a no-op without a core —
which is why it went unnoticed for a while that it had never actually run. When
it did, it failed on its fourth assertion.

### Adversarial review, and where its counterexamples live

Independent reviews left counterexamples as ordinary passing integration tests.
The files originally followed review order; they now name their main subject.
Each still records its review context at the top, including cases where the
reviewer's original assertion was wrong. Some suites span more than one area
because the original review checked cross-cutting behavior.

| Crate | Regression suite | Main subjects |
|---|---|---|
| `cvt-core` | `regression_config_and_profiles.rs` | Configuration invariants, profile index, validation and rollback |
| `cvt-core` | `regression_validation_and_overrides.rs` | Validator behavior, overlays and corrected review claims |
| `cvt-core` | `regression_logs_and_subscriptions.rs` | Log rotation, subscription metadata and control plane |
| `cvt-core` | `regression_selection_and_paths.rs` | Selection replay, profile paths, reload and log safety |
| `cvt-core` | `regression_backups_and_test_targets.rs` | Backup and restore, named test URLs and replay |
| `cvt-core` | `regression_cli_limits_and_backups.rs` | CLI limits, backup state and controller waits |
| `cvt-core` | `regression_restore_and_network_probes.rs` | Restore safety, timeouts, network probes and diagnostics |
| `cvt-core` | `regression_overlay_and_control_plane.rs` | Overlay rules, protected keys, backup safety and diagnostic scan |
| `cvt-tui` | `regression_settings_and_cli_contract.rs` | Settings editor, CLI flags, controller deadlines and backup races |
| `cvt-tui` | `regression_terminal_and_rendering.rs` | CLI name checks, rendering, terminal lifecycle and global flags |

The `regression_*.rs` files remain separate Cargo integration test targets so
their test fixtures and process state do not share one executable.

Three things a reader should know before trusting one of those files:

- **A reviewer's assertion can be wrong.** Past reviews produced an assertion
  that encoded the *buggy* state, a failure message that proved the
  fix worked while the assertion failed, two tests in one round that
  contradicted each other, and a premise that was timing-dependent and flaked.
  Each finding is reproduced by the author before it is acted on, and a
  disagreement is written down beside the assertion rather than silently
  absorbed.
- **A fix that is narrower than its defect is the recurring failure.** Several
  times, a guard has covered the members of a class that somebody had named
  rather than the class itself: `uid` then `file` then the arm beside it; the
  control plane; `--url` in two commands and then a third; `--concurrency` in
  one function and then a second copy of the flag; `--timeout` never checked at
  all; a canonicalised suffix with an uncanonicalised stamp. The fix that works
  is the one that makes the class impossible to join without the guard.
- **Some defects only running finds.** A fake controller cannot tell an allowed
  path from a refused one, so four rounds of API-client review missed that the
  hot reload could never work. A fifo where a document goes hangs
  `std::fs::copy`'s open forever. Both were found by running the program.

The *reports* are working documents about the code rather than part of it, and
they list defects that were open when they were written, so they are
deliberately not in this repository. What belongs here is the tests.
