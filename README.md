# clash-verge-tui

A terminal client for the [mihomo](https://github.com/MetaCubeX/mihomo) core
(Clash.Meta). It manages subscription profiles, generates and overrides the
runtime configuration, and talks to a running core to select nodes, watch
connections and logs, toggle rules and measure latency from the terminal.

```console
$ clash-verge-tui              # the interactive interface
$ clash-verge-tui status       # one line about the current state
$ clash-verge-tui doctor       # what is installed, what is missing, what the core supports
```

In the TUI, each tab shows its direct key in brackets: `[1]` through `[9]`.
Press that number to open the tab, or use Tab / Shift+Tab to move between tabs.
With a mouse, click a tab, table row or dialog choice; scroll the wheel to move
through tables, logs and scrollable dialogs. Keyboard shortcuts remain available
for actions such as activating a profile or confirming a change. Press `m` to
read the complete latest status message, including after its footer notice has
disappeared; press Esc to dismiss a footer notice immediately.
On Home, press `U` to download or update the managed Mihomo core. Once a profile
is selected, `s` starts the core and generates the first runtime configuration
automatically.
Press `M` on Home, Proxies or Rules to cycle the running core through rule, global and
direct routing modes. The current live mode appears in the core summary. Mode
changes made this way last until the next core restart or profile apply.
The Proxies tab also shows the selected profile's groups and declared members
while the core is stopped; live health and provider membership appear after it
starts.
Press `?` for the full key reference. The interface supports English and
Simplified Chinese; change **language** on the Settings tab (key `8`) and press
`s` to save it. The preference is stored as `ui.language: zh-CN` in `cvt.yaml`.
Command-line output and configuration field names remain in English.

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

**Subscriptions.** Three-tier fetching: direct, then through a running core's
mixed or HTTP proxy port when configured, then through the system proxy — so an
update works on a machine whose only route to the internet is the proxy it is
updating. `subscription-userinfo` is parsed so the interface can show what is
left.

**The running core.** Select nodes, pin a selection, run latency and
DNS tests, list and close connections, watch traffic and memory, follow the
log stream, toggle rules, update rule providers, switch mode, and start, stop,
restart, upgrade or garbage-collect the core itself.

On **Proxies**, press `v` to cycle latency methods. CONNECT uses
Mihomo's named-proxy URL test and measures the working proxy path. TCP opens a
direct connection to the node server and ICMP sends a direct echo to that
server; these two measure server reachability, not proxy throughput. Nodes
whose server address is unavailable in the deployed configuration can still
use CONNECT. CONNECT makes a warm-up request before reporting a reading; a
local TUN may intercept direct TCP connections, in which case the TCP test
reports that interception instead of a misleading local handshake time.
Press `s` to cycle source, fastest and slowest member order; group headings
always keep their source order. Press `b` to choose a current-route bandwidth
test: a 4 MB, 20 MB or 100 MB download sample, or `speedtest-go`. The latter
uses Mihomo with saving mode and upload disabled. If it is missing, the TUI
asks before downloading a verified release into the application directory;
`B` offers the same installation command directly. The result
measures the current routing policy, not necessarily the highlighted node.
ICMP requires the system `ping` command and may be blocked by a network.

The **Tests** tab runs 12 streaming and AI availability checks, including
Netflix, Disney+, YouTube Premium, ChatGPT, Claude and Gemini. Requests use the
local Mihomo proxy and the current route; Enter runs one check, `a` runs all
checks in sequence, `s` cancels a batch, and `c` clears all results. The checks use service responses and regional
hints, which can change when providers update their sites.

**Two front ends, one implementation.** The interactive interface and the
command line are both thin shells over the same `Service` facade, so they
cannot disagree about what a profile chain means or how an apply is sequenced.

## Feature coverage and limits

| Area | Terminal interface | Command line |
|---|---|---|
| Profiles and subscriptions | List, switch, edit, update one or all due, import | The same, plus editing a subscription URL |
| Configuration | Preview, apply, edit profiles and overrides | Generate, validate, diff, apply, roll back |
| Running core | Nodes, connections, logs, rules, CONNECT/TCP/ICMP latency, current-route download speed, unlock checks and resource use | Node and group URL latency, DNS and named URL tests, plus core controls |
| Diagnostics and maintenance | Core and Geo database updates | Doctor, media unlock (YouTube Premium, Netflix, ChatGPT, Disney+), exit IP/geolocation, local backup and restore |

The TUI's **update all** action updates subscriptions that are due according to
each profile's interval; `profiles update <uid>` explicitly updates one. The
`core.auto_start` and `update.update_on_start` settings apply when the TUI
launches. Neither setting installs a background system service.

This project does not execute JavaScript enhancement scripts. It uses
[declarative overrides](docs/OVERRIDE-FORMAT.md); a subscription that requires
its own script cannot be reproduced automatically. It does not manage the
system proxy, PAC, the system resolver, privileged TUN setup, or WebDAV backup.
Those desktop and remote-sync operations need external tools. `dialer-proxy`
chains can be inspected and validated, but are not rewritten automatically.
TLS, Unix-socket and Windows-pipe controller addresses are recognised; the
pipe transport depends on Windows. The command line's unlock checks cover four services; the TUI presents 12.

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
│   └── config.previous.yaml
├── snapshots/             the last 20 generated documents, newest first
├── backups/               `backup create` output: settings, index, profiles, overrides
├── core/
│   ├── mihomo             where a core binary is looked for
│   └── work/              the core's own working directory, passed as -d
└── logs/                  core.log and app.log, rotated on start
```

A *snapshot* is one generated document, kept so a bad apply can be undone in
seconds; a *backup* is everything a person would have to recreate by hand. The
names are close enough to be worth the sentence, and the two live at the top
level rather than inside `runtime/` because neither is derived — this tree is
what `doctor` and `cvt config path` print.

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

The counterexamples from independent reviews remain as **passing regression
tests** in `crates/cvt-core/tests/regression_*.rs` and
`crates/cvt-tui/tests/regression_*.rs`. The [test suite map](docs/ARCHITECTURE.md#adversarial-review-and-where-its-counterexamples-live)
shows which area each file covers.

## Relationship to other projects

The TUI's 12 unlock probes in `crates/cvt/src/media_unlock/` adapt the
`clash-verge-media-unlock` crate from
[clash-verge-rev at `897a117dc5fc`](https://github.com/clash-verge-rev/clash-verge-rev/tree/897a117dc5fc), licensed
GPL-3.0-or-later like this project. The adaptation replaces its logging and
country-code registry integrations and connects individual probes to the TUI.

[clash-verge-rev](https://github.com/clash-verge-rev/clash-verge-rev) is a
Tauri desktop application for the same core, and this project borrows its
profile model and file layout on purpose — being able to point at an existing
`clash-verge-rev` home and import it is worth more than a novel layout.
[clashtui](https://github.com/JohanChane/clashtui) is an earlier TUI for the
same problem; its existence is why this one is written as a library with a
thin TUI over it rather than as a TUI with logic inside it.

This is an **independent implementation**, not a fork. The unlock probe
module is the code adaptation described above.

## Licence

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
