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

/// A command for a program of the system (git, gh, flatc, mergiraf, xdg-open).
///
/// Inside an AppImage the app runs with its own libraries first on
/// `LD_LIBRARY_PATH` (and its folders on `PATH`, `XDG_DATA_DIRS` …). A system
/// program started with them loads libraries it was not built for and fails
/// ("symbol lookup error", "no version information available"), so those
/// entries are taken out of the program's environment. Elsewhere this is `Command::new`.
#[allow(clippy::disallowed_methods)] // the one place system programs are created
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    if let Some(dir) = bundle_dir() {
        for (key, value) in outside_bundle(std::env::vars_os(), &dir) {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            };
        }
    }
    cmd
}

/// Whether the app runs in a Flatpak sandbox. The system's programs (git, gh,
/// mergiraf, flatc) are not inside it, and neither are the user's sign-ins and
/// Git settings: they run on the host through `flatpak-spawn --host`, which the
/// app's `org.freedesktop.Flatpak` permission allows.
pub fn in_flatpak() -> bool {
    static FLATPAK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLATPAK.get_or_init(|| std::path::Path::new("/.flatpak-info").exists())
}

/// `cmd` as `flatpak-spawn --host`: the same program and arguments on the host,
/// with its working folder and the variables it sets. Variables it removes stay
/// as the host's session has them. Paths mean the same on both sides for the
/// user's files and the app's cache; the sandbox's /tmp is its own (see
/// [`shared_temp_dir`]).
fn on_host(cmd: &Command) -> Command {
    #[allow(clippy::disallowed_methods)] // runs the program given, on the host
    let mut host = Command::new("flatpak-spawn");
    // --watch-bus: the program ends when the app does.
    host.args(["--host", "--watch-bus"]);
    if let Some(dir) = cmd.get_current_dir() {
        let mut arg = std::ffi::OsString::from("--directory=");
        arg.push(dir);
        host.arg(arg);
    }
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            let mut arg = std::ffi::OsString::from("--env=");
            arg.push(key);
            arg.push("=");
            arg.push(value);
            host.arg(arg);
        }
    }
    host.arg(cmd.get_program()).args(cmd.get_args());
    host
}

/// A temporary folder for files a system program reads or writes (mergiraf's
/// inputs, flatc's schemas). In a Flatpak those programs run on the host, which
/// does not see the sandbox's /tmp, so the folder is made in the app's cache,
/// whose path is the same on both sides. Elsewhere it is an ordinary temporary folder.
pub fn shared_temp_dir(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    match std::env::var_os("XDG_CACHE_HOME").filter(|_| in_flatpak()) {
        Some(cache) => {
            let dir = std::path::PathBuf::from(cache).join("tmp");
            std::fs::create_dir_all(&dir)?;
            builder.tempdir_in(dir)
        }
        None => builder.tempdir(),
    }
}

/// The folder an AppImage runs from, while the app runs as one.
pub fn bundle_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("APPIMAGE")?;
    let dir = std::env::var_os("APPDIR")?;
    (std::path::Path::new(&dir).is_absolute() && dir.len() > 1).then_some(dir)
}

/// The variables to change for a program outside the bundle at `dir`: lists of
/// paths without the bundle's entries (None: the variable goes, as nothing is left),
/// and single paths into the bundle removed.
fn outside_bundle(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    dir: &std::ffi::OsStr,
) -> Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> {
    let Some(dir) = dir.to_str().map(|d| d.trim_end_matches('/').to_string()) else { return Vec::new() };
    let inside = |p: &str| p == dir || p.starts_with(&format!("{dir}/"));
    let mut out = Vec::new();
    for (key, value) in vars {
        let Some(text) = value.to_str() else { continue };
        if !text.contains(&dir) {
            continue;
        }
        let kept: Vec<&str> = text.split(':').filter(|p| !inside(p)).collect();
        if kept.len() == text.split(':').count() {
            continue;
        }
        let kept = kept.join(":");
        out.push((key, (!kept.trim_matches(':').is_empty()).then(|| kept.into())));
    }
    out
}

/// Whether a program's error output says it could not load its libraries.
pub(crate) fn library_failure(stderr: &str) -> bool {
    ["symbol lookup error", "error while loading shared libraries", "no version information available"]
        .iter()
        .any(|m| stderr.contains(m))
}

/// How long output from a helper that outlives the command may still arrive.
/// Every command finishes within its limit; only this grace may follow.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

// Listings can be large, but diagnostics should never consume comparable memory.
const TEXT_OUTPUT: usize = 16 * 1024 * 1024;
const DIAGNOSTIC_OUTPUT: usize = 1024 * 1024;
// Blob fallback has its own budget; ordinary commands do not inherit this allowance.
const BLOB_OUTPUT: usize = 256 * 1024 * 1024;

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
    exceeded: Arc<AtomicBool>,
    closed: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drain {
    fn spawn(mut pipe: Box<dyn Read + Send>, budget: usize) -> Self {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&shared);
        let wanted = Arc::new(AtomicBool::new(true));
        let keep = Arc::clone(&wanted);
        let exceeded = Arc::new(AtomicBool::new(false));
        let overflow = Arc::clone(&exceeded);
        let (signal, closed) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut buf = [0; 16 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) if keep.load(Ordering::Relaxed) => {
                        let mut bytes = sink.lock();
                        let fits = read <= budget.saturating_sub(bytes.len());
                        // Geometric growth avoids reallocating on every pipe read;
                        // the requested capacity still never exceeds the budget.
                        let needed = bytes.len() + read;
                        let reserve = if needed > bytes.capacity() {
                            bytes.capacity().saturating_mul(2).max(needed).min(budget).saturating_sub(bytes.len())
                        } else {
                            0
                        };
                        if !fits || bytes.try_reserve_exact(reserve).is_err() {
                            overflow.store(true, Ordering::Relaxed);
                            keep.store(false, Ordering::Relaxed);
                        } else {
                            bytes.extend_from_slice(&buf[..read]);
                        }
                    }
                    Ok(_) => {}
                }
            }
            let _ = signal.send(());
        });
        Self { shared, wanted, exceeded, closed, thread: Some(thread) }
    }

    /// The bytes that arrived until `deadline`. A helper that inherited the pipe
    /// (git spawning ssh or a credential helper) can hold it open indefinitely,
    /// so the wait is bounded and a partial result is returned instead.
    fn until(mut self, deadline: Instant) -> (Vec<u8>, bool) {
        if self.closed.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
        self.wanted.store(false, Ordering::Relaxed);
        let mut out = self.shared.lock();
        (std::mem::take(&mut out), self.exceeded.load(Ordering::Relaxed))
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

/// Like [`run`], with bounded output as bytes (Git status and other listings).
pub(crate) fn run_bytes(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<Vec<u8>> {
    run_with_budget(cmd, program, operation, limit, TEXT_OUTPUT)
}

pub(crate) fn run_blob(cmd: &mut Command, program: &str, operation: &str, limit: Duration) -> Result<Vec<u8>> {
    run_with_budget(cmd, program, operation, limit, BLOB_OUTPUT)
}

fn run_with_budget(
    cmd: &mut Command,
    program: &str,
    operation: &str,
    limit: Duration,
    budget: usize,
) -> Result<Vec<u8>> {
    let (status, out, err) = run_status_with_budget(cmd, program, operation, limit, budget)?;
    if status.success() {
        Ok(out)
    } else {
        let detail = String::from_utf8_lossy(&err);
        let detail = detail.trim();
        if library_failure(detail) {
            // Not the request's fault: say so first, then the loader's own words.
            return Err(Error::Unsupported(format!(
                "{operation}: {program} could not start because it loaded libraries that do not fit it; this is not a sign-in or access problem. {}",
                detail
                    .lines()
                    .find(|l| l.contains("symbol lookup error") || l.contains("error while loading"))
                    .unwrap_or(detail)
            )));
        }
        Err(Error::Unsupported(format!(
            "{operation}: {}",
            if detail.is_empty() { status.to_string() } else { detail.to_string() }
        )))
    }
}

/// Runs a command to its end, whatever its exit status: the status, standard
/// output and standard error. Start failures, time and output limits are errors.
pub(crate) fn run_status(
    cmd: &mut Command,
    program: &str,
    operation: &str,
    limit: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    run_status_with_budget(cmd, program, operation, limit, TEXT_OUTPUT)
}

fn output_error(operation: &str, budget: usize) -> Error {
    Error::Unsupported(format!(
        "{operation}: helper output exceeded its memory limit ({budget} bytes stdout, {DIAGNOSTIC_OUTPUT} bytes stderr), or memory could not be reserved. Reduce the request size and retry."
    ))
}

fn run_status_with_budget(
    cmd: &mut Command,
    program: &str,
    operation: &str,
    limit: Duration,
    budget: usize,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut hosted;
    let cmd = if in_flatpak() {
        hosted = on_host(cmd);
        &mut hosted
    } else {
        cmd
    };
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
    let stdout = Drain::spawn(Box::new(child.stdout.take().expect("piped stdout")), budget);
    let stderr = Drain::spawn(Box::new(child.stderr.take().expect("piped stderr")), DIAGNOSTIC_OUTPUT);
    let started = Instant::now();
    let status = loop {
        if stdout.exceeded.load(Ordering::Relaxed) || stderr.exceeded.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(output_error(operation, budget));
        }
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
    let ((out, out_exceeded), (err, err_exceeded)) = (stdout.until(deadline), stderr.until(deadline));
    if out_exceeded || err_exceeded {
        return Err(output_error(operation, budget));
    }
    Ok((status, out, err))
}

#[cfg(test)]
mod tests {
    #[test]
    fn flatpak_runs_programs_on_the_host_with_their_folder_and_variables() {
        let mut cmd = command("git");
        cmd.args(["-C", "/home/u/repo", "fetch"]).current_dir("/home/u").env("GIT_TERMINAL_PROMPT", "0");
        let host = on_host(&cmd);
        assert_eq!(host.get_program(), "flatpak-spawn");
        let args: Vec<_> = host.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            [
                "--host",
                "--watch-bus",
                "--directory=/home/u",
                "--env=GIT_TERMINAL_PROMPT=0",
                "git",
                "-C",
                "/home/u/repo",
                "fetch"
            ]
        );
    }

    #[test]
    fn programs_outside_an_appimage_get_the_systems_libraries() {
        use std::ffi::OsString;
        let vars = [
            ("LD_LIBRARY_PATH", "/tmp/.mount_Px/usr/lib:/tmp/.mount_Px/usr/lib/x86_64-linux-gnu"),
            ("PATH", "/tmp/.mount_Px/usr/bin:/usr/local/bin:/usr/bin"),
            ("GDK_PIXBUF_MODULE_FILE", "/tmp/.mount_Px/usr/lib/gdk-pixbuf/loaders.cache"),
            ("XDG_DATA_DIRS", "/usr/share:/tmp/.mount_Px/usr/share"),
            ("HOME", "/home/timo"),
            ("OTHER", "/tmp/.mount_Pxy/usr/lib"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let changed = outside_bundle(vars.into_iter(), "/tmp/.mount_Px/".as_ref());
        let get = |k: &str| changed.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("LD_LIBRARY_PATH"), Some(None));
        assert_eq!(get("GDK_PIXBUF_MODULE_FILE"), Some(None));
        assert_eq!(get("PATH"), Some(Some("/usr/local/bin:/usr/bin".into())));
        assert_eq!(get("XDG_DATA_DIRS"), Some(Some("/usr/share".into())));
        // Unrelated variables and look-alike folders stay as they are.
        assert_eq!((get("HOME"), get("OTHER")), (None, None));
        assert!(library_failure("git-remote-https: symbol lookup error: /usr/lib/libcurl.so.4: undefined symbol: x"));
        assert!(!library_failure("fatal: repository not found"));
    }

    #[test]
    fn drains_stop_retaining_bytes_at_the_budget() {
        let drain = Drain::spawn(Box::new(std::io::Cursor::new(vec![0; 100000])), 32);
        let (bytes, exceeded) = drain.until(Instant::now() + Duration::from_secs(5));
        assert!(exceeded);
        assert!(bytes.len() <= 32);
        let drain = Drain::spawn(Box::new(std::io::Cursor::new(vec![42; 32])), 32);
        assert_eq!(drain.until(Instant::now() + Duration::from_secs(5)), (vec![42; 32], false));
    }

    #[test]
    #[cfg(unix)]
    fn excessive_stdout_and_stderr_report_resource_limits() {
        for script in ["head -c 65536 /dev/zero", "head -c 2000000 /dev/zero >&2"] {
            let error = run_with_budget(
                Command::new("sh").args(["-c", script]),
                "sh",
                "test output",
                Duration::from_secs(5),
                32,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("test output") && error.contains("memory limit"), "{error}");
        }
        // The blob path must not inherit the much smaller listing budget.
        let out = run_blob(
            Command::new("sh").args(["-c", "head -c 17000000 /dev/zero"]),
            "sh",
            "test blob",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.len(), 17000000);
    }

    #[test]
    #[cfg(unix)]
    fn an_unending_output_producer_is_stopped_at_the_budget() {
        let started = Instant::now();
        let error = run_with_budget(&mut Command::new("yes"), "yes", "test output", Duration::from_secs(10), 1024)
            .unwrap_err()
            .to_string();
        assert!(error.contains("memory limit"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }

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
