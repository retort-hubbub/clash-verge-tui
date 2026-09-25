# Contributing

## Branching model (GitFlow)

This repository follows [GitFlow](https://nvie.com/posts/a-successful-git-branching-model/).

| Branch | Purpose | Branches from | Merges into |
|---|---|---|---|
| `main` | Released, tagged code only | — | — |
| `develop` | Integration branch; always buildable | `main` | `release/*` |
| `feature/*` | One feature or fix | `develop` | `develop` |
| `release/*` | Stabilisation of the next version | `develop` | `main` **and** `develop` |
| `hotfix/*` | Urgent fix to a shipped version | `main` | `main` **and** `develop` |
| `support/*` | Long-lived maintenance of an old major | `main` | — |

Rules that are enforced by CI or review:

* `main` is **protected**: no direct pushes, no force pushes, linear history.
* Every change reaches `develop` through a pull request with a green pipeline.
* Version bumps happen **only** on `release/*` and `hotfix/*` branches, never on
  `develop`, so that `develop` never needs a merge-back conflict resolution over
  `Cargo.toml`.
* Merge commits are preserved on `main` (`--no-ff`) so that a release is a
  single, revertible unit of history.

### Day-to-day

```bash
git switch develop && git pull
git switch -c feature/rule-editor
# ... work ...
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
git commit -m "feat(rules): add a rule editor screen"
git switch develop && git merge --no-ff feature/rule-editor
git branch -d feature/rule-editor
```

### Cutting a release

```bash
git switch -c release/0.2.0 develop
# bump versions, update CHANGELOG.md, only fixes from here on
git switch main && git merge --no-ff release/0.2.0
git tag -a v0.2.0 -m "v0.2.0"
git switch develop && git merge --no-ff release/0.2.0
```

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<body>

<footer>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`,
`ci`, `chore`, `revert`. Scopes are crate or area names: `core`, `tui`, `cli`,
`profiles`, `proxies`, `connections`, `logs`, `rules`, `tests`, `config`.

The subject is imperative, lower-case, and under 72 characters. A commit that
closes an issue says `Closes #123` in the footer.

## Definition of done

A change is complete when **all** of the following hold:

1. `cargo fmt --check` is clean.
2. `cargo clippy --all-targets --all-features -- -D warnings` is clean.
3. `cargo test --workspace` passes.
4. New behaviour has tests; new public items have doc comments (the crate is
   `#![warn(missing_docs)]`).
5. User-visible changes update the relevant file in `docs/`.
6. `CHANGELOG.md` has an entry under `## [Unreleased]`.

## Architecture rules

The dependency direction is one-way and enforced by review:

```
cvt (binary)  ->  cvt-tui  ->  cvt-core
```

* `cvt-core` must not depend on `ratatui` or `crossterm`, and must not read
  environment variables or print to stdout. It takes an [`AppPaths`] and
  returns values.
* `cvt-tui` must not spawn processes or open sockets directly; it calls
  `cvt-core`.
* The binary crate wires the two together and owns CLI parsing.

Keeping this boundary is what makes `cvt-core` testable without a terminal and
reusable from a future GUI.
