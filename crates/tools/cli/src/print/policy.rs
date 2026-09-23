use std::io::{self, IsTerminal};

/// Capabilities for one human-output destination. JSON bypasses rendering entirely.
#[derive(Debug, Clone, Copy)]
pub(super) struct Policy {
    pub width: Option<u16>,
    pub color: bool,
    pub unicode: bool,
}

impl Policy {
    pub const PLAIN: Self = Self {
        width: None,
        color: false,
        unicode: false,
    };

    pub fn stderr(no_ui: bool) -> Self {
        Self::destination(
            no_ui,
            io::stderr().is_terminal(),
            crate::terminal::stderr_width(),
        )
    }

    pub fn stdout(no_ui: bool) -> Self {
        Self::destination(
            no_ui,
            io::stdout().is_terminal(),
            crate::terminal::stdout_width(),
        )
    }

    fn destination(no_ui: bool, tty: bool, width: Option<usize>) -> Self {
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
        Self::resolve(
            no_ui,
            tty,
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("NO_COLOR").is_some(),
            locale.as_deref(),
            width,
        )
    }

    pub fn resolve(
        no_ui: bool,
        tty: bool,
        term: Option<&str>,
        no_color: bool,
        locale: Option<&str>,
        width: Option<usize>,
    ) -> Self {
        if no_ui || !tty || term == Some("dumb") {
            return Self::PLAIN;
        }
        let Some(width) = width.filter(|width| *width >= 2) else {
            return Self::PLAIN;
        };
        Self {
            width: Some(u16::try_from(width.min(100)).unwrap_or(100)),
            color: !no_color,
            unicode: locale.is_none_or(|locale| {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            }),
        }
    }
}
