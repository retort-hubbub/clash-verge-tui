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

`App` state transitions perform no filesystem, network or process I/O.
The binary constructs state with `App::with_settings` and settings already
loaded by `Service`. The compatibility constructor `App::new` reads settings
from disk; it is not part of the pure transition boundary. Interaction is
expressed as a fold:

```
App::on_event(Event) -> Vec<Effect>
App::on_tick()        -> Vec<Effect>
```

`Event` is what happened (`Key`, `Tick`, `Resize`, `Data`, `Done`, `Failed`).
`Effect` is what should happen as a result (for example `Refresh(screen)`,
`ApplyConfig { mode }`, `RunTest { kind, target }`, `ExportLogs { path, .. }`).
Rendering is the same idea from the other side: `ui::render(frame, &app)` and
one module per screen, each a pure function from application state into a
frame.

The binary's effect executor performs application I/O and turns an `Effect`
into a `cvt_core::Service` call, then feeds the outcome back as `Event::Data` /
`Done` / `Failed`. The TUI `run` module owns terminal I/O and editor handoff. Errors come back as events and surface in
the status line; they never unwind the loop.

## Consequences

- Interaction logic can be checked without a terminal. Rendering checks use
  `TestBackend` at multiple screen sizes; neither replaces a real terminal run.
- The `q` problem is solved structurally rather than per-screen: the overlay
  stack takes key precedence, so an open prompt consumes input and `q` is a
  character.
- The interface requests operations through effects. Shared reload and
  rollback decisions belong to `Service`; adapters must preserve their order.
- Adding an I/O action costs an `Effect` variant, an executor arm and an
  appropriate result event. This is real ceremony, and it is the price of the property.
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
