# 0003 — Support the mihomo core only

**Status:** accepted

## Context

There are three cores in circulation: the original Clash, Clash Premium, and
mihomo (formerly Clash.Meta, and itself the successor that most of the
ecosystem moved to). They share a configuration vocabulary and an HTTP API
that *look* like the same interface and are not.

The differences are not cosmetic. Clash Premium has no `RULE-SET` and no
`rule-providers`. Support for `sub-rule`, `smart` groups, `tun` and the
DNS-with-`fake-ip` behaviour differs between them and between versions of the
same one. Error envelopes differ. `clashtui`, an earlier terminal client for
this problem, supports two cores, and its core-detection logic threads through
its configuration handling, its API client and its UI.

The tempting move is to support "the Clash family" and branch where it
differs. The cost of that is not the branching; it is that *every* feature has
to be validated against every combination, and the number of combinations
grows. A user of a core we half-support gets a bug report we cannot act on.

## Decision

mihomo only. One core, one API, one configuration vocabulary, and no
compatibility shims.

Where a capability genuinely varies — and it does, since mihomo's own optional
routes depend on how it was built — the program *asks* rather than assumes.
`Client::probe()` establishes what this particular binary exposes, and
`doctor` reports it, so "this build cannot disable rules" is a stated fact
rather than a mysterious failure.

## Consequences

- The configuration model, the validator and the API client are written
  against one target and can use its actual semantics: `RULE-SET`,
  `rule-providers`, `sub-rule`, the `GLOBAL`/`PASS-RULE` built-ins, and the
  PascalCase type names the API returns.
- Users of Clash Premium or the original Clash are not served. This is a
  deliberate loss of audience for a deliberate gain in correctness.
- There is no core-detection code anywhere, which removes a whole class of
  "it works on my machine" bugs.
- The capability probe has to be maintained as mihomo's optional surface
  changes, which is a small, honest cost paid in one place.

## Alternatives considered

**Dual-core like `clashtui`.** Rejected as above: it doubles the validation
matrix for a core that is already the minority case, and the earlier client's
history is the evidence.

**mihomo plus a documented "compatibility mode" for older cores.** Rejected
because a compatibility mode that is not continuously tested is a claim, not a
feature, and this project would not be running Clash Premium on a schedule.

**Support whatever core answers on the API port.** Tempting for the failure
mode it removes, but it means never knowing what configuration is legal, and
validation is the feature that keeps a user's network working.
