# 0005 — The interface is a pure state machine driven by an effect vocabulary

**Status:** accepted

## Context

A terminal interface is the hardest part of a program to test. The usual
structure — an event loop that reads a key and then goes and does the thing —
puts file access, network calls and process spawning in the middle of the
interaction logic. Testing any of it means standing up a terminal, and in
practice it means not testing it.

The behaviours that actually break are interaction behaviours, and they are
exactly the ones that need a test: is the selection still on the same profile
after a refresh reorders the list? Does typing `q` into a search box quit the
program or search for `q`? Does a destructive action ask for confirmation, and
does a no-op action *avoid* asking? Is the log view frozen without losing the
lines that arrive underneath it?

There is a second problem, specific to this program. The CLI and the interface
must agree about what an operation means. If the interface also owns the
sequencing of an apply — generate, then try a hot reload, then restart, then
roll back — that logic exists twice and will diverge.

## Decision

`App` is a state machine and performs no I/O. It does not read a file, open a
socket, spawn a process or print. Interaction is expressed as a fold:

```
App::on_event(Event) -> Vec<Effect>
App::on_tick()        -> Vec<Effect>
```

`Event` is what happened (`Key`, `Tick`, `Resize`, `Data`, `Done`, `Failed`).
`Effect` is what should happen as a result (~36 variants: `Refresh(screen)`,
`ApplyConfig { mode }`, `RunTest { kind, target }`, `ExportLogs { path, .. }`).
Rendering is the same idea from the other side: `ui::render(frame, &app)` and
one module per screen, each a pure function from application state into a
frame.

Everything I/O-shaped lives in the binary's effect executor, which turns an
`Effect` into a `cvt_core::Service` call and feeds the outcome back as
`Event::Data` / `Done` / `Failed`. Errors come back as events and surface in
the status line; they never unwind the loop.

## Consequences

- The interaction logic is testable without a terminal. That is what makes 67
  state-machine tests and 23 render tests — over a `TestBackend`, at several
  screen sizes — possible at all.
- The `q` problem is solved structurally rather than per-screen: the overlay
  stack takes key precedence, so an open prompt consumes input and `q` is a
  character.
- An operation cannot be sequenced differently in the two front ends, because
  the interface has no way to sequence anything — it can only ask.
- Adding a feature costs an `Effect` variant, a `Done` variant and one arm in
  the executor. This is real ceremony, and it is the price of the property.
- Exhaustiveness is load-bearing: the executor matches every `Effect` with no
  catch-all arm, so a new variant is a compile error rather than a feature
  that silently does nothing.

## Alternatives considered

**An event loop that performs the work inline.** Less code and immediately
legible. Rejected because the interaction logic becomes untestable, and the
behaviours that need testing most are the ones that need a terminal.

**A command/undo stack with `Box<dyn Command>`.** More idiomatic in some
traditions and would give undo for free. Rejected because the interesting
behaviours here are not undoable — you cannot un-send a node-selection request
— and dynamic dispatch would hide the exhaustiveness that the enum gives.

**A reducer over an immutable state with `Arc` snapshots.** Would make
time-travel debugging possible. Rejected as machinery without a consumer:
nothing in this program benefits from replaying history, and the cost is
either deep clones on every keystroke or pervasive `Arc` in the state.
