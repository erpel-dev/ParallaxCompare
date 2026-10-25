//! Bounded, noninteractive subprocess execution shared by Git and GitHub CLI.

use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::{Error, Result};

/// How long output from a helper that outlives the command may still arrive.
/// Every command finishes within its limit; only this grace may follow.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// One pipe's output, read on a thread so a full pipe buffer can never block the child.
struct Drain {
    shared: Arc<Mutex<Vec<u8>>>,
    closed: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drain {
    fn spawn(mut pipe: Box<dyn Read + Send>) -> Self {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&shared);
        let (signal, closed) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut buf = [0; 16 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => sink.lock().extend_from_slice(&buf[..read]),
                }
            }
            let _ = signal.send(());
        });
        Self { shared, closed, thread: Some(thread) }
    }

    /// The bytes that arrived until `deadline`. A helper that inherited the pipe
    /// (git spawning ssh or a credential helper) can hold it open indefinitely,
    /// so the wait is bounded and a partial result is returned instead.
    fn until(mut self, deadline: Instant) -> Vec<u8> {
        if self.closed.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
        let mut out = self.shared.lock();
        std::mem::take(&mut out)
    }
}

pub(crate) fn run(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<String> {
    run_bytes(cmd, program, operation, limit).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// Like [`run`], with the output as bytes (file contents from `git cat-file`).
pub(crate) fn run_bytes(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<Vec<u8>> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::Unsupported(format!("{program} could not be run. Install {program} and make it available on PATH: {e}"))
    })?;
    // Drain both streams while waiting: either pipe can otherwise block a child.
    let stdout = Drain::spawn(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = Drain::spawn(Box::new(child.stderr.take().expect("piped stderr")));
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
    // The command has exited, but helpers it started can still hold the pipes.
    // Its output is complete, so the grace applies even at the limit: truncating
    // a successful result would be worse than returning 1 s later.
    let deadline = Instant::now() + DRAIN_GRACE;
    let (out, err) = (stdout.until(deadline), stderr.until(deadline));
    if status.success() {
        Ok(out)
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

    #[test]
    #[cfg(unix)]
    fn pipes_held_by_a_grandchild_do_not_outlive_the_deadline() {
        // The background sleep inherits both pipes and outlives the shell.
        let start = Instant::now();
        let out = run(
            Command::new("sh").args(["-c", "echo done; sleep 5 & exit 0"]),
            "sh",
            "test request",
            Duration::from_millis(300),
        )
        .unwrap();
        assert_eq!(out.trim(), "done");
        assert!(start.elapsed() < Duration::from_millis(1500), "took {:?}", start.elapsed());
    }

    #[test]
    #[cfg(unix)]
    fn stalled_commands_report_without_waiting_for_grandchildren() {
        // Killing the direct child leaves the sleep behind, still holding the pipes.
        let start = Instant::now();
        let error = run(
            Command::new("sh").args(["-c", "sleep 5 & exec sleep 5"]),
            "sh",
            "test request",
            Duration::from_millis(30),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no answer"));
        assert!(start.elapsed() < Duration::from_secs(2), "took {:?}", start.elapsed());
    }
}
