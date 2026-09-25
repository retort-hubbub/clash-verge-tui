//! The run loop: the only part of this crate that touches a terminal.
//!
//! Everything else here turns state into a frame or a key into an effect. This
//! module is where the two meet, and it is split in two on purpose:
//!
//! * [`run`] takes the real terminal over — raw mode, the alternate screen, a
//!   panic hook that gives both back — and does not return without restoring
//!   it, whatever happened;
//! * [`Session`] is the loop itself, drawing on a generic
//!   [`ratatui::backend::Backend`] and reading a stream of [`Event`]s, so the
//!   tests below drive it with a `TestBackend`, a scripted input and a fake
//!   effect executor: no terminal, no core, no clock.
//!
//! The effect executor is *injected*. Performing an effect means talking to the
//! world, and the crate above this one already does that for every subcommand;
//! what the loop owns is the terminal, so the only effects it answers itself
//! are the ones that need it — [`Effect::Quit`], which means "stop", and the
//! two that hand the screen to `$EDITOR` and take it back.

use std::io::{self, IsTerminal as _};
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{EventStream, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, Stream};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use tokio::sync::mpsc;

use crate::action::Action;
use crate::app::{App, Effect, Event};
use crate::theme::Theme;
use crate::ui;

/// What an effect executor returns.
///
/// The future reports through the [`EventSink`] it was handed and resolves when
/// the work is over; dropping it without resolving is how a cancelled test
/// batch stops.
pub type EffectFuture = BoxFuture<'static, ()>;

/// How many events may wait to be handed to the state machine.
///
/// The queue is the interface's back pressure: a live stream that outruns the
/// loop fills it and then loses events, exactly as `cvt_core::mihomo::stream`
/// drops events for a consumer that cannot keep up. An unbounded queue would
/// turn "the loop is busy" into unbounded memory.
const REPORT_QUEUE: usize = 1024;

/// How many reports are folded into one frame before it is drawn.
///
/// A burst of log lines should cost one frame, not one frame per line; a bound
/// rather than a full drain keeps a fast stream from starving the keyboard.
const MAX_COALESCED: usize = 256;

/// The slowest the loop will draw when nothing else is happening.
///
/// `Settings::ui` accepts intervals down to a millisecond, which is a request
/// to spin; a frame is what a terminal can show, and 60 of them a second is
/// more than any of this needs. Anything slower is honoured as written.
const FASTEST_FRAME: Duration = Duration::from_millis(16);

/// Why the interface did not run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// There is no terminal on stdout, so there is nothing to draw on.
    ///
    /// Reported rather than worked around: an interface that silently wrote
    /// escape sequences into a pipe would look like success.
    #[error("the interactive interface needs a terminal, but stdout is not one")]
    NotATerminal,
    /// The terminal would not go into raw mode, would not enter the alternate
    /// screen, or would not draw.
    #[error("could not use the terminal: {0}")]
    Terminal(#[source] io::Error),
}

/// Where an effect reports back.
///
/// Cloning it is how a live stream — logs, traffic, memory, connections —
/// keeps reporting after the effect that started it has returned. [`EventSink::send`]
/// is deliberately not `async`: an effect must never wait on the interface.
#[derive(Debug, Clone)]
pub struct EventSink {
    reports: mpsc::Sender<Event>,
}

impl EventSink {
    /// Report one event.
    ///
    /// `false` means the interface is behind and the event was dropped, which
    /// is only possible while the loop is busy with something long — reading
    /// `$EDITOR`, above all. Nothing retries: a log line from ten seconds ago
    /// is not worth a queue that grows without limit.
    pub fn send(&self, event: Event) -> bool {
        self.reports.try_send(event).is_ok()
    }
}

/// The panic hook that was installed before this module replaced it.
///
/// Named because the type is long enough to hide what the field means, and it
/// appears on both sides of [`install_panic_hook`].
type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// The two states a terminal can be in.
///
/// The loop needs this for `$EDITOR` and for nothing else, and the indirection
/// is what lets the loop be tested against a backend that has no terminal at
/// all.
trait TerminalModes {
    /// Give the terminal back to the user.
    fn suspend(&mut self);
    /// Take it over again.
    fn resume(&mut self);
}

/// The real terminal, and the promise that it is given back.
///
/// Every way out of the loop passes through `Drop`: a normal quit, an error and
/// an unwind all restore the terminal. The one that does not unwind — a release
/// build is compiled with `panic = "abort"` — is covered by the panic hook
/// installed here, which restores first and prints the panic second.
struct TerminalScope {
    /// Whether raw mode and the alternate screen are on right now.
    active: bool,
    /// The hook that was installed before this one, put back on the way out.
    previous_hook: Option<PanicHook>,
}

impl std::fmt::Debug for TerminalScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalScope")
            .field("active", &self.active)
            .field("hook_installed", &self.previous_hook.is_some())
            .finish()
    }
}

impl TerminalScope {
    /// Take the terminal over.
    fn enter() -> Result<Self, RunError> {
        enable_raw_mode().map_err(RunError::Terminal)?;
        // Raw mode is already on, so from here every early return has to undo
        // it: the scope is marked active before the next fallible step and its
        // `Drop` does the rest.
        let mut scope = Self {
            active: true,
            previous_hook: None,
        };
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            scope.suspend();
            return Err(RunError::Terminal(error));
        }
        scope.previous_hook = Some(install_panic_hook());
        Ok(scope)
    }
}

impl TerminalModes for TerminalScope {
    fn suspend(&mut self) {
        if self.active {
            self.active = false;
            give_the_terminal_back();
        }
    }

    fn resume(&mut self) {
        if self.active {
            return;
        }
        if enable_raw_mode().is_err() {
            return;
        }
        if execute!(io::stdout(), EnterAlternateScreen).is_err() {
            // Half-taken: raw mode without the alternate screen is worse than
            // no interface at all, so it is undone rather than kept.
            let _ = disable_raw_mode();
            return;
        }
        self.active = true;
    }
}

impl Drop for TerminalScope {
    fn drop(&mut self) {
        self.suspend();
        if let Some(previous) = self.previous_hook.take() {
            std::panic::set_hook(previous);
        }
    }
}

/// Put the terminal back the way it was found.
///
/// Written so that it cannot panic, because it is called from a panic hook and
/// from `Drop` during an unwind; a panic in either would take the process down
/// with the terminal still in raw mode. Every call is best effort: there is no
/// useful answer to "the restore failed" at this point.
fn give_the_terminal_back() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Install a hook that restores the terminal before the panic is printed.
///
/// The hook is re-entrancy-guarded twice over. A panic raised *inside* a panic
/// hook aborts the process, so the restore runs at most once, and it is wrapped
/// in `catch_unwind` so that even a panic in the terminal calls themselves —
/// which would otherwise abort — cannot skip it. The previous hook is called
/// afterwards, so the panic still prints exactly once, where it always did.
fn install_panic_hook() -> PanicHook {
    let previous = std::panic::take_hook();
    // Two owners of the previous hook are needed: the closure that chains to
    // it, and the caller that puts it back on the way out. `Box<dyn Fn>` is
    // not `Clone`, so it is shared behind an `Arc` and a second box over the
    // same allocation is handed back.
    let shared: Arc<dyn Fn(&PanicHookInfo<'_>) + Sync + Send> = Arc::from(previous);
    let chained = Arc::clone(&shared);
    let already_restoring = Arc::new(AtomicBool::new(false));
    std::panic::set_hook(Box::new(move |info| {
        if !already_restoring.swap(true, Ordering::SeqCst) {
            let _ = std::panic::catch_unwind(give_the_terminal_back);
        }
        chained(info);
    }));
    Box::new(move |info| shared(info))
}

/// One wake-up of the loop.
#[derive(Debug, Clone, PartialEq)]
enum Wake {
    /// The terminal delivered an event, or stopped delivering them.
    Input(Option<Event>),
    /// The frame clock fired.
    Frame,
    /// An effect finished; what it reported is in the queue.
    Effect,
}

/// The loop, with the terminal and the clock reduced to what it actually uses.
struct Session<'a, B, E>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Where frames go: a real terminal, or a `TestBackend`.
    terminal: &'a mut Terminal<B>,
    /// How the terminal is handed over and taken back.
    modes: &'a mut dyn TerminalModes,
    /// The state machine.
    app: App,
    /// What performs the effects.
    executor: E,
    /// Terminal input, already translated into [`Event`]s.
    input: Pin<Box<dyn Stream<Item = Event> + Send>>,
    /// Where effects report back.
    sink: EventSink,
    /// What they have reported.
    reports: mpsc::Receiver<Event>,
    /// Effects that are still running.
    running: FuturesUnordered<EffectFuture>,
    /// Whether the input stream has ended.
    input_ended: bool,
}

impl<B, E> Session<'_, B, E>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    E: Fn(Effect, EventSink) -> EffectFuture,
{
    /// Run until the machine quits, the input ends, or drawing fails.
    ///
    /// The loop never awaits an effect: every one of them joins `running` and
    /// the `select` below polls them alongside the terminal, so a request that
    /// takes ten seconds cannot stop a key from being answered or a frame from
    /// being drawn.
    async fn run(&mut self) -> Result<(), RunError> {
        self.prime().await?;
        self.draw()?;
        while !self.app.is_quit() {
            let frame = Duration::from_millis(self.app.settings.ui.refresh_ms).max(FASTEST_FRAME);
            let wake = tokio::select! {
                event = self.input.next(), if !self.input_ended => Wake::Input(event),
                event = self.reports.recv() => Wake::Input(event),
                Some(()) = self.running.next(), if !self.running.is_empty() => Wake::Effect,
                () = tokio::time::sleep(frame) => Wake::Frame,
            };
            match wake {
                Wake::Input(Some(event)) => self.deliver(event).await?,
                // The terminal stopped delivering input. That is not a reason
                // to stop drawing: an effect that is still running may have
                // something to say, and a report that has already arrived has
                // not been shown yet. The loop lasts until both are drained.
                Wake::Input(None) => self.input_ended = true,
                Wake::Frame => self.deliver(Event::Tick).await?,
                // Its report is already in the queue and `coalesce` reads it.
                Wake::Effect => {}
            }
            self.coalesce().await?;
            self.draw()?;
            if self.input_ended && self.running.is_empty() && self.reports.is_empty() {
                break;
            }
        }
        Ok(())
    }

    /// Show the machine what a start looks like.
    ///
    /// `App` has no start event, and its first screen is drawn from an empty
    /// state until something asks for data. So the loop does what a user does:
    /// it tells the machine how big the terminal is, and presses the key the
    /// interface itself binds to "refresh". The key comes from the key map
    /// rather than from a literal, so a rebinding cannot leave the first frame
    /// permanently empty.
    async fn prime(&mut self) -> Result<(), RunError> {
        let size = self
            .terminal
            .size()
            .map_err(|e| RunError::Terminal(io::Error::other(e)))?;
        self.deliver(Event::Resize(size.width, size.height)).await?;
        if let Some(key) = refresh_key(&self.app) {
            self.deliver(Event::Key(key)).await?;
        }
        Ok(())
    }

    /// Hand one event to the state machine and start what it asks for.
    async fn deliver(&mut self, event: Event) -> Result<(), RunError> {
        let effects = self.app.on_event(event);
        self.perform(effects).await
    }

    /// Start every effect the machine asked for.
    ///
    /// The two editor effects are the exception to the no-await rule, and
    /// deliberately so: `$EDITOR` needs the real screen, so the loop gives it
    /// back, waits for the editor to exit, and takes the screen over again.
    /// Holding the loop for the duration of an editor session is what a user
    /// expects from a program that opened one.
    async fn perform(&mut self, effects: Vec<Effect>) -> Result<(), RunError> {
        for effect in effects {
            match effect {
                // The loop stops on `App::is_quit`, which the machine sets in
                // the same step as this effect; there is nothing to perform.
                Effect::Quit => {}
                Effect::OpenEditor { .. } | Effect::EditProfile { .. } => {
                    self.modes.suspend();
                    (self.executor)(effect, self.sink.clone()).await;
                    self.modes.resume();
                    // The editor wrote all over the screen: ratatui's buffers
                    // no longer describe what is on it, so the next frame has
                    // to repaint every cell.
                    self.terminal
                        .clear()
                        .map_err(|e| RunError::Terminal(io::Error::other(e)))?;
                }
                other => self.running.push((self.executor)(other, self.sink.clone())),
            }
        }
        Ok(())
    }

    /// Take what has already arrived, so a burst costs one frame.
    async fn coalesce(&mut self) -> Result<(), RunError> {
        for _ in 0..MAX_COALESCED {
            let Ok(event) = self.reports.try_recv() else {
                break;
            };
            self.deliver(event).await?;
        }
        Ok(())
    }

    /// Draw one frame.
    fn draw(&mut self) -> Result<(), RunError> {
        let app = &self.app;
        self.terminal
            .draw(|frame| ui::render(frame, app))
            .map_err(|e| RunError::Terminal(io::Error::other(e)))?;
        Ok(())
    }

    /// Give the state machine back once the loop is over.
    ///
    /// Consuming the session is also what releases its borrows on the terminal
    /// and the terminal modes, so a test can look at both afterwards.
    #[cfg(test)]
    fn into_app(self) -> App {
        self.app
    }
}

/// The key that means "read what this screen shows", from the key map.
fn refresh_key(app: &App) -> Option<KeyEvent> {
    app.keymap
        .bindings()
        .iter()
        .find(|binding| binding.action == Action::Refresh)
        .map(|binding| KeyEvent::new(binding.key, binding.mods))
}

/// Terminal input, translated into the interface's own vocabulary.
///
/// The frame clock is not here: it is a deadline the loop resets after every
/// wake-up, so an interval that arrives while the user is typing cannot queue
/// up frames behind them.
fn terminal_input() -> impl Stream<Item = Event> {
    EventStream::new().filter_map(|event| async move {
        match event {
            Ok(crossterm::event::Event::Key(key)) => Some(Event::Key(key)),
            Ok(crossterm::event::Event::Resize(width, height)) => {
                Some(Event::Resize(width, height))
            }
            // Mouse, focus and paste events have no meaning for this
            // interface; dropping them here keeps `Event` at six variants.
            _ => None,
        }
    })
}

/// Run the interactive interface.
///
/// Takes over the terminal, drives [`App`] against `executor` until the user
/// quits, and gives the terminal back on every exit path — including a panic.
///
/// The executor is called for every effect that is not [`Effect::Quit`], and
/// reports by pushing an [`Event`] into the [`EventSink`] it is handed. It may
/// keep the sink for as long as it likes: that is how a log or traffic stream
/// keeps feeding the interface after the effect that opened it has returned.
///
/// # Errors
/// [`RunError::NotATerminal`] when stdout is not a terminal, and
/// [`RunError::Terminal`] when the terminal cannot be taken over or drawn on.
pub async fn run<E>(home: PathBuf, theme: Theme, executor: E) -> Result<(), RunError>
where
    E: Fn(Effect, EventSink) -> EffectFuture + Send + Sync + 'static,
{
    // Checked before anything is attempted: entering raw mode on a pipe would
    // write escape sequences into whatever is reading it.
    if !io::stdout().is_terminal() {
        return Err(RunError::NotATerminal);
    }
    let app = App::new(home, theme);
    let (reports, inbox) = mpsc::channel(REPORT_QUEUE);
    let mut scope = TerminalScope::enter()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(RunError::Terminal)?;
    let mut session = Session {
        terminal: &mut terminal,
        modes: &mut scope,
        app,
        executor,
        input: Box::pin(terminal_input()),
        sink: EventSink { reports },
        reports: inbox,
        running: FuturesUnordered::new(),
        input_ended: false,
    };
    // `session` is dropped before `scope`, so the terminal is restored after
    // the last frame has been drawn and not before.
    session.run().await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crossterm::event::{KeyCode, KeyModifiers};
    use futures_util::stream;
    use ratatui::backend::TestBackend;

    use crate::app::{App, Data, Done, StatusKind};
    use crate::row::ProfileRow;
    use crate::theme::Theme;

    /// An executor that records what the loop asked for and answers from a
    /// script.
    #[derive(Debug, Default)]
    struct Fake {
        asked: Mutex<Vec<Effect>>,
        answers: Mutex<VecDeque<Event>>,
    }

    impl Fake {
        /// The closure the loop is run with.
        fn executor(self: &Arc<Self>) -> impl Fn(Effect, EventSink) -> EffectFuture + Send + Sync {
            let fake = Arc::clone(self);
            move |effect, sink| {
                fake.asked.lock().unwrap().push(effect);
                let answer = fake.answers.lock().unwrap().pop_front();
                Box::pin(async move {
                    if let Some(event) = answer {
                        sink.send(event);
                    }
                })
            }
        }

        /// Answer the next effect with `event`.
        fn answer(&self, event: Event) {
            self.answers.lock().unwrap().push_back(event);
        }

        /// Everything the loop asked for, in order.
        fn asked(&self) -> Vec<Effect> {
            self.asked.lock().unwrap().clone()
        }
    }

    /// An executor that never resolves, to prove the loop does not wait.
    fn never() -> impl Fn(Effect, EventSink) -> EffectFuture + Send + Sync {
        |_effect, _sink| Box::pin(std::future::pending())
    }

    /// The terminal modes, recorded rather than performed.
    #[derive(Debug, Default)]
    struct Modes {
        log: Vec<String>,
    }

    impl TerminalModes for Modes {
        fn suspend(&mut self) {
            self.log.push("suspend".to_owned());
        }

        fn resume(&mut self) {
            self.log.push("resume".to_owned());
        }
    }

    /// Drive one session with a scripted input and return its app.
    ///
    /// The terminal is a `TestBackend`, the input is a fixed list of events and
    /// the clock only fires as `Event::Tick` in that list, so nothing here
    /// sleeps or waits.
    async fn drive<E>(
        app: App,
        input: Vec<Event>,
        executor: E,
        modes: &mut Modes,
    ) -> Result<(App, Terminal<TestBackend>), RunError>
    where
        E: Fn(Effect, EventSink) -> EffectFuture,
    {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let (reports, inbox) = mpsc::channel(REPORT_QUEUE);
        let mut session = Session {
            terminal: &mut terminal,
            modes,
            app,
            executor,
            input: Box::pin(stream::iter(input)),
            sink: EventSink { reports },
            reports: inbox,
            running: FuturesUnordered::new(),
            input_ended: false,
        };
        session.run().await?;
        // Consuming the session releases its borrows, which is what makes the
        // terminal readable and the app returnable from here.
        let app = session.into_app();
        Ok((app, terminal))
    }

    fn app() -> App {
        App::new(PathBuf::from("/tmp/cvt-run-test"), Theme::default())
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn profile(uid: &str, name: &str) -> ProfileRow {
        ProfileRow {
            uid: uid.to_owned(),
            name: name.to_owned(),
            kind: cvt_core::profile::item::ProfileType::Local,
            current: true,
            in_chain: false,
            updated: None,
            url: None,
            quota: cvt_core::profile::item::UserInfo::default(),
            unsupported: None,
            edits: None,
        }
    }

    #[tokio::test]
    async fn the_first_frame_loads_the_screen_the_interface_starts_on() {
        let fake = Arc::new(Fake::default());
        let mut modes = Modes::default();
        let input = vec![key(KeyCode::Char('q'))];
        let (app, _terminal) = drive(app(), input, fake.executor(), &mut modes)
            .await
            .unwrap();
        assert!(app.is_quit(), "`q` has to end the loop");
        assert!(
            fake.asked()
                .contains(&Effect::Refresh(crate::action::Screen::Home)),
            "the dashboard must be loaded before the user asks: {:?}",
            fake.asked()
        );
    }

    #[tokio::test]
    async fn an_effect_that_never_finishes_does_not_stop_the_keyboard() {
        let mut modes = Modes::default();
        // The first event asks for an effect that never resolves; `q` must
        // still end the loop.
        let input = vec![
            Event::Data(Data::Notice("busy".to_owned())),
            key(KeyCode::Char('q')),
        ];
        let (app, _terminal) = drive(app(), input, never(), &mut modes).await.unwrap();
        assert!(app.is_quit());
    }

    #[tokio::test]
    async fn a_failure_reaches_the_status_line_instead_of_the_loop() {
        let fake = Arc::new(Fake::default());
        // Answer the primed refresh with a failure.
        fake.answer(Event::Failed("the controller is unreachable".to_owned()));
        let mut modes = Modes::default();
        let (app, _terminal) = drive(app(), Vec::new(), fake.executor(), &mut modes)
            .await
            .unwrap();
        let status = app.current_status().unwrap();
        assert_eq!(status.kind, StatusKind::Error);
        assert!(status.text.contains("unreachable"), "{}", status.text);
    }

    #[tokio::test]
    async fn reports_are_fed_back_in_order() {
        let fake = Arc::new(Fake::default());
        fake.answer(Event::Data(Data::Profiles(vec![profile("L1", "office")])));
        fake.answer(Event::Done(Done::ProfilesLoaded));
        let mut modes = Modes::default();
        let (app, _terminal) = drive(app(), Vec::new(), fake.executor(), &mut modes)
            .await
            .unwrap();
        assert_eq!(app.profiles.total(), 1);
        assert_eq!(app.profiles.items()[0].name, "office");
    }

    #[tokio::test]
    async fn the_editor_gets_the_terminal_and_gives_it_back() {
        let fake = Arc::new(Fake::default());
        // `e` on the profiles screen opens the highlighted profile.
        let input = vec![
            Event::Data(Data::Profiles(vec![profile("L1", "office")])),
            key(KeyCode::Char('2')),
            key(KeyCode::Char('e')),
        ];
        let mut modes = Modes::default();
        let (app, _terminal) = drive(app(), input, fake.executor(), &mut modes)
            .await
            .unwrap();
        assert!(
            fake.asked()
                .iter()
                .any(|effect| matches!(effect, Effect::EditProfile { uid } if uid == "L1")),
            "{:?}",
            fake.asked()
        );
        assert_eq!(modes.log, vec!["suspend", "resume"]);
        assert!(!app.is_quit());
    }

    #[tokio::test]
    async fn a_resize_redraws_and_the_clock_ticks() {
        let fake = Arc::new(Fake::default());
        let mut modes = Modes::default();
        let input = vec![Event::Resize(120, 40), Event::Tick, key(KeyCode::Char('q'))];
        let (app, terminal) = drive(app(), input, fake.executor(), &mut modes)
            .await
            .unwrap();
        assert_eq!(app.viewport, (120, 40));
        // Something was drawn: the tab bar is on the first row.
        let buffer = terminal.backend().buffer();
        let first_row: String = (0..buffer.area().width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect();
        assert!(first_row.contains("Home"), "{first_row:?}");
    }

    #[tokio::test]
    async fn the_refresh_key_comes_from_the_key_map() {
        let app = app();
        let key = refresh_key(&app).unwrap();
        let mut app = app;
        let effects = app.on_event(Event::Key(key));
        assert_eq!(effects, vec![Effect::Refresh(crate::action::Screen::Home)]);
    }
}
