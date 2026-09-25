# `clash-verge-tui` command line

Everything the terminal interface can do, a script can do. This document is
written for someone who has never seen the source: it lists every subcommand,
every flag, the JSON shape each command prints, and the exit codes a script
should branch on.

```
clash-verge-tui [OPTIONS] [COMMAND]
```

Running it with **no subcommand** starts the terminal interface. Until that
interface is wired into the binary, the command says so and exits `1`; every
subcommand below works.

## Global options

| Option | Meaning |
|---|---|
| `--home <DIR>` | Application home. Overrides `CVT_HOME`; the platform data directory is the fallback. |
| `--json` | Print one machine-readable JSON object on stdout. |
| `-v`, `--verbose` | More diagnostics on stderr. `-v` info, `-vv` debug, `-vvv` trace. |
| `--no-color` | Never emit ANSI colour. `NO_COLOR` in the environment does the same. |
| `-h`, `--help` | Help. The exit-code table is appended to the top-level help. |
| `-V`, `--version` | Version. |

Global options are accepted before or after the subcommand:
`clash-verge-tui --json status` and `clash-verge-tui status --json` are the same.

## Environment

| Variable | Effect |
|---|---|
| `CVT_HOME` | Application home, used when `--home` is absent. |
| `CVT_CORE` | Explicit path to the `mihomo` binary, overriding the one in the home. |
| `NO_COLOR` | Any non-empty value disables colour. |
| `VISUAL`, `EDITOR` | Editor used by `config edit`; `vi` when neither is set. |
| `RUST_LOG` | Filter for `-v` tracing output, when set. |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | A generic failure. |
| `2` | Invalid usage. `clap` produces this itself, before any work happens. |
| `3` | The generated configuration failed validation. |
| `4` | The controller is unreachable, or the core is not running. |
| `5` | No core binary was found. |

How the library's errors map onto these:

| `cvt_core::Error` | Code |
|---|---|
| `Validation` | `3` |
| `ControllerUnreachable` | `4` |
| `CoreUnavailable` | `5` |
| everything else — `Io`, `Parse`, `Http`, `Api`, `ProfileNotFound`, `InvalidChain`, `MissingField`, `ProcessFailed`, `InvalidValue`, `Cancelled`, `Unsupported` | `1` |

Two deliberate choices:

* `Api` is **not** `4`. The controller answered, so it is running; a rejected
  request is an ordinary failure.
* `ProcessFailed` is **not** `3`. It covers both "`mihomo -t` rejected the
  document" and "the binary would not spawn", and guessing between them from
  the message would be worse than reporting a generic failure.

`doctor` exits with the code of its **first failed check**: the report is read
top-down, and the earliest failure is the one the rest follow from.

## The `--json` contract

* One JSON object per invocation, pretty-printed, on **stdout**.
* Its first field is `schema`, which names the shape, e.g. `cvt.status.v1`.
  It changes only when the shape does.
* Diagnostics, progress and warnings always go to **stderr**, in both modes, so
  `clash-verge-tui ... --json | jq` never has to filter prose out of the
  payload.
* A command that fails prints nothing on stdout and exits non-zero; the reason
  is on stderr. Scripts should branch on the exit code first.
* `logs --follow` is the one streaming command: with `--json` it prints
  newline-delimited JSON, one `cvt.logs.entry.v1` object per line.

`--json` is accepted by every command, including the ones that change things,
so a script never has to parse a table.

## Commands

### `status`

Core state, whether a binary was found and what version it reports, the current
profile, the generated configuration's summary, validation counts, and the
controller endpoint. This is the command to run when something is wrong: it
answers even when half the installation is missing, and it exits `0` whenever
it could gather the report. `doctor` is the one that fails.

*JSON:* `cvt.status.v1` — `home`, `core`, `profile`, `runtime_config`,
`generated`, `generation_error`, `controller`, `notes`.

```console
$ clash-verge-tui status
$ clash-verge-tui status --json | jq -r .controller.endpoint
```

### `doctor`

A diagnostic report. Checks run in this order, each reported as `pass`, `warn`
or `fail` with a hint:

| # | Check | Fails with |
|---|---|---|
| 1 | `home` — the home exists and is writable | `1` |
| 2 | `settings` — the settings file parses and the layout exists | `1` |
| 3 | `core-binary` — a `mihomo` binary can be located | `5` |
| 4 | `core-version` — the binary reports a version this program understands | `5` |
| 5 | `profile-index` — the index exists and is readable | `1` |
| 6 | `profile-current` — a profile is selected | `1` |
| 7 | `chain-documents` — every chain document is readable and parses | `1` |
| 8 | `config-validation` — the generated configuration validates | `3` |
| 9 | `core-validate` — `mihomo -t` accepts the generated configuration | `3` |
| 10 | `controller` — the controller is reachable and `probe()` succeeds | `4` |
| 11 | `controller-exposure` — the endpoint is loopback, or has a secret | `1` |
| 12 | `subscription` — remote profiles are fresh | `1` |

Notes:

* A check that could not run is a `warn` with the reason, never a silent pass.
* If the home itself is unusable the run stops there and `not_checked` lists
  everything that was skipped. `doctor` never creates the home it reports on.
* `controller-exposure` warns — it does not fail — when a controller without a
  secret is bound to a non-loopback address. That is a real security problem,
  but it is the user's decision to make.
* `mihomo -t` runs against a temporary copy of what *would* be generated, so
  the check is meaningful before the first apply and writes nothing.

*JSON:* `cvt.doctor.v1` — `home`, `checks[]` (`id`, `title`, `status`,
`detail`, `hint`, `data`), `passed`, `warnings`, `failures`, `verdict`,
`aborted`, `not_checked`.

### `profiles`

| Command | Effect |
|---|---|
| `list` | Every profile, with the current one marked `*` and explicit chain positions. |
| `add <url> [--name N]` | Add a subscription and download it. The name defaults to the URL's host. |
| `remove <uid>` | Remove a profile **and its document**. |
| `rename <uid> <name>` | Rename; the uid and the file do not change. |
| `switch <uid>` | Make a profile the base of the generated configuration. |
| `update [uid] [--all-due]` | Refresh one profile (default: the current one), or every remote profile whose interval has elapsed. |
| `import <dir>` | Copy profiles from a `clash-verge-rev` home. An existing uid is never overwritten. |
| `chain [<uid>...] [--clear]` | Print the chain, or replace it. `--clear` goes back to the automatic order. |
| `show <uid>` | Print a profile's document verbatim. |

`add` keeps the profile even when the download fails: retrying is one command,
and re-adding would lose the name you chose. A failed download still exits `1`.

`update --all-due` reports one row per profile and exits `1` if any failed.

*JSON:* `cvt.profiles.list.v1`, `cvt.profiles.added.v1`,
`cvt.profiles.changed.v1`, `cvt.profiles.update.v1`,
`cvt.profiles.import.v1`, `cvt.profiles.chain.v1`, `cvt.profiles.show.v1`.

### `config`

| Command | Effect |
|---|---|
| `generate [--apply] [--force] [--mode auto\|hot\|restart]` | Generate the runtime configuration. Without `--apply` nothing is written. |
| `show` | Print the deployed runtime configuration. |
| `validate` | Validate what *would* be generated, without writing anything. |
| `diff` | Diff the generated document against the deployed one. |
| `rollback` | Restore the most recent snapshot. |
| `snapshots` | List snapshots, newest first. |
| `edit` | Open the deployed configuration in `$VISUAL`/`$EDITOR`. |
| `path` | Print every path the application uses. |

`--force` requires `--apply`, and so does `--mode`: forcing a write that is not
happening would be a lie.

Validation errors exit `3`. `--apply` with errors refuses to write unless
`--force` is given. With `--apply`, a failed reload reports what happened —
including the rollback, if the core rejected the document and the previous
configuration was restored.

`edit` opens the **generated** file. It is regenerated on every apply; lasting
changes belong in an `override` profile.

*JSON:* `cvt.config.generate.v1` (includes `yaml`), `cvt.config.show.v1`,
`cvt.config.validate.v1`, `cvt.config.diff.v1`, `cvt.config.rollback.v1`,
`cvt.config.snapshots.v1`, `cvt.config.edit.v1`, `cvt.config.path.v1`.

### `proxies`

| Command | Effect |
|---|---|
| `list [group]` | Without a group: every policy group. With one: its members, delays and current selection. |
| `select <group> <node>` | Pin a group's selection. |
| `test <group> [--url U] [--timeout MS] [--concurrency N]` | Measure every member. |
| `test-all [--url U] [--timeout MS] [--concurrency N]` | Measure every node of every group, each node once. |
| `unpin <group>` | Clear the pinned selection. |

Latency is measured one node at a time — the group endpoint omits the nodes
that failed, and the reason they failed is the interesting half of the answer —
with `--concurrency` requests in flight. Defaults come from settings
(`test.url`, `test.timeout_ms`, `test.concurrency`).

*JSON:* `cvt.proxies.list.v1`, `cvt.proxies.selection.v1`,
`cvt.proxies.test.v1`.

### `connections`

| Command | Effect |
|---|---|
| `list [--limit N]` | Live connections, most recent first. `--limit 0` means no limit. |
| `close <id>` / `close --all` | Close one connection, or every connection. |

Exactly one of `<id>` and `--all` is required; passing both is a usage error.

*JSON:* `cvt.connections.list.v1`, `cvt.connections.close.v1`.

### `rules`

| Command | Effect |
|---|---|
| `list [--disabled] [--stats]` | The active rule set, optionally only disabled rules, optionally with hit counts. |
| `toggle <index>` | Flip one rule's enabled state. The index is the core's own. |
| `providers` | Rule providers, in name order. |
| `update-providers [name]` | Refresh one provider, or every provider. |

*JSON:* `cvt.rules.list.v1`, `cvt.rules.toggle.v1`, `cvt.rules.providers.v1`,
`cvt.rules.update_providers.v1`.

### `test`

| Command | Effect |
|---|---|
| `delay --group G` / `delay --all` | Measure a group, or every group. One of the two is required. |
| `urls --node N` / `urls --list` | Measure every configured URL through one node, or list them. |
| `dns <name> [--type A]` | Resolve a name through the core's DNS. |

`delay` accepts `--url`, `--timeout` and `--concurrency` like `proxies test`.

`urls` measures `test.urls` from `cvt.yaml` — `google`, `github` and `youtube`
out of the box — through a single node. The point is not the latency but
*which sites a node can reach*: a delay probe says a socket opened to one host,
and the host is the same for every node, so a node that cannot reach anything
useful still reports a healthy number.

`--url` accepts one of those names wherever it accepts a URL:

```console
$ cvt test delay --group PROXY --url youtube
$ cvt test delay --group PROXY --url https://example.com/generate_204
```

A value that is neither an http(s) URL nor a configured name is refused, with
the list in the message. Fetching a typo would fail, and it would look like a
node problem rather than a mistake.

*JSON:* `cvt.test.delay.v1`, `cvt.test.dns.v1`, `cvt.test.urls.v1`,
`cvt.test.targets.v1`.

### `core`

| Command | Effect |
|---|---|
| `status` | Process state, binary and version. |
| `start` | Validate and start the core with the deployed configuration. |
| `stop` | Stop the core. |
| `restart` | Stop, then start. |
| `version` | The binary's version, and the running core's version when reachable. |
| `upgrade [--channel release\|alpha] [--force]` | Ask the core to replace its own binary. |
| `geo` | Refresh the geo databases. |
| `gc` | Ask the core to run a garbage collection. Requires `log-level: debug`. |

`start` runs `mihomo -t` first: a crash loop is far harder to diagnose than one
error message.

*JSON:* `cvt.core.status.v1`, `cvt.core.action.v1`, `cvt.core.version.v1`,
`cvt.core.request.v1`.

### `logs`

| Flag | Meaning |
|---|---|
| `--level <LEVEL>` | Show this level and anything more severe: `silent`, `error`, `warning`, `info`, `debug`. |
| `--filter <TEXT>` | Only lines containing this text. |
| `--lines <N>` | How many lines to print without `--follow`. Default `200`; `0` means everything. |
| `--follow` | Keep printing as the core logs, until interrupted. |

Two sources, deliberately:

* **Without `--follow`** the command reads the file the supervisor captured the
  core's own output into. That is instant, complete, and works when the core is
  down.
* **With `--follow`** it attaches to the live `/logs` stream over the
  controller. Ctrl-C exits cleanly; a closed stream is reported, not a panic;
  and a core that was never reachable exits `4` instead of retrying forever.

Lines that do not carry a `level=` field are always shown: they are usually the
continuation lines of a multi-line message, and dropping them would cut a crash
report in half.

*JSON:* `cvt.logs.tail.v1`; with `--follow`, one `cvt.logs.entry.v1` object per
line.

### `theme`

Print the tab titles and every key binding, read from `cvt_tui::Keymap`, as
plain text. This makes the key map checkable — and diffable — without a
terminal.

*JSON:* `cvt.theme.v1` — `screens[]` (`index`, `digit`, `title`) and
`bindings[]` (`screen`, `context`, `keys`, `action`, `help`).

## Recipes

```console
# Is anything wrong, and why?
clash-verge-tui doctor; echo "exit: $?"

# Which node is the group actually using?
clash-verge-tui proxies list PROXY

# Deploy a change, then check what it did
clash-verge-tui profiles update --all-due
clash-verge-tui config diff
clash-verge-tui config generate --apply --mode auto

# Machine-readable, without a single line of prose on stdout
clash-verge-tui status --json | jq -r '.core.state, .controller.endpoint'

# Everything a bug report needs
clash-verge-tui doctor --json > doctor.json
clash-verge-tui status --json > status.json
clash-verge-tui logs --lines 500 > core.log
```
