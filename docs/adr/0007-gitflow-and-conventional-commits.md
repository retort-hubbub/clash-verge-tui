# 0007 — GitFlow with Conventional Commits

**Status:** accepted

## Context

This is a program that touches a user's network configuration, and the failure
mode that matters is not "a feature is missing" — it is "a change broke
working connectivity". Two things follow. A release needs a point that can be
identified and returned to, and the history needs to explain *why* a change
was made by a stranger six months from now, without a ticket tracker to
consult.

There is also an unusual constraint in this codebase: several known defects
are preserved deliberately as passing counterexample tests with their reasoning
in the test body. The history records when each case was reproduced and fixed.

## Decision

**GitFlow.** `main` holds released code and nothing else. `develop` is where
work integrates. Work happens on `feature/*`, `release/*` and `hotfix/*`
branches, tagged on `main` with `vX.Y.Z`, and merged back so that no fix
reaches a release without reaching `develop`. The full table is in
`CONTRIBUTING.md`.

**Conventional Commits.** Every commit is `type(scope): summary`, with the
types `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`, `ci`,
`chore`. The scope names the crate or the area (`core`, `tui`, `cli`,
`config`, `docs`). Commits are focused: one change, with the reasoning in the
body rather than in the subject.

The body is not decoration. A commit that fixes a defect says what the defect
was, how it was reproduced, and why this fix rather than another. A commit
that introduces a limitation says so.

Validation requirements are documented in `CONTRIBUTING.md`. The current
workflow builds Linux release artifacts; branch checks, an MSRV matrix and
dependency audits are not currently automated by the repository.

## Consequences

- A release can be identified and returned to, and a hotfix can reach `main`
  without dragging unreleased work with it.
- The history is readable without a tracker: `git log --oneline develop`
  describes what changed, and the bodies describe why.
- `main` lags `develop` deliberately, so a fix merged to `main` must also be
  merged back. Forgetting that is the standard GitFlow mistake, and it is why
  the workflow is written down rather than assumed.
- Commit discipline is a real cost on every change. It is paid because the
  alternative — a history of "fix stuff" — is the thing that makes a
  three-crate codebase hard to pick up.

## Alternatives considered

**Trunk-based development with feature flags.** The better default for a
continuously deployed service. Rejected because there is no deployment here: a
release is a tagged build somebody installs, and `main` holding only released
code is worth more than a short-lived branch model.

**GitHub Flow — one long-lived `main` and short branches.** Simpler, and a
defensible choice for a project this size. Rejected because the release branch
is the mechanism that lets a hotfix ship without also shipping everything else
that landed in `develop`, and that is a property worth having in a program
that can break a user's network.

**Free-form commit messages.** No cost, and no history. Rejected: the
counterexamples depend on the history being legible, and so does
reviewing a change to the reload decision tree.
