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

Two properties are worth noting because they are enforced rather than
intended. `enhance::path::set` is atomic — a path that cannot be applied
leaves the document untouched — and merges are idempotent, so applying an
override twice is the same as applying it once. Appending a rule inserts it
*before* the terminal `MATCH`, because a catch-all appended after a catch-all
is dead code.

`enhance::pipeline` keeps the last 20 generated documents. That is what makes
`ReloadOutcome::RolledBack` possible, and it is the reason the reload decision
tree can be as aggressive as it is.

### Talking to the core

`mihomo::endpoint` normalises the three ways to reach the API — TCP with an
optional secret, a unix socket, a Windows named pipe — into one type.
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
   again.

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
executor. Nothing else: every operation is a `Service` call, and the module
structure mirrors the CLI's subcommand groups.

Two conventions are load-bearing. `--json` emits a stable schema name on every
read command, so a script can depend on the shape rather than parse the human
output. And exit codes are meaningful rather than binary — 3 for a validation
failure, 4 for an unreachable controller, 5 for a missing core — so a script
can tell "your configuration is wrong" from "the core is not running". The
full table is in `docs/CLI.md` and in `--help`.

## Testing strategy

Four layers, each catching what the one below cannot.

| Layer | Where | What it is for |
|---|---|---|
| Property tests | `cvt-core/src/**.rs`, `tests/invariants.rs` | Invariants over generated input: losslessness, idempotence, atomicity |
| API contract tests | `tests/client_contract.rs` | The client against a fake controller that answers the observed bytes |
| Render tests | `cvt-tui/src/ui/render_tests.rs` | Every screen, empty and populated, at four terminal sizes |
| Live core tests | `tests/live_controller.rs` | The same expectations against a real binary |

The property layer is the interesting one, because the risk in this program is
not a wrong value but a wrong *property*: a merge that is not idempotent, a
round trip that loses a field, an edit that reports failure and mutates anyway.
Those are stated as properties and searched over generated input.

The last layer exists because the fake controller is written by the same people
who wrote the client, so it can only confirm what they already believe.
`tests/live_controller.rs` is environment-gated and is a no-op without a core —
which is why it went unnoticed for a while that it had never actually run. When
it did, it failed on its fourth assertion.

### Known defects are kept, not deleted

The counterexamples the property suite has found are preserved as `#[ignore]`d
tests with the reasoning in the body, rather than being deleted along with the
bug or left failing. Each one is a reproduction that outlives the report that
found it, and fixing one shows up in the history as a commit that removes an
`#[ignore]` line and its note. `cargo test -p cvt-core --test invariants --
--ignored` runs them, and they fail on purpose.

The review that produced them was an adversarial audit by a separate agent
whose job was to attack this code rather than confirm it. Its report is a
working document about the code rather than part of it, and it lists defects
that are still open, so it is deliberately not in this repository — it lives
beside the checkout in the maintainer's working copy. What matters to a
reader here is the part that *is* in the repository: every counterexample it
found is a disabled test above, with the analysis in the test body.
