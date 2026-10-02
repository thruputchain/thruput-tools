/// Width of the terminal receiving human status output, when available.
pub(crate) fn stderr_width() -> Option<usize> {
    output_size(false).map(|(width, _)| width)
}

/// Height of the terminal receiving human status output, when available.
pub(crate) fn stderr_height() -> Option<usize> {
    output_size(false)
        .map(|(_, height)| height)
        .filter(|height| *height > 0)
}

/// Width of the terminal receiving human data output, when available.
pub(crate) fn stdout_width() -> Option<usize> {
    output_size(true).map(|(width, _)| width)
}

fn output_size(stdout: bool) -> Option<(usize, usize)> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: size is writable and TIOCGWINSZ only fills that winsize.
        let result = unsafe {
            libc::ioctl(
                if stdout {
                    std::io::stdout().as_raw_fd()
                } else {
                    std::io::stderr().as_raw_fd()
                },
                libc::TIOCGWINSZ,
                &raw mut size,
            )
        };
        if result == 0 && size.ws_col > 0 {
            return Some((usize::from(size.ws_col), usize::from(size.ws_row)));
        }
    }
    #[cfg(not(unix))]
    let _ = stdout;
    None
}

/// Escape control and bidirectional-formatting characters in terminal output.
pub(crate) fn terminal_safe(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// Render one terminal-safe argument for a copy-pasteable POSIX shell command.
///
/// Control and bidirectional-formatting characters are shown as visible escapes
/// instead of being reproduced in the resulting argument.
pub(crate) fn render_shell_arg(value: &str) -> String {
    let value = terminal_safe(value);
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"%+,-./:=@_".contains(&byte))
    {
        return value;
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{render_shell_arg, terminal_safe};

    #[test]
    fn terminal_safe_escapes_controls_and_bidi_formatting() {
        assert_eq!(terminal_safe("a\n\u{1b}\u{202e}b"), r"a\n\u{1b}\u{202e}b");
    }

    #[test]
    fn render_shell_arg_quotes_only_when_needed() {
        for (input, expected) in [
            ("https://rpc.example:8545/v1", "https://rpc.example:8545/v1"),
            (
                "https://rpc.example?key=x&network=test",
                "'https://rpc.example?key=x&network=test'",
            ),
            ("two words", "'two words'"),
            ("a'b", r"'a'\''b'"),
            ("", "''"),
        ] {
            assert_eq!(render_shell_arg(input), expected);
        }
    }

    #[test]
    fn render_shell_arg_never_reproduces_terminal_controls() {
        let rendered = render_shell_arg("$(touch /tmp/pwned)\n\u{1b}[31m");
        assert_eq!(rendered, r"'$(touch /tmp/pwned)\n\u{1b}[31m'");
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{1b}'));
    }
}
