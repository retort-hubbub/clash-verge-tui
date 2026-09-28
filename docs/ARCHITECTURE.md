# Architecture

This document maps current responsibilities and contracts. Design rationale
lives in [the ADRs](adr/README.md); contribution and verification commands live
in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Crate boundaries

| Crate | Responsibility | Workspace dependencies |
|---|---|---|
| `cvt-core` | Configuration, profiles, controller API, process supervision and application operations | None |
| `cvt-tui` | Interaction state, rendering and the terminal loop | `cvt-core` |
| `cvt` | CLI parsing, output formatting and executing TUI effects | `cvt-core`, `cvt-tui` |

Dependencies point toward `cvt-core`. Shared application operations belong in
`Service`; core code does not import terminal types or print command output.

## Core modules

| Module | I/O | Responsibility |
|---|---|---|
| `model` | No | Configuration document, proxies, groups and rules |
| `enhance::{path,merge,overlay,diff}` | No | Document edits, merging and structural differences |
| `validate` | No | Whole-document diagnostics with stable codes |
| `profile::item` | No | Profile types and options |
| `error` | No | Shared errors and concise status messages |
| `paths` | Yes | Home discovery, directory layout and atomic writes |
| `settings` | Yes | Preferences, validation and persistence |
| `profile::store` | Yes | Profile index, chain and documents on disk |
| `profile::source` | Yes | Subscription fetching, decoding and update scheduling |
| `enhance::pipeline` | Yes | Read the profile chain, generate, commit and snapshot configuration |
| `mihomo` | Yes | Controller client, event streams and process supervisor |
| `service` | Yes | Public application facade, paths, settings and adapter construction |
| `service::{deployment,lifecycle,selection}` (private) | Yes | Apply/reload/rollback, process lifecycle and bounded selection replay |
| `service::backup` (private) | Yes | Backup copying, restore and retention behind the `Service` API |

Pure transformations accept values. Adapters own filesystem, network,
environment and process access. Tests pass explicit temporary `AppPaths` to
avoid depending on the user's home.

### Configuration and enhancement

`model::Config` wraps an ordered JSON map. Unknown fields and key order survive
parsing and serialization; typed accessors read this document without defining
its entire schema. Rule text, including nested logical rules, is preserved by
`model::rule`. Validation reports multiple findings with stable codes; see
[the diagnostic reference](DIAGNOSTICS.md).

```text
profile::store: resolve the ordered chain and read its documents
    -> enhance::pipeline: apply each profile's transformation in chain order
    -> validate: collect errors and warnings
    -> Outcome: rendered configuration, diff and per-profile results
    -> Pipeline::commit: snapshot the previous runtime file, then write the new one
    -> Service::reload: hot reload or restart, with rollback when configured
```

`Pipeline::generate` reads files but does not write or contact the core. This
allows preview and validation before `commit`. Key contracts are:

- A failed `enhance::path` edit leaves the document unchanged.
- Re-running generation on the same source documents produces the same
  configuration. Reapplying a positional overlay to its own output is not
  generally idempotent: list indices can refer to different elements after an
  edit. See [the override format](OVERRIDE-FORMAT.md).
- Overlay rule insertion keeps an existing terminal rule last by default; a
  new terminal rule replaces it. The validator warns about unreachable rules
  in imported configurations rather than rejecting them solely for that reason.
- Application controller settings take precedence over the base profile;
  enhancements cannot change control-plane keys. See
  [ADR 0008](adr/0008-control-plane-ownership.md).
- `Pipeline::SNAPSHOT_LIMIT` bounds generated snapshots used for rollback.
  These snapshots are separate from backups of user-maintained state.

### Controller and process adapters

`mihomo::endpoint` represents TCP, TLS, Unix sockets and Windows named pipes.
`mihomo::client` exposes typed API methods, with contract tests for request
encoding and response decoding. Ordinary requests use `DEFAULT_TIMEOUT`;
configuration reloads and latency tests use operation-specific timeouts.
The UI refresh interval does not control request timeouts.

`mihomo::stream` provides WebSocket and HTTP newline-delimited transports with
reconnection and bounded queues. `mihomo::supervisor` locates and controls the
core, validates configuration with `mihomo -t`, and records process identity so
a recycled pid is not mistaken for the managed core. Platform-specific behavior
is implemented in that adapter.

### Subscription updates

`profile::source` attempts direct fetching, then the deployed core's proxy,
then the system proxy. Bodies are size-limited; plain-text and base64 payloads
are supported. Response metadata supplies subscription usage information, and
`update_all_due` respects each profile's update interval.

### Applying configuration

`Service` exposes operations shared by the CLI and TUI. `apply` generates and
commits a document, then reloads it according to `ReloadMode`. Automatic mode
tries hot reload before restart. When restart fails and rollback is enabled,
it restores a previous runtime snapshot and attempts to start that configuration.
An `ApplyReport` distinguishes an applied configuration from a rollback.

After a successful reload, the service waits for the expected groups and
replays saved selections. A `/version` response only establishes that the
process is reachable; groups may still be rebuilding. These waits have bounded
budgets, with each controller request bounded by the remaining time. Selection
replay is best effort: missing groups must not consume the whole budget before
valid choices are attempted.

### Backups and restore

The private `service::backup` module implements `Service::backup`, `backups`,
`restore` and `prune_backups`. Public import paths for `Backup` and `BACKUP_LIMIT`
remain under `service`.

The module owns the lists of state files and directories used for copying and
recognising backups. It excludes generated runtime files, the core's working
data and logs. Sources reached through symlinks are skipped; destination checks
prevent copies through unrelated symlinks or into special files. Same-file
copies are no-ops, and Unix hard-linked destinations are replaced before writing.

A restore is additive: saved files are copied back and unrelated files remain.
It validates destinations and creates a safety backup before copying. Retention
runs after the source has been read, so restoring the oldest backup cannot prune
that source prematurely. The safety backup is retained even if its timestamp
sorts behind other backups. This is not an atomic directory transaction; I/O
failure during copying can leave partially restored state, with the safety
backup available for recovery.

## TUI and effect execution

`App` separates interaction state from effect execution. State transitions do
not perform filesystem, network or process I/O. The binary loads settings through
`Service`, injects them with `App::with_settings`, and starts `run::run_app`.
The compatibility constructor `App::new` still reads settings from its home and
falls back to defaults; callers that need explicit error handling should load
settings themselves and use `with_settings`.

```text
App::on_event(Event) -> Vec<Effect>
App::on_tick()       -> Vec<Effect>
ui::render(frame, &app)
```

The binary's executor translates effects into `Service` and adapter calls, then
returns `Data`, `Done` or `Failed` events. Failures reach the status line instead
of unwinding the terminal loop. `cvt-tui::run` owns terminal setup, input and
restoration; renderers consume state.

| Module | Responsibility |
|---|---|
| `action` | User actions and labels |
| `keys` | Screen-specific keys and overlay precedence |
| `i18n` | Localized labels and formatted messages |
| `theme` | Semantic color roles and monochrome rendering |
| `state` | Tables, sorting, filtering, log buffers and metrics |
| `row` | Convert model values into display rows |
| `app` | State container, initialization, observable state and event entry point |
| `app::protocol` | Typed effect commands and result events; re-exported through `app` |
| `app::{input,actions}` | Modal input handling and action dispatch |
| `app::{navigation,details}` | Selection, filtering and complete detail views |
| `app::settings` | Validated setting edits and selection dialogs |
| `app::settings::catalog` | Setting labels, editable values and allowed choices |
| `app::{profiles,proxies,lists,testing}` | Domain actions and test queue state |
| `app::updates` | Reduce incoming data and operation results into state |
| `app::overlay` | Modal and status data types |
| `ui` | Screen renderers |
| `run` | Terminal lifecycle and event loop |

### Executor boundaries

`cvt::executor::Executor` owns shared services and task coordination. Its main
`Effect` match is exhaustive: adding a command requires choosing its handler.
The private modules keep these responsibilities together:

| Module | Responsibility |
|---|---|
| `configuration` | Generate/commit and apply the chosen reload policy |
| `lifecycle` | Startup actions, core launch/readiness and managed core updates |
| `settings` | Authorization and persistence, followed by the requested runtime change |
| `selection`, `profiles` | Node choice persistence and subscription downloads |
| `refresh`, `streams` | Screen reads and long-lived controller subscriptions |
| `inventory`, `adapters` | Model-to-view conversions and editor integration |
| `probes`, `diagnostics` | Probe scheduling, direct node probes, exit IP and bandwidth |

This uses a facade (`Service`), typed commands (`Effect`) and event reducers
(`App`), with adapters for external I/O. Internal modules use explicit imports
and keep helper functions private unless another module needs them. Existing
public import paths are preserved. `App::with_settings` and `run_app` let the
caller supply initialized state. Internal splits do not require new public traits.

When extending these layers:

- Add command and result data to the protocol. Keep application I/O in the
  executor and terminal I/O in `run`.
- Put domain behavior in its owning module; keep dispatch focused on routing.
- Keep public re-exports stable and expose internal helpers only to modules
  that need them.
- Prefer an explicit input or shared workflow over another global dependency
  or a duplicate sequence of persistence steps.
- Keep tests beside their owning layer, grouped by behavior. Moving code also
  requires updating source fixtures and architecture references.

The service mutex protects shared settings and profile operations. Network
jobs generally copy their inputs before awaiting. Configuration application
and settings persistence still hold the mutex on a blocking worker while they
perform their serialized work; they are not lock-free transactions.

An open prompt consumes input before the screen key map, so `q` can be entered
as text. Refreshes preserve table selection by identity rather than row index.
These contracts are covered by state-machine and rendering tests.

The TUI language is saved as `ui.language`. Setting copy is selected by its
persisted setting key, action labels by `Action`, and formatted messages by
typed message variants. Context-specific text has its own key: cursor movement,
download traffic and an unavailable proxy may all read “down” in English but
have different Chinese text. Renderers pass data into `i18n` and keep profile
names, rule contents and core errors as user or external data. Shared legacy
copy still uses an English-string lookup and falls back to English for unknown
phrases; new context-sensitive copy should use a semantic key.

## CLI

The command modules mirror the CLI's subcommand groups. The binary owns argument
validation, human-readable output, JSON schemas and exit codes. Detailed command
contracts belong in [CLI.md](CLI.md).

Most commands use `Service` or its controller client. `geo` and `unlock` make
HTTP requests through the core's proxy to test the tunnel itself. Their shared
client setup and limits live in `commands/mod.rs`.

## Testing

| Layer | Location | Purpose |
|---|---|---|
| Unit and property tests | Module tests and `cvt-core/tests/invariants.rs` | Local behavior and generated-input invariants |
| API contracts | `cvt-core/tests/client_contract.rs` | Controller request and response contracts |
| State-machine tests | `cvt-tui/src/app/tests/` | Interaction and effect sequencing without a terminal |
| User-flow rendering | `cvt-tui/src/ui/render_tests.rs` | Profile metadata, page transitions, live refresh, sorting, logs and CJK text drawn into a terminal buffer |
| Runtime ordering | `cvt/src/executor/tests.rs` | Reconcile controller inventory with generated configuration order |
| Live core checks | `cvt-core/tests/live_controller.rs` | Verify assumptions against a real, disposable core |
| Regression tests | `regression_*.rs` in both libraries | Reproduce previously found failures |

Live checks are environment-gated and do not run unless `CVT_LIVE_CONTROLLER`
is set. Their setup and side effects are documented in the test file. A green
default test run does not establish live-core compatibility.

For each user-visible defect, add a regression at the boundary where it was
observed: source metadata through its list cell, events through a rendered
screen, or controller data through the executor. A state-only assertion cannot
prove that the first frame is legible, and a rendering-only assertion cannot
prove that a profile switch refreshes live data. The profile-to-proxy flow in
`render_tests.rs` exercises both layers together. Before release, run an
isolated TUI against a real Mihomo at both 80×24 and a larger terminal size:
switch profile while stopped, start the core, revisit Proxies, expand a group,
measure one node, change member sort, then follow and scroll Logs. Check source
group order and names with emoji as well as the absence of spurious controller
errors. Keep private subscription URLs and node names out of test fixtures and
CI logs.

### Regression suite map

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

Each regression file is a separate Cargo integration test target, keeping its
fixtures and process state in its own executable. Existing suites may cover
multiple subjects because they originated in cross-cutting reviews. New cases
belong with the behavior they protect; use descriptive test names and record
only the evidence needed to understand the input and expected result.

Correct mistaken test premises only with reproducible evidence. Keep essential
counterexamples executable; temporary review reports and progress logs do not
belong in the repository. [ADR 0009](adr/0009-adversarial-review.md) records the
review rationale.
