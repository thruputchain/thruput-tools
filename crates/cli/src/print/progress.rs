//! Command-scoped inline output. The build and its children keep normal terminal
//! input and signal behaviour; only this worker writes while its viewport is active.

use std::{
    io::{self, IsTerminal, Read, Write},
    panic::PanicHookInfo,
    sync::{Arc, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::{Backend, CrosstermBackend},
    layout::{Position, Rect, Size},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use super::{
    Print,
    policy::Policy,
    theme::{Outcome, Theme},
};
use crate::terminal::terminal_safe;
mod backend;
use backend::InlineBackend;

const DELAY: Duration = Duration::from_millis(150);
const TICK: Duration = Duration::from_millis(100);
const CHUNK: usize = 8192;
static OWNER: Mutex<()> = Mutex::new(());
type PanicHook = Arc<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

#[cfg(unix)]
struct ReflowSignals(Vec<signal_hook::SigId>);

#[cfg(unix)]
impl Drop for ReflowSignals {
    fn drop(&mut self) {
        for signal in self.0.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn write(self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Stdout => io::stdout().lock().write_all(bytes),
            Self::Stderr => io::stderr().lock().write_all(bytes),
        }
    }
}

#[derive(Debug)]
enum Event {
    Phase(String),
    Data(Stream, Vec<u8>),
    Line(Stream, String),
    Finish(mpsc::Sender<()>),
}

#[derive(Debug)]
pub(super) struct TaskOutput {
    sender: Option<mpsc::SyncSender<Event>>,
    warnings: Mutex<Vec<String>>,
}

impl TaskOutput {
    pub fn live(&self) -> bool {
        self.sender.is_some()
    }

    pub fn phase(&self, message: &str) -> bool {
        self.sender
            .as_ref()
            .is_some_and(|sender| sender.send(Event::Phase(terminal_safe(message))).is_ok())
    }

    pub fn line(&self, stream: Stream, message: &str) -> bool {
        self.sender.as_ref().is_some_and(|sender| {
            sender
                .send(Event::Line(stream, terminal_safe(message)))
                .is_ok()
        })
    }

    pub fn data(&self, stream: Stream, bytes: &[u8]) -> io::Result<()> {
        if let Some(sender) = &self.sender {
            for chunk in bytes.chunks(CHUNK) {
                sender
                    .send(Event::Data(stream, chunk.to_vec()))
                    .map_err(|_| io::Error::other("inline output worker stopped"))?;
            }
            Ok(())
        } else {
            stream.write(bytes)
        }
    }

    pub fn warn(&self, message: String) {
        self.warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message);
    }

    fn take_warnings(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .warnings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// Owns one command's presentation; finishing joins the renderer before results print.
pub(crate) struct Task {
    print: Print,
    worker: Option<JoinHandle<()>>,
    previous_hook: Option<PanicHook>,
    _owner: Option<MutexGuard<'static, ()>>,
}

impl Task {
    pub fn start(print: &Print, phase: &str, animate: bool) -> Self {
        let started = Instant::now();
        let mut scoped = print.clone();
        let policy = print.output_policy();
        scoped.compact_build = print.compact_build && eligible(print, policy);
        let owner = if animate && eligible(print, policy) {
            OWNER.try_lock().ok()
        } else {
            None
        };
        let mut worker = None;
        let mut sender = None;
        if owner.is_some() {
            let (tx, rx) = mpsc::sync_channel(64);
            let phase = terminal_safe(phase);
            if let Ok(handle) = thread::Builder::new()
                .name("groundhog-output".into())
                .spawn(move || Worker::new(policy, phase, started).run(&rx))
            {
                worker = Some(handle);
                sender = Some(tx);
            }
        }
        let previous_hook = worker
            .as_ref()
            .zip(sender.as_ref())
            .map(|(worker, sender)| {
                let previous: PanicHook = Arc::from(std::panic::take_hook());
                let hook = previous.clone();
                let sender = sender.clone();
                let worker_id = worker.thread().id();
                std::panic::set_hook(Box::new(move |info| {
                    if thread::current().id() != worker_id {
                        stop_sender(&sender);
                    }
                    hook(info);
                }));
                previous
            });
        if !print.json {
            scoped.task = Some(Arc::new(TaskOutput {
                sender,
                warnings: Mutex::new(Vec::new()),
            }));
        }
        Self {
            print: scoped,
            worker,
            previous_hook,
            _owner: owner,
        }
    }

    pub fn print(&self) -> &Print {
        &self.print
    }

    pub fn finish(mut self) -> Vec<String> {
        self.stop();
        self.print
            .task
            .as_ref()
            .map_or_else(Vec::new, |output| output.take_warnings())
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            if let Some(sender) = self
                .print
                .task
                .as_ref()
                .and_then(|output| output.sender.as_ref())
            {
                stop_sender(sender);
            }
            let _ = worker.join();
        }
        if !thread::panicking()
            && let Some(previous) = self.previous_hook.take()
        {
            std::panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.stop();
        if let Some(output) = self.print.task.take() {
            for warning in output.take_warnings() {
                let mut report = super::report::Report::new(self.print.output_policy());
                report.message(Outcome::Warning, &warning, None, None);
                for line in report.into_lines() {
                    let _ = writeln!(io::stderr(), "{line}");
                }
            }
        }
    }
}

fn stop_sender(sender: &mpsc::SyncSender<Event>) {
    let (done, wait) = mpsc::channel();
    if sender.send(Event::Finish(done)).is_ok() {
        let _ = wait.recv();
    }
}

pub(super) fn eligible(print: &Print, policy: Policy) -> bool {
    !print.json
        && !print.quiet
        && !print.no_ui
        && print.task.is_none()
        && policy.width.is_some_and(|width| width >= 24)
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
        && same_terminal()
}

#[cfg(unix)]
fn same_terminal() -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: these queries only inspect the controlling terminal/process group.
    // Cursor-position queries must not stop a background job with SIGTTIN/SIGTTOU.
    if unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) != libc::getpgrp() } {
        return false;
    }
    let devices: io::Result<Vec<_>> = (0..3)
        .map(|fd| std::fs::metadata(format!("/dev/fd/{fd}")).map(|stat| stat.rdev()))
        .collect();
    devices.is_ok_and(|devices| devices.windows(2).all(|pair| pair[0] == pair[1]))
}

#[cfg(not(unix))]
fn same_terminal() -> bool {
    false
}

/// Capture still belongs to the existing caller; this helper only routes display bytes.
pub(crate) fn relay(mut reader: impl Read, print: &Print, stream: Stream) -> io::Result<()> {
    let mut bytes = [0; CHUNK];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        print.relay_output(stream, &bytes[..count])?;
    }
}

struct Inline {
    terminal: Terminal<InlineBackend>,
    size: Size,
    valid: bool,
}

impl Inline {
    fn open(invalidated: Arc<std::sync::atomic::AtomicBool>) -> io::Result<Self> {
        let inner = CrosstermBackend::new(io::stderr());
        let size = inner.size()?;
        if size.width < 24 || size.height < 3 {
            return Err(io::Error::other(
                "terminal is too small for inline progress",
            ));
        }
        let terminal = Terminal::with_options(
            InlineBackend {
                inner,
                size,
                invalidated,
            },
            TerminalOptions {
                viewport: Viewport::Inline(1),
            },
        )?;
        Ok(Self {
            terminal,
            size,
            valid: true,
        })
    }

    fn current_size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    fn draw(&mut self, phase: &str, elapsed: Duration, policy: Policy) -> io::Result<()> {
        self.terminal.draw(|frame| {
            let area = frame.area();
            let content = Rect {
                width: area.width.saturating_sub(1),
                ..area
            };
            let symbol = if policy.unicode {
                ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                    [usize::try_from(elapsed.as_millis() / 100 % 10).unwrap_or(0)]
            } else {
                ["|", "/", "-", "\\"][usize::try_from(elapsed.as_millis() / 100 % 4).unwrap_or(0)]
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{symbol} "),
                    Theme(policy).outcome(Outcome::Information),
                ),
                Span::styled(
                    format!("{}s  {phase}", elapsed.as_secs()),
                    Theme(policy).routine_log(),
                ),
            ]);
            let cursor = u16::try_from(line.width())
                .unwrap_or(u16::MAX)
                .min(content.width);
            frame.render_widget(Paragraph::new(line), content);
            // No hidden cursor, raw mode, or alternate screen: ordinary signals keep
            // their original semantics, even if the process is killed between frames.
            frame.set_cursor_position(Position::new(area.x + cursor, area.y));
        })?;
        Ok(())
    }

    fn lines(&mut self, lines: &[String], policy: Policy) -> io::Result<()> {
        self.current_size()?;
        let paragraph = Paragraph::new(
            lines
                .iter()
                .map(|line| super::log::line(line, Theme(policy)))
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false });
        let height =
            u16::try_from(paragraph.line_count(self.size.width).max(1)).unwrap_or(u16::MAX);
        self.terminal
            .insert_before(height, |buffer| paragraph.render(buffer.area, buffer))?;
        self.terminal.backend_mut().flush()
    }

    fn finish(&mut self) {
        if self.valid
            && !thread::panicking()
            && self.current_size().is_ok_and(|size| size == self.size)
        {
            let _ = self.terminal.draw(|frame| {
                frame.set_cursor_position(Position::new(0, frame.area().y));
            });
        } else {
            // A resize can reflow the old row. Never guess its new coordinates.
            let _ = writeln!(io::stderr());
        }
        let _ = self.terminal.show_cursor();
    }
}

impl Drop for Inline {
    fn drop(&mut self) {
        self.finish();
    }
}

struct Worker {
    policy: Policy,
    phase: String,
    started: Instant,
    terminal: Option<Inline>,
    attempted: bool,
    last_draw: Instant,
    stdout: LogBuffer,
    stderr: LogBuffer,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
}

impl Worker {
    fn new(policy: Policy, phase: String, started: Instant) -> Self {
        Self {
            policy,
            phase,
            started,
            terminal: None,
            attempted: false,
            last_draw: Instant::now(),
            stdout: LogBuffer::default(),
            stderr: LogBuffer::default(),
            invalidated: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn run(mut self, events: &mpsc::Receiver<Event>) {
        #[cfg(unix)]
        let signals: Vec<_> = [libc::SIGCONT, libc::SIGWINCH]
            .into_iter()
            .filter_map(|signal| signal_hook::flag::register(signal, self.invalidated.clone()).ok())
            .collect();
        #[cfg(unix)]
        let _signals = {
            if signals.len() != 2 {
                self.attempted = true;
            }
            ReflowSignals(signals)
        };
        loop {
            let event = events.recv_timeout(TICK);
            // Check before draw/insert: Ratatui 0.30's horizontal shrink path clears
            // the whole screen. Resume may also leave the old row behind other output.
            if self
                .invalidated
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                self.fallback(true);
            }
            if self.terminal.as_ref().is_some_and(|terminal| {
                terminal
                    .current_size()
                    .map_or(true, |size| size != terminal.size)
            }) {
                self.fallback(true);
            }
            match event {
                Ok(Event::Phase(phase)) => {
                    if self.partial(Stream::Stderr).is_err() {
                        break;
                    }
                    self.phase = phase;
                    if self.attempted
                        && self.terminal.is_none()
                        && self.lines(Stream::Stderr, &[self.phase.clone()]).is_err()
                    {
                        break;
                    }
                }
                Ok(Event::Line(stream, line)) => {
                    if self.partial(stream).is_err() {
                        break;
                    }
                    if self.lines(stream, &[line]).is_err() {
                        break;
                    }
                }
                Ok(Event::Data(stream, bytes)) => {
                    if self.data(stream, &bytes).is_err() {
                        break;
                    }
                }
                Ok(Event::Finish(done)) => {
                    self.flush();
                    self.terminal.take();
                    let _ = done.send(());
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.flush();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.flush_partial().is_err() {
                        break;
                    }
                }
            }
            if !self.attempted && self.started.elapsed() >= DELAY {
                self.attempted = true;
                self.terminal = Inline::open(self.invalidated.clone()).ok();
                if self.terminal.is_none()
                    && self.lines(Stream::Stderr, &[self.phase.clone()]).is_err()
                {
                    break;
                }
            }
            if self.last_draw.elapsed() >= TICK
                && let Some(terminal) = &mut self.terminal
            {
                let result = terminal.draw(&self.phase, self.started.elapsed(), self.policy);
                self.last_draw = Instant::now();
                if result.is_err() {
                    self.fallback(false);
                }
            }
        }
    }

    fn fallback(&mut self, abandon: bool) {
        if let Some(mut terminal) = self.terminal.take() {
            if abandon {
                // Avoid Drop clearing coordinates that are stale after resume.
                terminal.valid = false;
            }
            drop(terminal);
        }
        self.attempted = true;
    }

    fn lines(&mut self, stream: Stream, lines: &[String]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        if let Some(terminal) = &mut self.terminal {
            if terminal.lines(lines, self.policy).is_ok() {
                return Ok(());
            }
            self.fallback(false);
        }
        for line in lines {
            stream.write(super::log::append_line(line, Theme(self.policy)).as_bytes())?;
        }
        Ok(())
    }

    fn data(&mut self, stream: Stream, bytes: &[u8]) -> io::Result<()> {
        let log = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        let lines = log.push(bytes);
        self.lines(stream, &lines)
    }

    fn flush_partial(&mut self) -> io::Result<()> {
        for stream in [Stream::Stdout, Stream::Stderr] {
            self.partial(stream)?;
        }
        Ok(())
    }

    fn partial(&mut self, stream: Stream) -> io::Result<()> {
        let log = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        if let Some(line) = log.partial() {
            self.lines(stream, &[line])?;
        }
        Ok(())
    }

    fn flush(&mut self) {
        for stream in [Stream::Stdout, Stream::Stderr] {
            let bytes = match stream {
                Stream::Stdout => std::mem::take(&mut self.stdout.pending),
                Stream::Stderr => std::mem::take(&mut self.stderr.pending),
            };
            if !bytes.is_empty() {
                let _ = self.lines(stream, &[terminal_safe(&String::from_utf8_lossy(&bytes))]);
            }
        }
    }
}

/// Bounded line assembly; terminal controls become visible text, not cursor commands.
#[derive(Default)]
struct LogBuffer {
    pending: Vec<u8>,
    after_cr: bool,
}

impl LogBuffer {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for byte in bytes {
            if *byte == b'\n' && self.after_cr {
                self.after_cr = false;
                continue;
            }
            self.after_cr = *byte == b'\r';
            if matches!(byte, b'\n' | b'\r') {
                if !self.pending.is_empty() || *byte == b'\n' {
                    lines.push(terminal_safe(&String::from_utf8_lossy(&self.pending)));
                    self.pending.clear();
                }
            } else {
                self.pending.push(*byte);
                if self.pending.len() >= CHUNK
                    && let Some(line) = self.partial()
                {
                    lines.push(line);
                }
            }
        }
        lines
    }

    fn partial(&mut self) -> Option<String> {
        let end = match std::str::from_utf8(&self.pending) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => self.pending.len(),
        };
        if end == 0 {
            return None;
        }
        let line = terminal_safe(&String::from_utf8_lossy(&self.pending[..end]));
        self.pending.drain(..end);
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_utf8_and_crlf_are_preserved_without_cursor_controls() {
        let mut log = LogBuffer::default();
        assert!(log.push(&[0xe2]).is_empty());
        assert!(log.partial().is_none());
        assert_eq!(log.push(&[0x9c, 0x93, b'\r']), ["✓"]);
        assert_eq!(log.push(b"\nnext\n\x1b[2J\n"), ["next", r"\u{1b}[2J"]);
    }

    #[test]
    fn long_unterminated_output_is_bounded_and_complete() {
        let value = "x".repeat(CHUNK * 4 + 13);
        let mut log = LogBuffer::default();
        let mut lines = log.push(value.as_bytes());
        assert!(log.pending.len() < CHUNK);
        lines.extend(log.partial());
        assert_eq!(lines.concat(), value);
    }

    #[test]
    fn warnings_are_returned_once_without_starting_a_quiet_renderer() {
        let task = Task::start(&Print::quiet(), "Building", true);
        assert!(!task.print().is_live());
        task.print().warn("first warning");
        task.print().warn("second warning");
        assert_eq!(task.finish(), ["first warning", "second warning"]);
    }
}
