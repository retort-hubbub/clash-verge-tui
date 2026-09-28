# Contributing

## Development checks

Run from the workspace root before merging:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --no-deps --all-features --locked
```

The checked-in GitHub workflow builds Linux release artifacts on tags or manual
runs. It does not currently run the full checks above, an MSRV matrix or a
dependency audit. Run `cargo deny check advisories bans licenses sources` when
reviewing dependency changes. Keep `Cargo.lock` committed; use `--locked` for
verification so an out-of-date lockfile fails visibly.

The default suite does not start a real mihomo instance. For controller changes,
run `crates/cvt-core/tests/live_controller.rs` against a disposable core using
that file's setup instructions. It replaces the core's configuration, so use an
isolated instance. Record whether live checks ran when reporting validation.

## Branching model (GitFlow)

| Branch | Purpose | Branches from | Merges into |
|---|---|---|---|
| `main` | Released, tagged code | — | — |
| `develop` | Integration branch | `main` | `release/*` |
| `feature/*` | One feature, fix or refactor | `develop` | `develop` |
| `release/*` | Stabilise a version | `develop` | `main` and `develop` |
| `hotfix/*` | Fix a released version | `main` | `main` and `develop` |

Preserve merge commits with `--no-ff` so a feature or release remains identifiable.
Do not commit feature work directly to `main` or rewrite released history.
When a hosted remote is available, use pull requests and configure branch
protection to require CI. In a local-only repository, run the same checks before
merging; branch protection and pull requests are hosting settings, not guarantees
made by files in this repository.

```bash
git switch develop
git switch -c feature/rule-editor
# Implement the change and run the development checks above.
git add <changed-files>
git commit -m "feat(rules): add a rule editor screen"
git switch develop
git merge --no-ff feature/rule-editor
git branch -d feature/rule-editor
```

### Releases

Create `release/X.Y.Z` from `develop`. Update the workspace version, internal
crate dependency requirements, `Cargo.lock`, and the changelog version section
and comparison links together. Version bumps belong on release or hotfix
branches. After validation, merge the branch into both `main` and `develop`,
and tag the release commit on `main` with `vX.Y.Z`.

Before tagging, build and exercise the release artifact:

```bash
cargo build --release -p cvt --locked
./target/release/clash-verge-tui --version
./target/release/clash-verge-tui --home /path/to/disposable-home status
./target/release/clash-verge-tui --home /path/to/disposable-home
```

The status command should report the state of that home; an empty home may have
no running core. Run the last command in a real terminal and press `q`. Verify
that the alternate screen closes and the cursor returns. The release profile
uses LTO, symbol stripping and `panic = "abort"`, which the ordinary test build
does not exercise. The panic hook must restore the terminal before aborting;
`Drop` and `catch_unwind` cannot recover from an abort.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```text
<type>(<optional scope>): <summary>
```

Types include `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`,
`ci`, `chore` and `revert`. Use a crate or area as the scope, such as `core`,
`tui`, `cli`, `profiles` or `config`. Keep the subject imperative, lower-case
and under 72 characters. For a fix, explain the triggering case and why the
change resolves it. Use `Closes #123` in the footer when there is an issue.

## Definition of done

- The development checks pass; report environment-gated checks separately.
- Behavior changes have relevant tests. A refactor preserves existing assertions
  and public behavior; add tests for newly discovered failure cases.
- Public items have doc comments, and user-visible changes update the relevant
  documentation and `CHANGELOG.md` under `Unreleased`.
- Tests use temporary homes and local fixtures. Do not commit subscriptions,
  credentials, generated logs or review scratch files.

## Architecture and documentation

Dependencies point toward `cvt-core`: the binary uses both libraries, and
`cvt-tui` uses `cvt-core`. Core code has no terminal dependency and returns values
or errors instead of printing command output.

Keep models, validation and document transformations free of I/O. Filesystem,
network, environment and process access belong in adapters such as `paths`,
`settings`, `profile::source`, `enhance::pipeline` and `mihomo`, coordinated by
`Service`. Pass explicit paths to tests. The TUI's `App` and renderers operate on
state; the terminal loop handles terminal I/O and the binary executes effects.

Document each fact in the place that owns it:

- `README.md`: getting started, user-facing features and limitations.
- `docs/CLI.md`, `docs/OVERRIDE-FORMAT.md`, `docs/DIAGNOSTICS.md`: detailed reference.
- `docs/ARCHITECTURE.md`: current module responsibilities and important invariants.
- `docs/adr/`: reasons for design decisions and their tradeoffs.
- Tests: executable regressions with the input and expected behavior.

Avoid copied feature ledgers, test counts, review-round narratives and temporary
line-number references. They drift when the implementation changes. Preserve
historical evidence where it explains a regression, but describe current
contracts in production comments and user documentation.
