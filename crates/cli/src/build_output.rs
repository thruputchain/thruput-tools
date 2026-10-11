//! Cargo's structured stream is used only for the compact human build view.

use std::io::{self, BufRead, BufReader};
use std::process::{Command, Stdio};

use crate::{CliError, Print};

pub(crate) fn run(cmd: &mut Command, print: &Print) -> Result<(String, String), CliError> {
    cmd.env("CARGO_TERM_COLOR", "never")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    print.trace_command(cmd);
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .expect("bug: structured Cargo stdout is piped");
    let stderr = child
        .stderr
        .take()
        .expect("bug: structured Cargo stderr is piped");
    let captured = std::thread::scope(|scope| {
        let reader = scope.spawn(|| read_messages(stdout, print));
        let stderr = crate::capture_cargo_stderr(stderr, true, print);
        if stderr.is_err() {
            crate::stop_cargo(&mut child);
        }
        let stdout = reader
            .join()
            .map_err(|_| io::Error::other("Cargo diagnostic reader panicked"))?;
        Ok::<_, io::Error>((stdout?, stderr?))
    });
    let (stdout, stderr) = match captured {
        Ok(output) => output,
        Err(error) => {
            crate::stop_cargo(&mut child);
            return Err(error.into());
        }
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            crate::stop_cargo(&mut child);
            return Err(error.into());
        }
    };
    if !status.success() {
        // Retain rendered compiler text for existing failure hints (e.g. linker scripts).
        return Err(CliError::CargoFailedWithOutput {
            code: status.code().unwrap_or(1),
            stdout: String::new(),
            stderr: format!("{}{stdout}", String::from_utf8_lossy(&stderr)),
        });
    }
    Ok((String::new(), String::new()))
}

fn read_messages(pipe: impl io::Read, print: &Print) -> io::Result<String> {
    let mut reader = BufReader::new(pipe);
    let mut bytes = Vec::new();
    let mut rendered = String::new();
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            return Ok(rendered);
        }
        let line = String::from_utf8_lossy(&bytes);
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) {
            match record.get("reason").and_then(serde_json::Value::as_str) {
                Some("compiler-message") => {
                    if let Some(text) = print.collect_compiler_message(&record) {
                        rendered.push_str(&text);
                        continue;
                    }
                }
                Some("compiler-artifact" | "build-script-executed" | "build-finished") => continue,
                _ => {}
            }
        }
        // Unknown records and non-JSON output are never discarded or treated as errors.
        print.collect_build_output(&line);
        rendered.push_str(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_malformed_records_remain_in_failure_context() {
        let input = concat!(
            "{\"reason\":\"compiler-artifact\"}\n",
            "{\"reason\":\"future-message\",\"detail\":\"keep me\"}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":null}}\n",
            "Compiling custom output\n",
            "{incomplete"
        );
        let output = read_messages(input.as_bytes(), &Print::new(false, false)).unwrap();
        assert_eq!(output, input.split_once('\n').unwrap().1);
    }
}
