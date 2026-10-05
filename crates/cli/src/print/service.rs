//! Coordinate service logs with temporary startup and shutdown progress.

use std::sync::{Arc, Mutex};

use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

use super::{
    Outcome, Print, log,
    progress::{Stream, Task, TaskOutput},
    report::Report,
};

const MAX_PENDING_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct Record {
    level: Level,
    text: String,
}

impl Record {
    fn line(&self) -> String {
        format!("{} {}", self.level, self.text)
    }

    fn outcome(&self) -> Outcome {
        match self.level {
            Level::ERROR => Outcome::Error,
            Level::WARN => Outcome::Warning,
            _ => Outcome::Information,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    task: Option<Arc<TaskOutput>>,
    pending: Vec<Record>,
    bytes: usize,
}

/// Shared tracing destination for an exclusive foreground service command.
#[derive(Debug, Clone)]
pub(crate) struct ServiceLogs {
    print: Print,
    state: Arc<Mutex<State>>,
}

impl ServiceLogs {
    pub fn new(print: &Print) -> Self {
        Self {
            print: print.clone(),
            state: Arc::default(),
        }
    }

    pub fn start(&self, phase: &str) -> ServicePhase {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task = Task::start(&self.print, phase, true);
        task.print().phase(phase);
        state.task.clone_from(&task.print().task);
        ServicePhase {
            logs: self.clone(),
            task: Some(task),
        }
    }

    fn record(&self, record: Record) {
        use std::io::Write;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = state.task.clone() {
            // An unusually noisy startup falls back to coordinated live output;
            // do not grow memory without limit or discard diagnostics.
            if state.pending.len() >= 256
                || state.bytes.saturating_add(record.text.len()) > MAX_PENDING_BYTES
            {
                for record in state.pending.drain(..).chain(std::iter::once(record)) {
                    let line = record.line();
                    if !task.line(Stream::Stderr, &line) {
                        let _ = std::io::stderr().lock().write_all(
                            log::append_line(
                                &line,
                                super::theme::Theme(self.print.output_policy()),
                            )
                            .as_bytes(),
                        );
                    }
                }
                state.bytes = 0;
            } else {
                state.bytes += record.text.len();
                state.pending.push(record);
            }
        } else {
            let line = log::append_line(
                &record.line(),
                super::theme::Theme(self.print.output_policy()),
            );
            let _ = std::io::stderr().lock().write_all(line.as_bytes());
        }
    }
}

impl<S: Subscriber> Layer<S> for ServiceLogs {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.record(Record {
            level: *event.metadata().level(),
            text: fields.text(),
        });
    }
}

/// Retains phase diagnostics until the result has been printed. On failure,
/// dropping the phase passes diagnostics to the command's final error report.
pub(crate) struct ServicePhase {
    logs: ServiceLogs,
    task: Option<Task>,
}

impl ServicePhase {
    pub fn print(&self) -> &Print {
        self.task.as_ref().map_or(&self.logs.print, Task::print)
    }

    pub fn finish(mut self, render: impl FnOnce()) {
        let mut state = self
            .logs
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = self.task.take() {
            for warning in task.finish() {
                self.logs.print.defer_warning(warning);
            }
        }
        defer_pending(&self.logs.print, &mut state);
        // Keep new live records behind the result and its startup diagnostics.
        render();
        let mut report = Report::new(self.logs.print.output_policy());
        self.logs.print.append_diagnostics(&mut report);
        self.logs.print.write_report(report);
        state.task = None;
    }
}

impl Drop for ServicePhase {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let mut state = self
                .logs
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for warning in task.finish() {
                self.logs.print.defer_warning(warning);
            }
            defer_pending(&self.logs.print, &mut state);
            state.task = None;
        }
    }
}

fn defer_pending(print: &Print, state: &mut State) {
    for record in state.pending.drain(..) {
        print.defer_diagnostic(record.outcome(), record.text);
    }
    state.bytes = 0;
}

#[derive(Default)]
struct Fields {
    message: String,
    context: Vec<String>,
}

impl Fields {
    fn text(self) -> String {
        let context = self.context.join(" ");
        crate::terminal::terminal_safe(&if context.is_empty() {
            self.message
        } else {
            format!("{} {context}", self.message)
        })
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "hogd_timestamp_ms" | "hogd_target" | "hogd_file" | "hogd_line" | "hogd_fields" => {}
            name => self.context.push(format!("{name}={value:?}")),
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.into();
        } else {
            self.record_debug(field, &value);
        }
    }
}
