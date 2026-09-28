//! Compose existing typed presentation calls into one result, without parsing text.

use super::{Outcome, Print};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub(super) struct Content {
    fields: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl Print {
    /// Group presentation-only detail/status calls into a result card on terminals.
    /// The callback must not perform operations or prompt; plain/quiet/JSON rules
    /// remain those of the original calls. Warnings follow the completed card.
    pub(crate) fn group(&self, outcome: Outcome, title: &str, render: impl FnOnce(&Self)) {
        if !self.has_human_ui() {
            render(self);
            return;
        }
        let content = Arc::new(Mutex::new(Content::default()));
        let mut scoped = self.clone();
        scoped.group = Some(content.clone());
        render(&scoped);
        let content = content
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.outcome_report(outcome, title, &content.fields, &content.warnings);
    }

    pub(super) fn group_detail(&self, label: &str, value: &str) -> bool {
        let Some(group) = &self.group else {
            return false;
        };
        group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fields
            .push((label.to_owned(), value.to_owned()));
        true
    }
}
