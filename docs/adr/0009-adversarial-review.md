# 0009 — Independent review and regression evidence

**Status:** accepted

## Context

Configuration deployment, rollback and controller operations can fail even
when unit tests pass. A simulated controller does not prove that the real core
accepts a configuration or has finished applying it.

## Decision

Use independent review for substantial changes to deployment and state
management. Review observable behavior and failure paths, including incomplete
updates, stale responses and rollback. Review may be performed by a human or
an automated reviewer; findings require reproduction before changing behavior.

Keep confirmed defects as focused regression cases in the relevant domain
suite. Name tests for behavior, not review rounds. Use property tests for
invariants and interaction or live-core checks for integration assumptions.
The testing layers and their scope are described in
[ARCHITECTURE.md](../ARCHITECTURE.md#testing).

When a test encodes an incorrect assumption, correct it using documented
behavior or a reproducible observation. Do not weaken assertions simply to
make a failing suite pass. Preserve the relevant explanation beside the test.

## Consequences

- Review findings remain reproducible after their original reports become stale.
- Tests and independent review complement runtime verification.
- Fake controllers keep failure paths deterministic but cannot establish real
  core compatibility on their own.
- Review effort should follow the risk of the changed behavior, rather than
  impose a particular reviewer tool or retain historical review transcripts.
