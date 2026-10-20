//! Bounded, noninteractive subprocess execution shared by Git and GitHub CLI.

use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crate::{Error, Result};

pub(crate) fn run(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::Unsupported(format!("{program} could not be run. Install {program} and make it available on PATH: {e}"))
    })?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    // Drain both streams while waiting: either pipe can otherwise block a child.
    let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::io(program, e));
            }
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Unsupported(format!("{operation}: no answer after {} s", limit.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (out, err) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        let detail = String::from_utf8_lossy(&err);
        let detail = detail.trim();
        Err(Error::Unsupported(format!(
            "{operation}: {}",
            if detail.is_empty() { status.to_string() } else { detail.to_string() }
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_tools_report_installation_and_path() {
        let error = run(
            &mut Command::new("parallax-test-missing-executable-385029"),
            "gh",
            "gh request",
            Duration::from_secs(1),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Install gh") && error.contains("PATH"), "{error}");
    }

    #[test]
    fn failed_commands_keep_the_tools_error() {
        let error = run(
            Command::new("git").arg("parallax-nonexistent-subcommand"),
            "git",
            "git request",
            Duration::from_secs(5),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("git request") && error.contains("parallax-nonexistent-subcommand"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn stalled_commands_have_a_deadline() {
        let start = Instant::now();
        let error =
            run(Command::new("sh").args(["-c", "exec sleep 5"]), "sh", "test request", Duration::from_millis(30))
                .unwrap_err()
                .to_string();
        assert!(error.contains("no answer"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
