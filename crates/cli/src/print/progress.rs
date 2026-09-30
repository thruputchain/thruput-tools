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
    buffer::Buffer,
    crossterm::{
        cursor::MoveToColumn,
        queue,
        terminal::{Clear, ClearType, size},
    },
    layout::{Rect, Size},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::{
    Print,
    policy::Policy,
    theme::{Outcome, Theme},
};
use crate::terminal::terminal_safe;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn write(self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Stdout => {
                let mut output = io::stdout().lock();
                output.write_all(bytes)?;
                output.flush()
            }
            Self::Stderr => {
                let mut output = io::stderr().lock();
                output.write_all(bytes)?;
                output.flush()
            }
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
    // A background job must not rewrite the foreground application's output.
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
    size: Size,
    valid: bool,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
}

impl Inline {
    fn open(invalidated: Arc<std::sync::atomic::AtomicBool>) -> io::Result<Self> {
        let (width, height) = size()?;
        let size = Size::new(width, height);
        if size.width < 24 || size.height < 3 {
            return Err(io::Error::other(
                "terminal is too small for inline progress",
            ));
        }
        // Reserve a fresh row without querying the cursor or reading stdin.
        // Crossterm's cursor query temporarily enters raw mode, which can swallow
        // keyboard signals or leave terminal input altered on abrupt termination.
        let mut stderr = io::stderr().lock();
        stderr.write_all(b"\r\n")?;
        stderr.flush()?;
        Ok(Self {
            size,
            valid: true,
            invalidated,
        })
    }

    fn current_size(&self) -> io::Result<Size> {
        let (width, height) = size()?;
        if self.invalidated.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::other("progress row invalidated"));
        }
        Ok(Size::new(width, height))
    }

    fn draw(&mut self, phase: &str, elapsed: Duration, policy: Policy) -> io::Result<()> {
        self.check_size()?;
        let content = Rect::new(0, 0, self.size.width - 1, 1);
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
        let mut buffer = Buffer::empty(content);
        Paragraph::new(line).render(content, &mut buffer);
        let mut stderr = io::stderr().lock();
        self.clear_row(&mut stderr)?;
        for line in super::render::buffer_lines(&buffer, policy.color) {
            stderr.write_all(line.as_bytes())?;
        }
        stderr.flush()
    }

    fn lines(&mut self, stream: Stream, lines: &[String], policy: Policy) -> io::Result<()> {
        self.check_size()?;
        let mut renderer = super::render::Renderer::new(policy);
        renderer.text(
            ratatui::text::Text::from(
                lines
                    .iter()
                    .map(|line| super::log::line(line, Theme(policy)))
                    .collect::<Vec<_>>(),
            ),
            0,
        );
        self.check_size()?;
        commit_log_lines(
            stream,
            &renderer.lines,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
    }

    fn check_size(&self) -> io::Result<()> {
        if self.current_size()? != self.size {
            return Err(io::Error::other("progress row resized"));
        }
        Ok(())
    }

    fn clear_row(&self, stderr: &mut impl Write) -> io::Result<()> {
        self.check_size()?;
        queue!(stderr, MoveToColumn(0), Clear(ClearType::CurrentLine))
    }

    fn finish(&mut self) {
        if self.valid
            && !thread::panicking()
            && self.current_size().is_ok_and(|size| size == self.size)
        {
            let mut stderr = io::stderr().lock();
            let _ = self.clear_row(&mut stderr).and_then(|()| stderr.flush());
        } else {
            // A resize can reflow the old row. Never guess its new coordinates.
            let _ = writeln!(io::stderr());
        }
    }
}

impl Drop for Inline {
    fn drop(&mut self) {
        self.finish();
    }
}

fn commit_log_lines(
    stream: Stream,
    lines: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<()> {
    queue!(stderr, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    stderr.flush()?;
    let output: &mut dyn Write = match stream {
        Stream::Stdout => stdout,
        Stream::Stderr => stderr,
    };
    for line in lines {
        writeln!(output, "{line}")?;
    }
    output.flush()
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
    line_open: Option<Stream>,
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
            line_open: None,
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
            // Reflow or resume invalidates row ownership. Switch to append-only
            // output before doing any further cursor writes.
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
                        && self.message(Stream::Stderr, &self.phase.clone()).is_err()
                    {
                        break;
                    }
                }
                Ok(Event::Line(stream, line)) => {
                    if self.partial(stream).is_err() {
                        break;
                    }
                    if self.message(stream, &line).is_err() {
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
                Err(mpsc::RecvTimeoutError::Timeout) => {}
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
        self.separate_stream(stream)?;
        if let Some(terminal) = &mut self.terminal {
            if terminal.lines(stream, lines, self.policy).is_ok() {
                self.line_open = None;
                return Ok(());
            }
            self.fallback(false);
        }
        for line in lines {
            stream.write(super::log::append_line(line, Theme(self.policy)).as_bytes())?;
        }
        self.line_open = None;
        Ok(())
    }

    fn message(&mut self, stream: Stream, text: &str) -> io::Result<()> {
        self.end_line()?;
        self.lines(stream, &[text.to_owned()])
    }

    fn end_line(&mut self) -> io::Result<()> {
        if self.line_open.is_some() {
            Stream::Stderr.write(b"\n")?;
            self.line_open = None;
        }
        Ok(())
    }

    fn fragment(&mut self, stream: Stream, text: &str) -> io::Result<()> {
        // An unfinished line owns the cursor until the child completes it. Stop
        // animation rather than insert a newline or repaint over that output.
        self.fallback(false);
        self.separate_stream(stream)?;
        stream.write(text.as_bytes())?;
        self.line_open = Some(stream);
        Ok(())
    }

    fn separate_stream(&mut self, stream: Stream) -> io::Result<()> {
        // Overlong lines are streamed to keep memory bounded. If another stream
        // interrupts one, separate the display instead of joining unrelated logs.
        if self.line_open.is_some_and(|open| open != stream) {
            self.end_line()?;
        }
        Ok(())
    }

    fn data(&mut self, stream: Stream, bytes: &[u8]) -> io::Result<()> {
        let log = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        for chunk in log.push(bytes) {
            if chunk.complete {
                self.lines(stream, &[chunk.text])?;
            } else {
                self.fragment(stream, &chunk.text)?;
            }
        }
        Ok(())
    }

    fn partial(&mut self, stream: Stream) -> io::Result<()> {
        let log = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        if let Some(line) = log.partial() {
            self.fragment(stream, &line)?;
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
                let _ = self.fragment(stream, &terminal_safe(&String::from_utf8_lossy(&bytes)));
            }
        }
        let _ = self.end_line();
    }
}

/// Assemble each stream until a terminator, explicit message boundary, or buffer
/// limit. Idle time is not a line boundary. Escape controls before display.
#[derive(Default)]
struct LogBuffer {
    pending: Vec<u8>,
    after_cr: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct LogChunk {
    text: String,
    complete: bool,
}

impl LogBuffer {
    fn push(&mut self, bytes: &[u8]) -> Vec<LogChunk> {
        let mut lines = Vec::new();
        for byte in bytes {
            if *byte == b'\n' && self.after_cr {
                self.after_cr = false;
                continue;
            }
            self.after_cr = *byte == b'\r';
            if matches!(byte, b'\n' | b'\r') {
                lines.push(LogChunk {
                    text: terminal_safe(&String::from_utf8_lossy(&self.pending)),
                    complete: true,
                });
                self.pending.clear();
            } else {
                self.pending.push(*byte);
                if self.pending.len() >= CHUNK
                    && let Some(line) = self.partial()
                {
                    lines.push(LogChunk {
                        text: line,
                        complete: false,
                    });
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
        assert_eq!(
            log.push(&[0x9c, 0x93, b'\r']),
            [LogChunk {
                text: "✓".into(),
                complete: true
            }]
        );
        assert_eq!(
            log.push(b"\nnext\n\x1b[2J\n"),
            ["next", r"\u{1b}[2J"].map(|text| LogChunk {
                text: text.into(),
                complete: true
            })
        );
    }

    #[test]
    fn long_unterminated_output_is_bounded_and_complete() {
        let value = "x".repeat(CHUNK * 4 + 13);
        let mut log = LogBuffer::default();
        let chunks = log.push(value.as_bytes());
        assert!(log.pending.len() < CHUNK);
        assert!(chunks.iter().all(|chunk| !chunk.complete));
        let mut lines = chunks
            .into_iter()
            .map(|chunk| chunk.text)
            .collect::<Vec<_>>();
        lines.extend(log.partial());
        assert_eq!(lines.concat(), value);
    }

    #[test]
    fn live_log_commit_keeps_child_streams_separate_from_cursor_control() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        commit_log_lines(
            Stream::Stdout,
            &["child stdout".into()],
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(stdout, b"child stdout\n");
        assert!(!String::from_utf8_lossy(&stderr).contains("child stdout"));
        assert!(stderr.starts_with(b"\x1b["));
        commit_log_lines(
            Stream::Stderr,
            &["child stderr".into()],
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(stdout, b"child stdout\n");
        assert!(stderr.ends_with(b"child stderr\n"));
    }

    #[test]
    fn partial_flush_preserves_a_later_crlf_terminator() {
        let mut log = LogBuffer::default();
        assert!(log.push(b"prefix").is_empty());
        assert_eq!(log.partial().as_deref(), Some("prefix"));
        assert_eq!(
            log.push(b"\r"),
            [LogChunk {
                text: String::new(),
                complete: true
            }]
        );
        assert!(log.push(b"\n").is_empty());
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
