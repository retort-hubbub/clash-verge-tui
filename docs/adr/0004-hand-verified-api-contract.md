# 0004 — A hand-verified API contract that outranks the official documentation

**Status:** accepted

## Context

The core's REST API is documented, and the documentation is wrong in ways that
break a client rather than annoy it. Six examples, all observed against a real
mihomo **v1.19.31**:

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

Two consequences follow from taking that seriously:

- **The contract is tested against a fake that speaks the observed bytes.**
  `crates/cvt-core/tests/client_contract.rs` drives the client with a
  hand-rolled HTTP and unix-socket controller that answers exactly what the
  real core answered, quirks included. This is what makes the suite runnable
  with nothing installed.
- **The claims that only a real core can settle are verified against one.**
  `crates/cvt-core/tests/live_controller.rs` replays the same expectations
  against a real binary. It is environment-gated, so it is a no-op on a
  machine without a core — and it has already earned its place: run for the
  first time, it failed on its fourth assertion, because a `select` group's
  remembered choice survives in the core's `cache.db` across a configuration
  reload.

## Consequences

- The client is correct against the real thing on the first try, which is the
  whole point.
- The research document is a maintained artifact, not a one-off. A core
  upgrade that changes a response shape will not be caught by the fake, which
  is a real limitation of this approach.
- Behaviour is verified rather than assumed, so the client does not carry
  defensive code for problems that do not exist — and does carry it for the
  ones that do, such as the `null` connection list and the string-encoded
  metadata ports.
- Two testing layers have to be kept in step. The compensation is that the
  cheap layer runs everywhere and the expensive layer catches what only a real
  core can.

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
