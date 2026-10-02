# 0006 — One `Service` facade behind both front ends

**Status:** accepted

## Context

The operations a user asks for are not single calls. "Apply this profile"
means: resolve the profile chain, merge the documents, apply the overrides,
write the resulting configuration, validate it, and *then* decide how to make
the running core adopt it. That last decision is the dangerous one, and it has
an order:

1. hand the document to the core over the API, which applies most changes
   without dropping connections;
2. only if that fails, restart the process;
3. if the core then fails to come up, restore the previous snapshot and start
   again — because "I edited my rules and now my network is gone" is the worst
   outcome this program can produce, and the snapshot is already on disk.

Getting that order wrong is expensive and easy. It is also exactly the kind of
logic that gets written once in the interface and once in the command line,
diverges, and is then debugged twice.

## Decision

`cvt_core::Service` owns the operations, and both front ends are thin shells
over it.

- `Service` holds resolved paths and loaded settings, constructs the profile
  store and supervisor as needed, and exposes sync and async operations with
  outcomes rather than raw effects: `ApplyReport`, `ReloadOutcome`
  (`HotReloaded`, `Restarted { pid }`, `RolledBack { reason, snapshot }`),
  and diagnostics a user can read.
- Service deployment methods own reload and recovery policy. Errors include
  the deployment failure and any recovery failure.
- The CLI is parse, dispatch, and map an error onto an exit code. The
  interface's effect executor is the same thing with events instead of exit
  codes.
- The sequencing is exercised with fake controllers and disposable application
  homes; live-core integration coverage complements those checks.

## Consequences

- Shared apply and reload decisions belong in the facade. Front-end adapters
  still own event sequencing and must not duplicate the reload policy.
- Shared ordering is covered at the service boundary; front-end and live-core
  checks cover integration behavior.
- Changes to shared operation sequencing affect both front ends and require
  service-boundary regression coverage.
- New shared operations require a library API. Terminal-specific interaction
  remains in the TUI and executor.

## Alternatives considered

**Put the sequencing in the binary and keep the library primitive.** Makes the
library smaller and more obviously reusable. Rejected because the interface
needs the same sequencing, and the alternative is either duplicating it or
having the interface call the command line.

**Have the interface call the CLI as a subprocess.** Superficially removes the
duplication. Rejected outright: it would make the interface's behaviour depend
on the binary's argument parsing, cost a process per action, and lose the
typed outcomes that the status line displays.

**An operation trait per action, with the front ends composing them.** More
composable, and it is where a larger codebase would end up. Rejected as
premature here: it trades one readable sequence of steps for an indirection
that has to be assembled mentally, in a program whose most dangerous code is
that sequence of steps.
