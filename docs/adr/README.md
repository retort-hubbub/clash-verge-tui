# Architecture Decision Records

Each record documents one decision that a maintainer would otherwise have to
reverse-engineer from the code, together with the alternatives that were
rejected and what they would have cost. Records are numbered. Correct stale
facts in place; supersede a changed decision with a new record so the original
reason remains visible.

| # | Decision |
|---|----------|
| [0001](0001-rust-three-crate-workspace.md) | Rust, as a three-crate workspace with a one-way dependency |
| [0002](0002-lossless-config-document.md) | Keep configuration as a lossless ordered document, not a typed struct |
| [0003](0003-mihomo-only.md) | Support the mihomo core only |
| [0004](0004-hand-verified-api-contract.md) | API contracts based on observed Mihomo responses |
| [0005](0005-pure-state-machine-with-effects.md) | The interface is a pure state machine driven by an effect vocabulary |
| [0006](0006-one-service-facade.md) | One `Service` facade behind both front ends |
| [0007](0007-gitflow-and-conventional-commits.md) | GitFlow with Conventional Commits |
| [0008](0008-control-plane-ownership.md) | Control-plane precedence across settings and profiles |
| [0009](0009-adversarial-review.md) | Adversarial review, and keeping its counterexamples |

## Adding a record

Copy the shape of an existing file. The sections are `Status`, `Context`,
`Decision`, `Consequences` and `Alternatives considered`, and the last one is
not optional — a decision recorded without the option it rejected is a
description, not a decision.
