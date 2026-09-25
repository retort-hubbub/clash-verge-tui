# 0009 — Adversarial review, and keeping its counterexamples

**Status:** accepted

## Context

This program holds a user's network configuration and can write the file the
core is started from. The failure modes that matter are not "a feature is
missing" but "the configuration is wrong and the program says it is fine", and
the second kind is exactly the kind its author cannot see: a test written by the
person who wrote the code checks the behaviour they intended, which is the
behaviour they already believe in.

That is not a hypothetical. Over five rounds of review this codebase produced:

| Round | What it checked | What it found |
|---|---|---|
| 1 | the code | 18 defects, in four major |
| 2 | the 18 fixes | **10 problems with the fixes**, including a path traversal round 1 had missed |
| 3 | those fixes | **10 more**, including the other half of that traversal: the field next to the one round 2 had named |
| 4 | those fixes | **11 more**, and the *fourth* instance of the same pattern — a guard covering the field somebody named rather than the class it belongs to |
| 5 | the newest feature | (in progress) |

Two of those rounds found a fix that was **narrower than the defect it was aimed
at**, and one found a fix that traded a silent data loss for a different one.
None of them would have been found by re-running the author's tests, because the
author's tests passed at every point.

## Decision

Every round of substantial work ends with an **independent adversarial pass** by
an agent that did not write the code, whose instructions are to falsify the
claims rather than confirm them, and which is explicitly told that the pattern
so far is that the author's tests pass while the claim is still false.

Two consequences are part of the decision rather than incidental:

**The counterexamples are kept.** Each finding becomes a test in the build —
`recheck.rs`, `recheck2.rs`, `recheck3.rs` — with its minimal input and the
observation in the body. A report is a document *about* the code, lists defects
that were open when it was written, and is deliberately not in the repository;
the test is the part that belongs in it. Fixing a finding shows up in the
history as a commit that inverts an assertion, with the evidence that settled it
in the comment above.

**An inverted assertion is the dangerous kind.** When a finding turns out to be
an over-strong *claim* rather than a defect — a positional removal is not
idempotent, and the promise was wrong rather than the behaviour — the fix
changes the documentation and the test is rewritten to assert what is now
documented. That is the one place where a wrong belief can be frozen into the
suite, so each inversion carries the evidence that settled it.

The review does not replace the author's own tests, and it is not a substitute
for running the program. The selection-memory feature passed every unit test
while doing nothing at all: the replay landed in the window where the core
applies a reload in the background, so the index said the choice had been made,
the command exited 0, and the core still held the old node. An end-to-end run
found it on the first try.

## Consequences

- Five rounds have found 49 defects and 45 of them were invisible to the tests
  that existed at the time.
- The suite is larger than the project strictly needs, and a reader has to know
  which file is whose: `invariants.rs` attacks the library's own claims,
  `recheck*.rs` attack the claims of *fixes*. Each file says so at the top.
- Review rounds cost a full round each. The alternative has been measured: two
  of the five rounds found a fix that was narrower than its defect, and both
  would have shipped.
- The reviews have their own failure mode, recorded here because it happened:
  one asserted `ends_with("/sub?token=abc")` against a whole request line, whose
  failure message proved the fix worked; another encoded the *buggy* state in an
  assertion whose own message described the bug. Reviewer findings are therefore
  reproduced before they are acted on, and a disagreement is written down rather
  than silently absorbed.

## Alternatives considered

**Trust the author's tests.** They were green at every one of the five rounds,
including the rounds where the code did not work. Rejected on that evidence.

**Have the reviewer fix what it finds.** Tried in one round and rejected: the
reviewer becomes the author of the thing under review, and the next round has
nothing independent to check. The reviewer writes tests and reports; the author
fixes and is reviewed again.

**Keep the reports and delete the failing tests.** Simpler, and the report is
better reading. Rejected because a report is prose: it can be wrong, and nothing
notices. A test that fails is a claim with a mechanical consequence, and the
build refuses to be green while it stands.
