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

- `Service` holds the resolved paths, the loaded settings, the profile store
  and the supervisor, and exposes the operations as async methods with
  outcomes rather than raw effects: `ApplyReport`, `ReloadOutcome`
  (`HotReloaded`, `Restarted { pid }`, `RolledBack { reason, snapshot }`),
  and diagnostics a user can read.
- The reload decision tree lives in exactly one method, and it reports *why*
  something failed rather than what the rollback bookkeeping did — a reload
  that cannot be undone reports the restart failure itself, and a machine with
  no core binary is not treated as a document worth rolling back.
- The CLI is parse, dispatch, and map an error onto an exit code. The
  interface's effect executor is the same thing with events instead of exit
  codes.
- The sequencing is exercised in tests through a small injection point, so the
  rollback path is covered without a real core.

## Consequences

- The two front ends cannot disagree about what an operation means, because
  neither of them implements an operation.
- The dangerous ordering is tested once, in one place, and its tests are unit
  tests rather than end-to-end tests.
- `Service` is a large surface, and it is the crate's centre of gravity. A
  change to how an operation is sequenced touches every front end at once,
  which is a feature here and would be a problem in a larger program.
- The interface cannot do something the library cannot express. That has
  forced at least one honest limitation into the open rather than into a
  workaround inside the UI.

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
