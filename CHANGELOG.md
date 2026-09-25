# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Workspace scaffold: `cvt-core`, `cvt-tui`, `cvt` binary.
- `cvt-core::paths` — XDG-compliant on-disk layout with `CVT_HOME`/`--home`
  overrides, atomic file writes, and detection of existing `clash-verge-rev`
  installations for import.
- `cvt-core::model` — lossless configuration document with typed accessors,
  plus proxy, group and rule models that preserve unknown fields.
- `cvt-core::model::rule` — rule parser that understands nested logical rules
  and payload-less `MATCH` rules.
- `cvt-core::validate` — whole-document pre-flight validation with stable
  diagnostic codes, including checks for dangling policies, dead rules behind a
  terminal rule, relay cycles and the fake-IP ULA trap.
- `cvt-core::enhance::path` — declarative path expressions for config edits,
  with atomic set/push semantics.
- `CONTRIBUTING.md` documenting the GitFlow workflow and architecture rules.

[Unreleased]: https://github.com/retort-hubbub/clash-verge-tui/commits/develop
