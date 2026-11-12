//! Whether the programs the app relies on start and work: git, gh and the
//! system's opener. Used by `parallax-compare --self-test` and shown next to a
//! pull request URL before it is resolved, so a broken setup is named precisely
//! instead of surfacing as a failed fetch.

use std::time::Duration;

use serde::Serialize;

use crate::process;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Ok,
    /// Not installed, or not on PATH.
    Missing,
    /// Installed, but it failed: could not load its libraries, no network …
    Broken,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// What was checked, such as `git` or `git ls-remote`.
    pub name: String,
    pub state: State,
    /// The version, or what went wrong, in one line.
    pub detail: String,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.state == State::Ok
    }
}

/// Runs a program as the app runs it and condenses the outcome into one line.
fn run(name: &str, program: &str, args: &[&str], limit: Duration) -> Check {
    run_text(name, program, args, limit).0
}

/// [`run`], with all the program printed (standard output, then standard error).
fn run_text(name: &str, program: &str, args: &[&str], limit: Duration) -> (Check, String) {
    let mut cmd = process::command(program);
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").env("GH_PROMPT_DISABLED", "1").env("GH_NO_UPDATE_NOTIFIER", "1");
    let check = |state, detail: String| Check { name: name.to_string(), state, detail };
    match process::run_status(&mut cmd, program, name, limit) {
        Err(e) => {
            let message = e.to_string();
            let c = if message.contains("could not be run") {
                check(State::Missing, format!("{program} is not installed or not on PATH"))
            } else {
                check(State::Broken, message.trim_start_matches("not supported: ").to_string())
            };
            (c, String::new())
        }
        Ok((status, out, err)) => {
            let (out, err) = (String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
            let text = format!("{out}\n{err}");
            let c = if status.success() {
                check(
                    State::Ok,
                    out.lines().chain(err.lines()).find(|l| !l.trim().is_empty()).unwrap_or("").trim().into(),
                )
            } else if process::library_failure(&err) {
                let line = err
                    .lines()
                    .find(|l| l.contains("symbol lookup error") || l.contains("error while loading"))
                    .or_else(|| err.lines().next())
                    .unwrap_or("");
                check(
                    State::Broken,
                    format!("could not start: it loaded libraries that do not fit it ({})", line.trim()),
                )
            } else {
                let line = err.lines().chain(out.lines()).find(|l| !l.trim().is_empty()).unwrap_or("");
                check(State::Broken, if line.is_empty() { status.to_string() } else { line.trim().to_string() })
            };
            (c, text)
        }
    }
}

/// `git --version`.
pub fn git() -> Check {
    run("git", "git", &["--version"], Duration::from_secs(20))
}

/// `git ls-remote` against `url`: git's HTTPS or SSH transport, which loads more
/// libraries than `git --version` (libcurl and its dependencies for HTTPS).
pub fn git_remote(url: &str) -> Check {
    let mut c = run("git ls-remote", "git", &["ls-remote", "--heads", "--", url], Duration::from_secs(60));
    if c.ok() {
        c.detail = format!("reached {url}");
    }
    c
}

/// `gh --version`.
pub fn gh() -> Check {
    run("gh", "gh", &["--version"], Duration::from_secs(20))
}

/// Who `gh` is signed in as on `host`, or why it is not.
pub fn gh_account(host: &str) -> Check {
    let (mut c, text) = run_text("gh auth", "gh", &["auth", "status", "--hostname", host], Duration::from_secs(20));
    match c.state {
        State::Ok => match account(&text) {
            Some(login) => c.detail = login,
            None => c.detail = String::new(),
        },
        State::Broken if !c.detail.contains("could not start") => {
            c.detail = format!("gh is not signed in to {host}: run gh auth login --hostname {host}");
        }
        _ => {}
    }
    c
}

/// The account in `gh auth status` output: "Logged in to github.com account NAME (keyring)"
/// in newer versions, "Logged in to github.com as NAME (oauth_token)" in older ones.
fn account(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some(rest) = line.split("Logged in to ").nth(1) else { continue };
        let rest = rest.split_once(' ').map(|(_, r)| r).unwrap_or("");
        let name = rest.strip_prefix("account ").or_else(|| rest.strip_prefix("as "));
        if let Some(name) = name.and_then(|n| n.split_whitespace().next()) {
            return Some(name.to_string());
        }
    }
    None
}

/// The opener for links and files on Linux (`xdg-open --version`).
pub fn opener() -> Check {
    run("xdg-open", "xdg-open", &["--version"], Duration::from_secs(20))
}

/// The self-test of the packaged app: each check, whether it must pass, and
/// whether all that must pass did. `remote` is an HTTPS repository to reach.
pub fn self_test(remote: Option<&str>) -> (Vec<(Check, bool)>, bool) {
    let mut checks = vec![(git(), true), (gh(), true)];
    if let Some(url) = remote {
        checks.push((git_remote(url), true));
    }
    if cfg!(target_os = "linux") {
        // Only desktops need it; a missing one is reported, not failed.
        let open = opener();
        let required = open.state != State::Missing;
        checks.push((open, required));
    }
    let passed = checks.iter().all(|(c, required)| c.ok() || !required);
    (checks, passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_account_of_old_and_new_gh() {
        let new = "github.com\n  ✓ Logged in to github.com account erpel-dev (keyring)\n  - Active account: true";
        let old = "github.com\n  ✓ Logged in to github.com as octocat (oauth_token)\n";
        assert_eq!(account(new).as_deref(), Some("erpel-dev"));
        assert_eq!(account(old).as_deref(), Some("octocat"));
        assert_eq!(account("You are not logged into any GitHub hosts."), None);
    }

    #[test]
    fn missing_and_failing_programs_are_told_apart() {
        let missing = run("x", "parallax-no-such-program-4821", &[], Duration::from_secs(5));
        assert_eq!(missing.state, State::Missing);
        if git().ok() {
            assert!(git().detail.starts_with("git version"));
            let failing = run("git", "git", &["parallax-no-such-command"], Duration::from_secs(5));
            assert_eq!(failing.state, State::Broken);
            assert!(failing.detail.contains("parallax-no-such-command"), "{}", failing.detail);
        }
    }
}
