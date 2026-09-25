# 0002 — Keep configuration as a lossless ordered document, not a typed struct

**Status:** accepted

## Context

A mihomo configuration is not a schema this program owns. It is a document
that a subscription provider wrote, that the core reads, and that the user may
have edited by hand. It contains fields this program has never heard of, it
gains new ones every time the core is released, and it is full of type
surprises: `port` and `socks-port` are integers, the same information in the
API's metadata is a string, `mixed-port` and `port` overlap, and several list
fields legitimately hold either a scalar or a list.

The obvious design — `#[derive(Deserialize)] struct Config { ... }` — fails on
all of that in a way that is actively harmful. The failure is silent: an
unknown key is dropped, and a round trip through the program *edits the user's
configuration*. A user's `tun` block, or a provider's new `sniffer` field, or
the order of their `rules`, disappears because a struct did not have a field
for it. That is not a bug the user can diagnose, because the program reports
success.

## Decision

`cvt_core::model::Config` is a newtype over an ordered JSON map, and it is the
representation that survives every stage:

- `serde_json` is used with the `preserve_order` feature, so key order is
  data rather than an accident of hashing.
- Unknown fields are carried through untouched. Nothing is discarded, so
  nothing can be silently lost.
- Typed accessors are layered on top — `Config::port()`, `Config::rules()`,
  `Config::proxy_groups()` — and they *read* the document. They do not define
  it.
- Every rule is parsed by a parser that round-trips byte-exactly, including
  the nested logical forms (`AND,((...),(...)),POLICY`) and the
  payload-less `MATCH`.
- Structural edits go through `enhance::path`, so a change is expressed as a
  path into the document and is all-or-nothing.

The property tests enforce the consequence rather than the implementation:
generate a document, run it through the whole pipeline, and assert it is
unchanged, ordering included.

## Consequences

- A configuration that this program does not understand still passes through
  it correctly. Unknown fields are preserved, not normalised.
- Editing is more verbose than field access, because a path expression has to
  name what it changes.
- Type confusion is resolved at generation time, not parse time, so a
  validation pass exists whose whole job is to catch the mistakes a typed
  model would have refused to parse. That pass reports every problem it finds
  rather than the first, with a stable diagnostic code per problem, which
  turns out to be a better user experience than a deserialisation error
  anyway.
- Round-tripping through YAML is not identity for *every* document: YAML merge
  keys (`<<`) are expanded, because that is what the format's specification
  says a consumer does with them. This is a documented limitation rather than
  a defect, and an independent review flagged it and then withdrew it for
  exactly this reason.

## Alternatives considered

**Typed structs with `#[serde(flatten)] extra: Map<String, Value>`.** Captures
unknown keys and is the usual advice. Rejected because it preserves unknown
keys *at the wrong level*: a key inside `proxy-groups[3]` lands in that
group's `extra`, order between known and unknown fields is not preserved, and
a nested unknown key inside an unknown subtree is handled inconsistently.

**`serde_yaml::Value` directly, with no wrapper type.** Closer, but
`Config` carries behaviour — typed accessors, statistics, a `minimal()`
constructor — and a bare `Value` would push helper functions into every
caller. The newtype is what keeps the invariants with the data.

**Convert to a typed model on load and back on save.** Would make every
access pleasant and every loss silent. This is precisely the failure mode the
decision exists to prevent, so it was not a close call.

**A maintained YAML library rather than a `serde_yaml` fork.** `serde_yaml`
is unmaintained; `serde_norway` is a maintained fork with the same API. This
is a dependency choice rather than an architectural one, but it is recorded
here because the reasoning is the same shape: pick the thing that will still
exist next year.
