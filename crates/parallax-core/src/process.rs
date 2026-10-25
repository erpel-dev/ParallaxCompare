//! Bounded, noninteractive subprocess execution shared by Git and GitHub CLI.

use std::{
    io::Read,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::{Error, Result};

/// How long output from a helper that outlives the command may still arrive.
/// Every command finishes within its limit; only this grace may follow.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// One pipe's output, read on a thread so a full pipe buffer can never block the child.
///
/// A helper the command started can keep the pipe open after the command and
/// the caller are done. Its reader then keeps reading until the pipe closes (a
/// blocking read cannot be cancelled portably), but throws the bytes away, so late
/// output costs no memory.
struct Drain {
    shared: Arc<Mutex<Vec<u8>>>,
    /// Whether output is still wanted.
    wanted: Arc<AtomicBool>,
    closed: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drain {
    fn spawn(mut pipe: Box<dyn Read + Send>) -> Self {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&shared);
        let wanted = Arc::new(AtomicBool::new(true));
        let keep = Arc::clone(&wanted);
        let (signal, closed) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut buf = [0; 16 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) if keep.load(Ordering::Relaxed) => sink.lock().extend_from_slice(&buf[..read]),
                    Ok(_) => {}
                }
            }
            let _ = signal.send(());
        });
        Self { shared, wanted, closed, thread: Some(thread) }
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
        self.wanted.store(false, Ordering::Relaxed);
        let mut out = self.shared.lock();
        std::mem::take(&mut out)
    }
}

impl Drop for Drain {
    /// Also on the error paths, which never ask for the output.
    fn drop(&mut self) {
        self.wanted.store(false, Ordering::Relaxed);
        self.shared.lock().clear();
    }
}

pub(crate) fn run(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<String> {
    run_bytes(cmd, program, operation, limit).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// Like [`run`], with the output as bytes (file contents from `git cat-file`).
pub(crate) fn run_bytes(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<Vec<u8>> {
    let (status, out, err) = run_status(cmd, program, operation, limit)?;
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

/// Runs a command to its end, whatever its exit status: the status, standard
/// output and standard error. Only failing to start and the time limit are errors.
pub(crate) fn run_status(
    cmd: &mut Command,
    program: &str,
    operation: &str,
    limit: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
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
    Ok((status, out, err))
}

#[cfg(test)]
mod tests {
    /// A helper that keeps the output open after the command exits: the command's
    /// own output comes back within the grace, and the late output is not kept.
    #[cfg(unix)]
    #[test]
    fn helpers_holding_the_pipe_do_not_hold_the_caller() {
        let started = Instant::now();
        let out = run(
            Command::new("sh").args(["-c", "echo hi; (sleep 2; head -c 4000000 /dev/zero) &"]),
            "sh",
            "sh",
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out, "hi\n");
        assert!(started.elapsed() < Duration::from_millis(1900), "{:?}", started.elapsed());
    }

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
