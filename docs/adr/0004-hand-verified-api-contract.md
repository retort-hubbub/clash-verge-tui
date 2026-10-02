# 0004 — API contracts based on observed Mihomo responses

**Status:** accepted

## Context

Mihomo **v1.19.31** responses differ from its API documentation in the following
cases:

- `GET /rules` returns rule types in Go's PascalCase (`DomainSuffix`,
  `RuleSet`, `Match`), not the configuration's SCREAMING-CASE spelling, so a
  client that reuses its config vocabulary sends the wrong type back.
- `GET /connections` returns `"connections": null` when there are none, not an
  empty array. Deserialising into `Vec<_>` fails on a proxy with no traffic.
- `GET /providers/rules/<name>` does not exist and answers `405`, not `404`.
- A rule is disabled with `PATCH /rules/disable` and a body that maps the rule
  *index as a string* to a boolean. `PUT` answers `405`; a bodyless request is
  inert and looks like success.
- The proxy delay endpoint parses `timeout` as a signed 16-bit integer, so
  32768 is rejected by the core and `0` means failure — except for a *group*
  delay, where `0` is a legitimate result and failure is a second `503`.
- `GET /version` returns only `{meta, version}`, with a leading `v`.

None of these is discoverable by reading the docs. All of them are
discoverable in an afternoon by pointing `curl` at a running core, which is
what the research document at `research/mihomo-api.md` records.

## Decision

The API client is written against an observed contract, not a documented one.
`research/mihomo-api.md` is the authority, it states which binary and version
each fact was observed against, and where it contradicts the upstream
documentation it says so explicitly and explains the evidence.

The contract has two test layers:

- `crates/cvt-core/tests/client_contract.rs` uses fake HTTP and Unix-socket
  controllers with responses observed from Mihomo. It runs without a core.
- `crates/cvt-core/tests/live_controller.rs` checks a real core when explicitly
  enabled, including selection persistence through `cache.db` across reloads.

## Consequences

- Fake-controller fixtures must match observed core responses, including
  `null` connection lists and string-encoded metadata ports.
- A fake controller cannot detect upstream response changes. Core upgrades
  require live checks and corresponding fixture updates.
- Live checks require a core; fake-controller checks run without one.

## Alternatives considered

**Trust the documentation and handle differences as bug reports arrive.**
Rejected: the differences are not edge cases, they are the normal path. A
`null` connection list on an idle proxy would be a crash on first use.

**Vendor a copy of the core's Go source as the specification.** Rejected as
the *primary* method — the router and the handlers are the truth, but reading
them is slower than asking the binary, and a running core settles ambiguous
cases (which HTTP status, what content type) that source reading leaves open.
The source was read for the parts a request cannot answer, such as which
routes are conditional on a build tag.

**Write only the live test and drop the fake.** Rejected because the live test
cannot run in CI, and a suite that only runs on a machine with a core
installed is a suite that does not run.
