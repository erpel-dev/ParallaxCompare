//! Read-only GitHub PRs: resolve a pinned merge-base/head pair through `gh`,
//! then fetch Git objects into the existing bare cache without a checkout.

use std::{process::Command, time::Duration};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Error, Result, source::SourceSpec};

#[derive(Debug, PartialEq, Eq)]
struct PullRequestUrl {
    host: String,
    owner: String,
    repo: String,
    number: u64,
    url: String,
}

impl PullRequestUrl {
    fn parse(input: &str) -> Result<Self> {
        let mut url = Url::parse(input.trim()).map_err(|_| bad("expected a GitHub pull request URL"))?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.host_str().is_none()
        {
            return Err(bad("use an HTTPS pull request URL without a username or password"));
        }
        let parts: Vec<_> = url.path_segments().into_iter().flatten().filter(|s| !s.is_empty()).collect();
        let word = |s: &str| {
            !matches!(s, "" | "." | "..") && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        };
        if parts.len() < 4
            || !word(parts[0])
            || !word(parts[1])
            || parts[2] != "pull"
            || (parts.len() > 4 && !matches!(parts[4], "files" | "commits" | "checks" | "changes" | "overview"))
        {
            return Err(bad("expected https://HOST/OWNER/REPO/pull/NUMBER"));
        }
        if !parts[3].bytes().all(|c| c.is_ascii_digit()) {
            return Err(bad("invalid pull request number"));
        }
        let number =
            parts[3].parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| bad("invalid pull request number"))?;
        let (owner, repo) = (parts[0].to_string(), parts[1].to_string());
        let host = url[url::Position::BeforeHost..url::Position::AfterPort].to_string();
        url.set_path(&format!("/{owner}/{repo}/pull/{number}"));
        url.set_query(None);
        url.set_fragment(None);
        Ok(Self { host, owner, repo, number, url: url.into() })
    }

    fn endpoint(&self) -> String {
        format!("repos/{}/{}", self.owner, self.repo)
    }

    fn access_help(&self) -> String {
        format!(
            "Check repository access and run `gh auth login --hostname {}`. Git fetches also need SSH access or an HTTPS credential helper (`gh auth setup-git --hostname {}`).",
            self.host, self.host
        )
    }
}

/// Both sources are immutable Git commit IDs. Reopening these sources keeps the
/// snapshot; resolving the PR URL again obtains its current head and merge base.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestComparison {
    pub url: String,
    pub number: u64,
    pub title: String,
    pub left: SourceSpec,
    pub right: SourceSpec,
}

#[derive(Deserialize)]
struct Pull {
    number: u64,
    title: String,
    base: Branch,
    head: Branch,
}

#[derive(Deserialize)]
struct Branch {
    sha: String,
    repo: Option<Repository>,
}

#[derive(Deserialize)]
struct Repository {
    clone_url: String,
    #[serde(default)]
    ssh_url: String,
}

#[derive(Deserialize)]
struct Comparison {
    merge_base_commit: Commit,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
}

fn bad(message: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("GitHub PR: {message}"))
}

fn commit_id(sha: &str) -> Result<()> {
    if sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(bad("the server returned an invalid Git commit ID"))
    }
}

fn gh(args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).env("GH_PROMPT_DISABLED", "1").env("GH_NO_UPDATE_NOTIFIER", "1");
    crate::process::run(&mut cmd, "gh", "gh request", Duration::from_secs(60))
}

/// Resolve only on explicit user action; no authentication or working-tree
/// configuration is changed. Host selection is explicit on every gh invocation.
pub fn resolve(input: &str) -> Result<PullRequestComparison> {
    let pr = PullRequestUrl::parse(input)?;
    let result = resolve_with(
        &pr,
        &mut |endpoint| {
            let json = gh(&["api", "--hostname", &pr.host, "--method", "GET", endpoint])
                .map_err(|e| bad(format!("cannot read {} on {}: {e}. {}", pr.endpoint(), pr.host, pr.access_help())))?;
            serde_json::from_str(&json).map_err(|e| bad(format!("invalid API response: {e}")))
        },
        &mut |url, rev| {
            let (dir, commit) = crate::remote::fetch(url, rev)?;
            crate::remote::pin_commit(&dir, &commit)?;
            Ok(commit)
        },
        &mut || gh(&["config", "get", "git_protocol", "--host", &pr.host]).is_ok_and(|v| v.trim() == "ssh"),
    );
    result.map_err(|e| bad(format!("cannot open {}: {e}", pr.url)))
}

fn resolve_with(
    pr: &PullRequestUrl,
    api: &mut dyn FnMut(&str) -> Result<serde_json::Value>,
    fetch: &mut dyn FnMut(&str, &str) -> Result<String>,
    prefer_ssh: &mut dyn FnMut() -> bool,
) -> Result<PullRequestComparison> {
    let pull: Pull = serde_json::from_value(api(&format!("{}/pulls/{}", pr.endpoint(), pr.number))?)
        .map_err(|e| bad(format!("invalid pull request response: {e}")))?;
    if pull.number != pr.number {
        return Err(bad("the server returned a different pull request"));
    }
    commit_id(&pull.base.sha)?;
    commit_id(&pull.head.sha)?;
    let repository = pull.base.repo.ok_or_else(|| bad("the target repository is unavailable"))?;
    let url = Url::parse(&repository.clone_url).map_err(|_| bad("invalid target clone URL"))?;
    // A PR from a fork can still be fetched from its target repository's PR ref,
    // including when head.repo is null because the fork was deleted.
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url[url::Position::BeforeHost..url::Position::AfterPort] != pr.host
    {
        return Err(bad("the target clone URL is not on the requested GitHub host"));
    }
    let clone = if prefer_ssh() {
        // GitHub's SSH hostname can legitimately differ from its HTTPS hostname.
        if !(repository.ssh_url.starts_with("git@") || repository.ssh_url.starts_with("ssh://"))
            || !crate::remote::is_remote(&repository.ssh_url)
            || repository.ssh_url.chars().any(char::is_control)
        {
            return Err(bad("the server did not return a valid SSH clone URL"));
        }
        repository.ssh_url
    } else {
        repository.clone_url
    };
    let comparison: Comparison = serde_json::from_value(api(&format!(
        "{}/compare/{}...{}?per_page=1",
        pr.endpoint(),
        pull.base.sha,
        pull.head.sha
    ))?)
    .map_err(|e| bad(format!("invalid merge-base response: {e}")))?;
    let base = comparison.merge_base_commit.sha;
    commit_id(&base)?;
    let access_error = |role: &str, error: Error| {
        bad(format!("Git cannot fetch the {role} from {clone}: {error}. {}", pr.access_help()))
    };
    let fetched = fetch(&clone, &base).map_err(|e| access_error("merge base", e))?;
    if fetched != base {
        return Err(bad("Git fetched a different merge-base commit"));
    }
    let fetched = match fetch(&clone, &pull.head.sha) {
        Ok(commit) => commit,
        Err(first) => fetch(&clone, &format!("refs/pull/{}/head", pr.number))
            .map_err(|fallback| access_error("PR head", bad(format!("{first}; PR ref fallback: {fallback}"))))?,
    };
    if fetched != pull.head.sha {
        return Err(bad("the PR head changed while opening it. Paste the PR URL again to load the new snapshot"));
    }
    let source = |rev| SourceSpec::GitRev { repo: clone.clone(), rev, subpath: String::new() };
    Ok(PullRequestComparison {
        url: pr.url.clone(),
        number: pr.number,
        title: pull.title,
        left: source(base),
        right: source(pull.head.sha),
    })
}

// ---------------------------------------------------------------------------
// The conversation of a pull request: reviews, comments, review threads with
// their lines, and the annotations CI left on the head commit.

/// Everything said about a pull request, as far as `gh` may read it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub url: String,
    pub number: u64,
    pub title: String,
    pub author: Option<String>,
    /// `OPEN`, `MERGED` or `CLOSED`.
    pub state: String,
    /// The head the pull request has now; `pinned_head` is the one compared.
    pub head: String,
    pub pinned_head: String,
    pub reviews: Vec<Review>,
    /// Comments on the pull request as a whole.
    pub comments: Vec<Note>,
    pub threads: Vec<Thread>,
    pub annotations: Vec<Annotation>,
    pub checks: Checks,
    /// Some reviews, comments or threads were left out (more than the app reads).
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub author: Option<String>,
    pub bot: bool,
    /// Markdown, as written: shown without raw HTML.
    pub body: String,
    pub created: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub author: Option<String>,
    pub bot: bool,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED` or `DISMISSED`.
    pub state: String,
    pub body: String,
    pub submitted: Option<String>,
    pub url: String,
}

/// A review thread: comments on one place of one file.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub path: String,
    /// `left` (the merge base) or `right` (the head).
    pub side: &'static str,
    /// 1-based lines on the current head's diff; None when outdated or on the file.
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    /// Where it was made, for outdated threads.
    pub original_line: Option<u32>,
    pub resolved: bool,
    pub outdated: bool,
    /// On the file as a whole, not on lines.
    pub file_level: bool,
    /// "Important", "Nit" or "Pre-existing", from a marker a review bot puts first.
    pub severity: Option<&'static str>,
    pub comments: Vec<Note>,
}

/// A finding CI attached to a line of the head commit (lint, compiler, analyzers).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    /// `failure`, `warning` or `notice`.
    pub level: String,
    pub title: Option<String>,
    pub message: String,
    /// The check that reported it.
    pub check: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Checks {
    pub passed: u32,
    pub failed: u32,
    pub pending: u32,
}

/// GraphQL pages of 100 review threads read at most.
const THREAD_PAGES: u32 = 20;
/// Check runs whose annotations are read at most.
const ANNOTATED_RUNS: usize = 30;

const CONVERSATION_QUERY: &str = r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      number title state url headRefOid
      author { login __typename }
      reviews(first: 100) { pageInfo { hasNextPage } nodes { author { login __typename } state body submittedAt url } }
      comments(first: 100) { pageInfo { hasNextPage } nodes { author { login __typename } body createdAt url } }
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          path line startLine originalLine diffSide isResolved isOutdated subjectType
          comments(first: 100) { pageInfo { hasNextPage } nodes { author { login __typename } body createdAt url } }
        }
      }
    }
  }
}"#;

/// The conversation of the pull request at `url`, whose comparison shows `pinned_head`.
/// Read only on request; nothing is changed on the server.
pub fn conversation(url: &str, pinned_head: &str) -> Result<Conversation> {
    let pr = PullRequestUrl::parse(url)?;
    commit_id(pinned_head)?;
    let help = pr.access_help();
    let number = pr.number.to_string();
    let result = conversation_with(
        &pr,
        pinned_head,
        &mut |after| {
            let query = format!("query={CONVERSATION_QUERY}");
            let (owner, repo, number) =
                (format!("owner={}", pr.owner), format!("repo={}", pr.repo), format!("number={number}"));
            let after = after.map(|a| format!("after={a}"));
            let mut args =
                vec!["api", "--hostname", &pr.host, "graphql", "-f", &query, "-f", &owner, "-f", &repo, "-F", &number];
            if let Some(a) = &after {
                args.extend(["-f", a.as_str()]);
            }
            let json = gh(&args).map_err(|e| bad(format!("cannot read the comments: {e}. {help}")))?;
            serde_json::from_str(&json).map_err(|e| bad(format!("invalid API response: {e}")))
        },
        &mut |endpoint| {
            let json = gh(&["api", "--hostname", &pr.host, "--method", "GET", endpoint])
                .map_err(|e| bad(format!("cannot read {endpoint}: {e}")))?;
            serde_json::from_str(&json).map_err(|e| bad(format!("invalid API response: {e}")))
        },
    );
    result.map_err(|e| bad(format!("cannot read the conversation of {}: {e}", pr.url)))
}

fn conversation_with(
    pr: &PullRequestUrl,
    pinned_head: &str,
    graphql: &mut dyn FnMut(Option<&str>) -> Result<serde_json::Value>,
    rest: &mut dyn FnMut(&str) -> Result<serde_json::Value>,
) -> Result<Conversation> {
    use serde_json::Value;
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let author = |v: &Value| -> (Option<String>, bool) {
        let a = v.get("author").filter(|a| !a.is_null());
        let login = a.and_then(|a| a.get("login")).and_then(Value::as_str).map(str::to_string);
        let bot = a.and_then(|a| a.get("__typename")).and_then(Value::as_str) == Some("Bot")
            || login.as_deref().is_some_and(|l| l.ends_with("[bot]"));
        (login, bot)
    };
    let note = |v: &Value, created: &str| {
        let (author, bot) = author(v);
        Note { author, bot, body: text(v, "body"), created: text(v, created), url: text(v, "url") }
    };
    let nodes = |v: &Value| v.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let more = |v: &Value| v.pointer("/pageInfo/hasNextPage").and_then(Value::as_bool).unwrap_or(false);

    let mut after: Option<String> = None;
    let mut conv: Option<Conversation> = None;
    let mut threads = Vec::new();
    let mut truncated = false;
    for page in 0..THREAD_PAGES {
        let data = graphql(after.as_deref())?;
        if let Some(errors) = data.get("errors").and_then(Value::as_array).filter(|e| !e.is_empty()) {
            let message = errors.iter().filter_map(|e| e.get("message").and_then(Value::as_str)).collect::<Vec<_>>();
            return Err(bad(message.join("; ")));
        }
        let pull = data
            .pointer("/data/repository/pullRequest")
            .filter(|p| !p.is_null())
            .ok_or_else(|| bad("the pull request was not found"))?;
        if conv.is_none() {
            if pull.get("number").and_then(Value::as_u64) != Some(pr.number) {
                return Err(bad("the server returned a different pull request"));
            }
            let head = text(pull, "headRefOid");
            commit_id(&head)?;
            let reviews = pull.get("reviews").cloned().unwrap_or_default();
            let comments = pull.get("comments").cloned().unwrap_or_default();
            truncated |= more(&reviews) || more(&comments);
            conv = Some(Conversation {
                url: pr.url.clone(),
                number: pr.number,
                title: text(pull, "title"),
                author: author(pull).0,
                state: text(pull, "state"),
                head,
                pinned_head: pinned_head.to_string(),
                reviews: nodes(&reviews)
                    .iter()
                    .filter(|r| text(r, "state") != "PENDING")
                    .map(|r| {
                        let (author, bot) = author(r);
                        let submitted = r.get("submittedAt").and_then(Value::as_str).map(str::to_string);
                        Review {
                            author,
                            bot,
                            state: text(r, "state"),
                            body: text(r, "body"),
                            submitted,
                            url: text(r, "url"),
                        }
                    })
                    .collect(),
                comments: nodes(&comments).iter().map(|c| note(c, "createdAt")).collect(),
                threads: Vec::new(),
                annotations: Vec::new(),
                checks: Checks::default(),
                truncated: false,
            });
        }
        let page_threads = pull.get("reviewThreads").cloned().unwrap_or_default();
        for t in nodes(&page_threads) {
            let line = |k: &str| t.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
            let comments = t.get("comments").cloned().unwrap_or_default();
            truncated |= more(&comments);
            let comments: Vec<Note> = nodes(&comments).iter().map(|c| note(c, "createdAt")).collect();
            let severity = comments.first().and_then(|c| severity(&c.body));
            let file_level = text(&t, "subjectType") == "FILE";
            let outdated = t.get("isOutdated").and_then(Value::as_bool).unwrap_or(false);
            threads.push(Thread {
                path: text(&t, "path"),
                side: if text(&t, "diffSide") == "LEFT" { "left" } else { "right" },
                line: if outdated { None } else { line("line") },
                start_line: if outdated { None } else { line("startLine") },
                original_line: line("originalLine"),
                resolved: t.get("isResolved").and_then(Value::as_bool).unwrap_or(false),
                outdated,
                file_level,
                severity,
                comments,
            });
        }
        let next = page_threads.pointer("/pageInfo/endCursor").and_then(Value::as_str).map(str::to_string);
        if !more(&page_threads) || next.is_none() {
            break;
        }
        if page + 1 == THREAD_PAGES {
            truncated = true;
        }
        after = next;
    }
    let mut conv = conv.ok_or_else(|| bad("no response"))?;
    conv.threads = threads;

    // Checks and their annotations, on the head that is compared. CI that cannot
    // be read (no permission, no checks) leaves the rest of the conversation.
    if let Ok(runs) = rest(&format!("{}/commits/{pinned_head}/check-runs?per_page=100", pr.endpoint())) {
        let runs = runs.get("check_runs").and_then(Value::as_array).cloned().unwrap_or_default();
        for run in &runs {
            let status = text(run, "status");
            match (status.as_str(), run.get("conclusion").and_then(Value::as_str)) {
                ("completed", Some("success" | "neutral" | "skipped")) => conv.checks.passed += 1,
                ("completed", _) => conv.checks.failed += 1,
                _ => conv.checks.pending += 1,
            }
        }
        let annotated =
            runs.iter().filter(|r| r.pointer("/output/annotations_count").and_then(Value::as_u64).unwrap_or(0) > 0);
        for run in annotated.take(ANNOTATED_RUNS) {
            let Some(id) = run.get("id").and_then(Value::as_u64) else { continue };
            let Ok(list) = rest(&format!("{}/check-runs/{id}/annotations?per_page=100", pr.endpoint())) else {
                continue;
            };
            for a in list.as_array().into_iter().flatten() {
                let line = |k: &str| a.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
                let path = text(a, "path");
                if path.is_empty() || line("start_line") == 0 {
                    continue;
                }
                conv.annotations.push(Annotation {
                    path,
                    start_line: line("start_line"),
                    end_line: line("end_line").max(line("start_line")),
                    level: text(a, "annotation_level"),
                    title: a.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string),
                    message: text(a, "message"),
                    check: text(run, "name"),
                    url: run.get("html_url").and_then(Value::as_str).map(str::to_string),
                });
            }
        }
    }
    conv.truncated = truncated;
    Ok(conv)
}

/// The severity a review bot marks a finding with, at the start of its comment.
fn severity(body: &str) -> Option<&'static str> {
    let start = body.trim_start().trim_start_matches(['*', '_', '#', ' ', '>']);
    let lower = start.chars().take(24).collect::<String>().to_lowercase();
    if start.starts_with('🔴')
        || start.starts_with('🟠')
        || lower.starts_with("important")
        || lower.starts_with("[p0]")
        || lower.starts_with("[p1]")
    {
        Some("Important")
    } else if start.starts_with('🟡')
        || lower.starts_with("nit")
        || lower.starts_with("[p2]")
        || lower.starts_with("[p3]")
    {
        Some("Nit")
    } else if start.starts_with('🟣') || lower.starts_with("pre-existing") {
        Some("Pre-existing")
    } else {
        None
    }
}

/// The link to lines of a file on the pull request's *Files changed* page, where
/// GitHub offers to comment on them: `…/pull/N/files#diff-<sha256 of path>R12-R14`.
/// `side` is `left` (the merge base, `L`) or `right` (the head, `R`); lines are 1-based.
pub fn line_link(url: &str, path: &str, side: &str, start: u32, end: u32) -> Result<String> {
    use sha2::{Digest, Sha256};
    let pr = PullRequestUrl::parse(url)?;
    if path.is_empty() || start == 0 {
        return Err(bad("a line link needs a file and a line"));
    }
    let hash: String = Sha256::digest(path.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let s = if side == "left" { 'L' } else { 'R' };
    let (start, end) = (start.min(end.max(start)), end.max(start));
    let lines = if end > start { format!("{s}{start}-{s}{end}") } else { format!("{s}{start}") };
    Ok(format!("{}/files#diff-{hash}{lines}", pr.url))
}

/// Whether `link` may be opened for the pull request at `url`: HTTPS on its host.
pub fn link_allowed(url: &str, link: &str) -> bool {
    let (Ok(pr), Ok(link)) = (PullRequestUrl::parse(url), Url::parse(link)) else { return false };
    link.scheme() == "https"
        && link.username().is_empty()
        && link.password().is_none()
        && link.host_str().is_some()
        && link[url::Position::BeforeHost..url::Position::AfterPort] == pr.host
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const TARGET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn pr() -> PullRequestUrl {
        PullRequestUrl::parse("https://github.company.example/team/project/pull/42").unwrap()
    }
    fn metadata() -> Value {
        json!({"number":42,"title":"Change calibration", "base":{"sha":TARGET,"repo":{
            "clone_url":"https://github.company.example/team/project.git",
            "ssh_url":"git@ssh.company.example:team/project.git"
        }}, "head":{"sha":HEAD,"repo":null}})
    }
    fn api(endpoint: &str) -> Result<Value> {
        if endpoint.ends_with("/pulls/42") {
            Ok(metadata())
        } else {
            assert_eq!(endpoint, format!("repos/team/project/compare/{TARGET}...{HEAD}?per_page=1"));
            Ok(json!({"merge_base_commit":{"sha":BASE}}))
        }
    }

    #[test]
    fn conversation_with_threads_reviews_and_annotations() {
        let who = |login: &str, kind: &str| json!({"login": login, "__typename": kind});
        let thread = |path: &str, line: Value, side: &str, outdated: bool, body: &str, author: Value| {
            json!({"path": path, "line": line, "startLine": null, "originalLine": 7, "diffSide": side,
                   "isResolved": false, "isOutdated": outdated, "subjectType": "LINE",
                   "comments": {"pageInfo": {"hasNextPage": false}, "nodes": [
                       {"author": author, "body": body, "createdAt": "2026-11-12T10:00:00Z", "url": "https://github.company.example/c/1"}]}})
        };
        let page = |threads: Vec<Value>, next: Option<&str>| {
            json!({"data": {"repository": {"pullRequest": {
                "number": 42, "title": "Change calibration", "state": "OPEN", "url": "x", "headRefOid": HEAD,
                "author": who("alice", "User"),
                "reviews": {"pageInfo": {"hasNextPage": false}, "nodes": [
                    {"author": who("bob", "User"), "state": "APPROVED", "body": "", "submittedAt": "2026-11-12T11:00:00Z", "url": "u"},
                    {"author": who("carol", "User"), "state": "PENDING", "body": "", "submittedAt": null, "url": "u"}]},
                "comments": {"pageInfo": {"hasNextPage": false}, "nodes": [
                    {"author": null, "body": "Thanks!", "createdAt": "2026-11-12T09:00:00Z", "url": "u"}]},
                "reviewThreads": {"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": threads}
            }}}})
        };
        let mut pages = vec![
            page(
                vec![thread(
                    "src/a.rs",
                    json!(12),
                    "RIGHT",
                    false,
                    "🔴 **Important**: unwrap_or(0) hides errors",
                    who("claude", "Bot"),
                )],
                Some("c1"),
            ),
            page(vec![thread("src/b.rs", json!(null), "LEFT", true, "Old remark", who("bob", "User"))], None),
        ]
        .into_iter();
        let mut afters = Vec::new();
        let conv = conversation_with(
            &pr(),
            HEAD,
            &mut |after| {
                afters.push(after.map(str::to_string));
                Ok(pages.next().unwrap())
            },
            &mut |endpoint| {
                if endpoint.ends_with("/check-runs?per_page=100") {
                    assert_eq!(endpoint, format!("repos/team/project/commits/{HEAD}/check-runs?per_page=100"));
                    Ok(json!({"check_runs": [
                        {"id": 7, "name": "clippy", "status": "completed", "conclusion": "failure", "html_url": "https://github.company.example/r/7", "output": {"annotations_count": 1}},
                        {"id": 8, "name": "test", "status": "in_progress", "conclusion": null, "output": {"annotations_count": 0}}]}))
                } else {
                    assert_eq!(endpoint, "repos/team/project/check-runs/7/annotations?per_page=100");
                    Ok(json!([{"path": "src/a.rs", "start_line": 20, "end_line": 20, "annotation_level": "warning",
                               "title": "", "message": "too many arguments"}]))
                }
            },
        )
        .unwrap();
        assert_eq!(afters, [None, Some("c1".to_string())]);
        assert_eq!(
            (conv.title.as_str(), conv.author.as_deref(), conv.head.as_str()),
            ("Change calibration", Some("alice"), HEAD)
        );
        // Pending reviews are drafts of their author, not shown.
        assert_eq!(conv.reviews.len(), 1);
        assert_eq!(conv.reviews[0].state, "APPROVED");
        assert_eq!(conv.comments[0].author, None);
        assert_eq!(conv.threads.len(), 2);
        let (a, b) = (&conv.threads[0], &conv.threads[1]);
        assert_eq!((a.side, a.line, a.severity, a.comments[0].bot), ("right", Some(12), Some("Important"), true));
        // Outdated threads keep where they were made, not a current line.
        assert_eq!((b.side, b.line, b.original_line, b.outdated), ("left", None, Some(7), true));
        assert_eq!(conv.checks, Checks { passed: 0, failed: 1, pending: 1 });
        assert_eq!(conv.annotations.len(), 1);
        assert_eq!((conv.annotations[0].check.as_str(), conv.annotations[0].title.as_deref()), ("clippy", None));
        assert!(!conv.truncated);
    }

    #[test]
    fn conversation_errors_and_links() {
        let err = conversation_with(
            &pr(),
            HEAD,
            &mut |_| Ok(json!({"errors": [{"message": "Could not resolve to a Repository"}]})),
            &mut |_| unreachable!(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Could not resolve"), "{err}");
        assert_eq!(severity("  **🟡 Nit:** naming"), Some("Nit"));
        assert_eq!(severity("[P1] crash on empty input"), Some("Important"));
        assert_eq!(severity("Looks good"), None);
        let url = "https://github.company.example/team/project/pull/42";
        assert!(link_allowed(url, "https://github.company.example/team/project/pull/42#discussion_r1"));
        assert!(!link_allowed(url, "https://evil.example/x"));
        assert!(!link_allowed(url, "http://github.company.example/x"));
        assert!(!link_allowed(url, "https://user@github.company.example/x"));
        // GitHub's anchor: the SHA-256 of the path, then the side and lines.
        assert_eq!(
            line_link("https://github.com/o/r/pull/7/files", "README.md", "right", 12, 14).unwrap(),
            "https://github.com/o/r/pull/7/files#diff-b335630551682c19a781afebcf4d07bf978fb1f8ac04c6bf87428ed5106870f5R12-R14"
        );
        assert!(line_link(url, "a.rs", "left", 3, 3).unwrap().ends_with("L3"));
        assert!(line_link(url, "", "left", 3, 3).is_err());
    }

    #[test]
    fn github_and_enterprise_links_are_canonical_and_host_specific() {
        for host in ["github.com", "github.company.example", "company.ghe.com", "ghe.example:8443"] {
            for suffix in ["", "/", "/files#diff-abc", "/commits/abcdef?tab=files", "?foo=bar#top"] {
                let url = PullRequestUrl::parse(&format!(" https://{host}/team/project/pull/42{suffix} ")).unwrap();
                assert_eq!(url.host, host);
                assert_eq!(url.url, format!("https://{host}/team/project/pull/42"));
                assert_eq!(url.endpoint(), "repos/team/project");
            }
        }
        for invalid in [
            "https://github.com/team/project",
            "https://github.com/team/project/pull/0",
            "https://github.com/team/project/pull/+42",
            "https://github.com/team/project/pull/no",
            "https://github.com/team/project/pull/18446744073709551616",
            "https://github.com/team/project/pull/42/unknown",
            "https://github.com/team/project/pulls/42",
            "https://user:secret@github.com/team/project/pull/42",
            "http://ghe.example/team/project/pull/42",
            "https://github.com/team/project%2fother/pull/42",
            "file:///team/project/pull/42",
        ] {
            assert!(PullRequestUrl::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn uses_actual_merge_base_and_pinned_head_even_with_deleted_fork() {
        let mut fetched = Vec::new();
        let result = resolve_with(
            &pr(),
            &mut api,
            &mut |url, rev| {
                fetched.push((url.to_string(), rev.to_string()));
                Ok(rev.to_string())
            },
            &mut || false,
        )
        .unwrap();
        let expected = |sha: &str| SourceSpec::GitRev {
            repo: "https://github.company.example/team/project.git".into(),
            rev: sha.into(),
            subpath: String::new(),
        };
        assert_eq!(result.left, expected(BASE));
        assert_ne!(result.left, expected(TARGET));
        assert_eq!(result.right, expected(HEAD));
        assert_eq!(fetched.iter().map(|(_, rev)| rev.as_str()).collect::<Vec<_>>(), [BASE, HEAD]);
        assert!(fetched.iter().all(|(url, _)| url == "https://github.company.example/team/project.git"));
        assert_eq!(result.title, "Change calibration");
    }

    #[test]
    fn respects_the_hosts_ssh_protocol_preference() {
        let result = resolve_with(
            &pr(),
            &mut api,
            &mut |url, rev| {
                assert_eq!(url, "git@ssh.company.example:team/project.git");
                Ok(rev.into())
            },
            &mut || true,
        )
        .unwrap();
        assert!(matches!(result.right, SourceSpec::GitRev { repo, .. } if repo.starts_with("git@")));
    }

    #[test]
    fn head_ref_fallback_must_match_the_resolved_snapshot() {
        for moved in [false, true] {
            let mut refs = Vec::new();
            let result = resolve_with(
                &pr(),
                &mut api,
                &mut |_, rev| {
                    refs.push(rev.to_string());
                    if rev == HEAD {
                        Err(bad("unadvertised object"))
                    } else if rev == BASE {
                        Ok(BASE.into())
                    } else {
                        Ok(if moved { TARGET } else { HEAD }.into())
                    }
                },
                &mut || false,
            );
            assert_eq!(refs, [BASE, HEAD, "refs/pull/42/head"]);
            if moved {
                assert!(result.unwrap_err().to_string().contains("changed while opening"));
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[test]
    fn fetch_errors_name_the_repo_and_explain_git_authentication() {
        let error = resolve_with(&pr(), &mut api, &mut |_, _| Err(bad("Permission denied")), &mut || false)
            .unwrap_err()
            .to_string();
        for text in [
            "merge base",
            "https://github.company.example/team/project.git",
            "Permission denied",
            "gh auth setup-git --hostname github.company.example",
        ] {
            assert!(error.contains(text), "{error}");
        }
        let mut count = 0;
        let error = resolve_with(
            &pr(),
            &mut api,
            &mut |_, rev| {
                count += 1;
                if rev == BASE { Ok(BASE.into()) } else { Err(bad("Git repository not found")) }
            },
            &mut || false,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(count, 3);
        assert!(error.contains("PR head") && error.contains("Git repository not found"));
    }

    #[test]
    fn api_errors_and_invalid_metadata_do_not_fetch() {
        let no_fetch = &mut |_: &str, _: &str| -> Result<String> { panic!("must not fetch") };
        let error = resolve_with(&pr(), &mut |_| Err(bad("HTTP 403: not authorized")), no_fetch, &mut || false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("403"));
        for data in [
            json!({}),
            {
                let mut p = metadata();
                p["number"] = json!(99);
                p
            },
            {
                let mut p = metadata();
                p["head"]["sha"] = json!("--upload-pack=bad");
                p
            },
            {
                let mut p = metadata();
                p["base"]["repo"] = Value::Null;
                p
            },
            {
                let mut p = metadata();
                p["base"]["repo"]["clone_url"] = json!("https://another.example/team/project.git");
                p
            },
            {
                let mut p = metadata();
                p["base"]["repo"]["clone_url"] = json!("https://user:secret@github.company.example/team/project.git");
                p
            },
        ] {
            assert!(resolve_with(&pr(), &mut |_| Ok(data.clone()), no_fetch, &mut || false).is_err());
        }
        assert!(
            resolve_with(
                &pr(),
                &mut |ep| {
                    if ep.contains("/pulls/") { Ok(metadata()) } else { Ok(json!({"merge_base_commit":{"sha":"bad"}})) }
                },
                no_fetch,
                &mut || false
            )
            .is_err()
        );
    }
}
