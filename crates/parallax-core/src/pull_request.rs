//! Read-only GitHub PRs: resolve a pinned merge-base/head pair through `gh`,
//! then fetch Git objects into the existing bare cache without a checkout.

use std::{collections::BTreeMap, time::Duration};

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
    Error::Unsupported(format!("GitHub PR: {}", plain(&message.to_string())))
}

/// A message without the prefixes of the errors it went through ("not supported: GitHub PR: …").
fn plain(message: &str) -> String {
    message
        .replace(": not supported: GitHub PR: ", ": ")
        .replace(": not supported: ", ": ")
        .trim_start_matches("not supported: ")
        .trim_start_matches("GitHub PR: ")
        .to_string()
}

/// Sign-in advice, unless the program could not start at all (then it would mislead).
fn advice(error: &impl std::fmt::Display, help: &str) -> String {
    if error.to_string().contains("could not start because") { String::new() } else { format!(" {help}") }
}

fn commit_id(sha: &str) -> Result<()> {
    if sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(bad("the server returned an invalid Git commit ID"))
    }
}

fn gh(args: &[&str]) -> Result<String> {
    let mut cmd = crate::process::command("gh");
    cmd.args(args).env("GH_PROMPT_DISABLED", "1").env("GH_NO_UPDATE_NOTIFIER", "1");
    crate::process::run(&mut cmd, "gh", "gh request", Duration::from_secs(60))
}

/// Whether git and gh work for a pull request on its host, before it is resolved.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tools {
    pub host: String,
    pub git: crate::tools::Check,
    pub gh: crate::tools::Check,
    /// Who gh is signed in as on the host; None while gh itself does not work.
    pub account: Option<crate::tools::Check>,
}

/// Checks git and gh as the app starts them, and gh's sign-in for the URL's host.
/// Nothing is sent to the host.
pub fn tools(url: &str) -> Result<Tools> {
    let pr = PullRequestUrl::parse(url)?;
    let (git, gh) = (crate::tools::git(), crate::tools::gh());
    let account = gh.ok().then(|| crate::tools::gh_account(&pr.host));
    Ok(Tools { host: pr.host, git, gh, account })
}

/// Resolve only on explicit user action; no authentication or working-tree
/// configuration is changed. Host selection is explicit on every gh invocation.
pub fn resolve(input: &str) -> Result<PullRequestComparison> {
    let pr = PullRequestUrl::parse(input)?;
    let result = resolve_with(
        &pr,
        &mut |endpoint| {
            let json = gh(&["api", "--hostname", &pr.host, "--method", "GET", endpoint]).map_err(|e| {
                bad(format!("cannot read {} on {}: {e}.{}", pr.endpoint(), pr.host, advice(&e, &pr.access_help())))
            })?;
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
        bad(format!("Git cannot fetch the {role} from {clone}: {error}.{}", advice(&error, &pr.access_help())))
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
    /// The pull request's GraphQL node ID, for starting a review.
    pub id: String,
    /// Who `gh` is signed in as.
    pub viewer: Option<String>,
    /// Whether the viewer may comment (signed in, and the conversation not locked to them).
    pub can_comment: bool,
    /// The viewer opened the pull request (GitHub refuses their approval).
    pub own: bool,
    /// The viewer's pending review, from the app or the browser.
    pub review: Option<PendingReview>,
    /// Lines GitHub accepts comments on, per changed file; read only while the
    /// compared head is the current one. A file with no ranges takes file comments only.
    pub files: BTreeMap<String, Commentable>,
}

/// The viewer's review that is not submitted yet: visible only to them.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingReview {
    pub id: String,
    pub body: String,
    /// The commit it was started on.
    pub commit: Option<String>,
    pub comments: u32,
}

/// 1-based line ranges, inclusive, of the diff hunks GitHub shows for a file.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Commentable {
    pub left: Vec<[u32; 2]>,
    pub right: Vec<[u32; 2]>,
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
    /// GraphQL node ID and REST ID.
    pub id: String,
    pub database_id: Option<u64>,
    pub can_update: bool,
    pub can_delete: bool,
    /// Written by the viewer.
    pub own: bool,
    /// Part of the viewer's pending review.
    pub pending: bool,
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
    /// GraphQL node ID; empty for a pending thread only REST reported.
    pub id: String,
    pub can_reply: bool,
    pub can_resolve: bool,
    pub can_unresolve: bool,
    /// Every comment is in the viewer's pending review.
    pub pending: bool,
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

/// Pages of 100 changed files read for the commentable lines at most.
const FILE_PAGES: u32 = 30;

const CONVERSATION_QUERY: &str = r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  viewer { login }
  repository(owner: $owner, name: $repo) {
    viewerPermission
    pullRequest(number: $number) {
      id number title state url headRefOid locked viewerDidAuthor
      author { login __typename }
      pending: reviews(first: 1, states: [PENDING]) {
        nodes { id databaseId body author { login } commit { oid } comments(first: 100) { totalCount nodes { id } } }
      }
      reviews(first: 100) { pageInfo { hasNextPage } nodes { author { login __typename } state body submittedAt url } }
      comments(first: 100) { pageInfo { hasNextPage } nodes { id databaseId author { login __typename } body createdAt url } }
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id viewerCanReply viewerCanResolve viewerCanUnresolve
          path line startLine originalLine diffSide isResolved isOutdated subjectType
          comments(first: 100) {
            pageInfo { hasNextPage }
            nodes { id databaseId state viewerCanUpdate viewerCanDelete viewerDidAuthor author { login __typename } body createdAt url }
          }
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
            let json = gh(&args).map_err(|e| bad(format!("cannot read the comments: {e}.{}", advice(&e, &help))))?;
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
    let flag = |v: &Value, k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let note = |v: &Value, created: &str| {
        let (author, bot) = author(v);
        Note {
            author,
            bot,
            body: text(v, "body"),
            created: text(v, created),
            url: text(v, "url"),
            id: text(v, "id"),
            database_id: v.get("databaseId").and_then(Value::as_u64),
            can_update: flag(v, "viewerCanUpdate"),
            can_delete: flag(v, "viewerCanDelete"),
            own: flag(v, "viewerDidAuthor"),
            pending: text(v, "state") == "PENDING",
        }
    };
    let nodes = |v: &Value| v.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let more = |v: &Value| v.pointer("/pageInfo/hasNextPage").and_then(Value::as_bool).unwrap_or(false);

    let mut after: Option<String> = None;
    let mut conv: Option<Conversation> = None;
    let mut threads = Vec::new();
    let mut truncated = false;
    let mut pending_db: Option<u64> = None;
    let mut pending_ids: Vec<String> = Vec::new();
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
            let viewer = data.pointer("/data/viewer/login").and_then(Value::as_str).map(str::to_string);
            let permission = data.pointer("/data/repository/viewerPermission").and_then(Value::as_str).unwrap_or("");
            let can_comment =
                viewer.is_some() && (!flag(pull, "locked") || matches!(permission, "ADMIN" | "MAINTAIN" | "WRITE"));
            // GitHub shows a pending review to its author only; checked all the same.
            let review = pull.pointer("/pending/nodes").and_then(Value::as_array).and_then(|n| {
                n.iter().find(|r| {
                    r.pointer("/author/login").and_then(Value::as_str) == viewer.as_deref() && viewer.is_some()
                })
            });
            if let Some(r) = review {
                pending_db = r.get("databaseId").and_then(Value::as_u64);
                pending_ids = r
                    .pointer("/comments/nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|c| text(c, "id"))
                    .collect();
            }
            let review = review.map(|r| PendingReview {
                id: text(r, "id"),
                body: text(r, "body"),
                commit: r.pointer("/commit/oid").and_then(Value::as_str).map(str::to_string),
                comments: r.pointer("/comments/totalCount").and_then(Value::as_u64).unwrap_or(0) as u32,
            });
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
                id: text(pull, "id"),
                viewer,
                can_comment,
                own: flag(pull, "viewerDidAuthor"),
                review,
                files: BTreeMap::new(),
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
                pending: !comments.is_empty() && comments.iter().all(|c| c.pending),
                severity,
                comments,
                id: text(&t, "id"),
                can_reply: flag(&t, "viewerCanReply"),
                can_resolve: flag(&t, "viewerCanResolve"),
                can_unresolve: flag(&t, "viewerCanUnresolve"),
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
    // Comments of the pending review the threads did not carry: read where they are.
    let listed: std::collections::HashSet<&str> =
        threads.iter().flat_map(|t| t.comments.iter().map(|c| c.id.as_str())).collect();
    if let Some(db) = pending_db.filter(|_| pending_ids.iter().any(|id| !listed.contains(id.as_str()))) {
        let endpoint = format!("{}/pulls/{}/reviews/{db}/comments?per_page=100", pr.endpoint(), pr.number);
        let known: std::collections::HashSet<String> = listed.iter().map(|s| s.to_string()).collect();
        for c in rest(&endpoint).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default() {
            if known.contains(&text(&c, "node_id")) {
                continue;
            }
            let line = |k: &str| c.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
            let login = c.pointer("/user/login").and_then(Value::as_str).map(str::to_string);
            threads.push(Thread {
                path: text(&c, "path"),
                side: if text(&c, "side") == "LEFT" { "left" } else { "right" },
                line: line("line"),
                start_line: line("start_line"),
                original_line: line("original_line"),
                resolved: false,
                outdated: false,
                file_level: text(&c, "subject_type") == "file",
                severity: None,
                comments: vec![Note {
                    bot: c.pointer("/user/type").and_then(Value::as_str) == Some("Bot"),
                    author: login,
                    body: text(&c, "body"),
                    created: text(&c, "created_at"),
                    url: text(&c, "html_url"),
                    id: text(&c, "node_id"),
                    database_id: c.get("id").and_then(Value::as_u64),
                    can_update: true,
                    can_delete: true,
                    own: true,
                    pending: true,
                }],
                id: String::new(),
                can_reply: false,
                can_resolve: false,
                can_unresolve: false,
                pending: true,
            });
        }
    }
    conv.threads = threads;

    // Where GitHub accepts comments: the hunks of the pull request's diff, which
    // is the merge base against the current head; only useful while that is the one compared.
    if conv.head == pinned_head {
        'pages: for page in 1..=FILE_PAGES {
            let Ok(list) = rest(&format!("{}/pulls/{}/files?per_page=100&page={page}", pr.endpoint(), pr.number))
            else {
                break;
            };
            let list = list.as_array().cloned().unwrap_or_default();
            for f in &list {
                let path = text(f, "filename");
                if !path.is_empty() {
                    conv.files.insert(path, hunks(f.get("patch").and_then(Value::as_str).unwrap_or("")));
                }
            }
            if list.len() < 100 {
                break 'pages;
            }
        }
    }

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

/// The line ranges of a unified diff's hunks, per side.
fn hunks(patch: &str) -> Commentable {
    let mut out = Commentable::default();
    let range = |s: &str| -> Option<[u32; 2]> {
        let (start, count) = s.split_once(',').unwrap_or((s, "1"));
        let (start, count) = (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?);
        (count > 0 && start > 0).then(|| [start, start + count - 1])
    };
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix("@@ -") else { continue };
        let Some((left, rest)) = rest.split_once(" +") else { continue };
        let Some((right, _)) = rest.split_once(" @@") else { continue };
        out.left.extend(range(left));
        out.right.extend(range(right));
    }
    out
}

// ---------------------------------------------------------------------------
// Writing: replies, resolving, comments and the viewer's pending review.

/// Where a new comment or reply goes: posted at once, or into the viewer's
/// pending review (started on `head` if there is none).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntoReview {
    /// The pending review, if the app knows one.
    pub id: Option<String>,
    /// The pull request's node ID, to start one.
    pub pull: String,
    pub head: String,
}

/// One change to a pull request's conversation, made only on explicit request.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Write {
    /// A reply to a thread: `comment` is the REST ID of one of its comments.
    Reply {
        thread: String,
        comment: u64,
        body: String,
        review: Option<IntoReview>,
    },
    /// A comment on lines `start..=end` (1-based) of `side`, or on the file when `end` is 0.
    Comment {
        path: String,
        side: String,
        start: u32,
        end: u32,
        body: String,
        head: String,
        review: Option<IntoReview>,
    },
    Resolve {
        thread: String,
        resolved: bool,
    },
    Edit {
        comment: String,
        body: String,
    },
    Delete {
        comment: String,
    },
    /// `event` is `COMMENT`, `APPROVE` or `REQUEST_CHANGES`; a review without
    /// comments (only a decision and a summary) is started first.
    Submit {
        review: IntoReview,
        event: String,
        body: String,
    },
    Discard {
        review: String,
    },
}

/// A variable of a GraphQL request or a field of a REST request.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Text(String),
    Number(u64),
    Flag(bool),
}

fn field_args(fields: &[(&str, Field)]) -> Vec<String> {
    let mut args = Vec::new();
    for (k, v) in fields {
        // `-f` is a raw string (never a file reference); `-F` turns numbers and flags into JSON.
        let (flag, value) = match v {
            Field::Text(t) => ("-f", t.clone()),
            Field::Number(n) => ("-F", n.to_string()),
            Field::Flag(b) => ("-F", b.to_string()),
        };
        args.push(flag.to_string());
        args.push(format!("{k}={value}"));
    }
    args
}

/// Makes one change through `gh`; the conversation is read again afterwards.
pub fn write(url: &str, change: Write) -> Result<()> {
    let pr = PullRequestUrl::parse(url)?;
    let host = pr.host.clone();
    let help = pr.access_help();
    let run = |args: Vec<String>| -> Result<serde_json::Value> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let json = gh(&args).map_err(|e| bad(format!("{e}.{}", advice(&e, &help))))?;
        if json.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_str(&json).map_err(|e| bad(format!("invalid API response: {e}")))
    };
    write_with(
        &pr,
        change,
        &mut |query, fields| {
            let mut args = vec!["api".into(), "--hostname".into(), host.clone(), "graphql".into()];
            args.extend(field_args(&[("query", Field::Text(query.to_string()))]));
            args.extend(field_args(fields));
            run(args)
        },
        &mut |endpoint, fields| {
            let mut args = vec!["api".into(), "--hostname".into(), host.clone(), "--method".into(), "POST".into()];
            args.push(endpoint.to_string());
            args.extend(field_args(fields));
            run(args)
        },
    )
    .map_err(|e| bad(format!("GitHub did not take the change: {}", plain(&e.to_string()))))
}

const MAX_BODY: usize = 65_000;

fn node_id(id: &str) -> Result<()> {
    if !id.is_empty() && id.len() <= 200 && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-=".contains(&c)) {
        Ok(())
    } else {
        Err(bad("invalid GitHub ID"))
    }
}

fn body_text(body: &str) -> Result<()> {
    if body.trim().is_empty() {
        Err(bad("the comment is empty"))
    } else if body.len() > MAX_BODY {
        Err(bad("the comment is too long for GitHub"))
    } else {
        Ok(())
    }
}

fn graphql_ok(v: serde_json::Value) -> Result<serde_json::Value> {
    use serde_json::Value;
    if let Some(errors) = v.get("errors").and_then(Value::as_array).filter(|e| !e.is_empty()) {
        let message = errors.iter().filter_map(|e| e.get("message").and_then(Value::as_str)).collect::<Vec<_>>();
        return Err(bad(message.join("; ")));
    }
    Ok(v)
}

type GraphQl<'a> = dyn FnMut(&str, &[(&str, Field)]) -> Result<serde_json::Value> + 'a;
type Post<'a> = dyn FnMut(&str, &[(&str, Field)]) -> Result<serde_json::Value> + 'a;

/// The pending review to add to: the known one, or a new one on `head`. A review
/// started meanwhile in the browser is used, since GitHub allows one per person.
fn review_id(graphql: &mut GraphQl, into: &IntoReview) -> Result<String> {
    use serde_json::Value;
    if let Some(id) = &into.id {
        node_id(id)?;
        return Ok(id.clone());
    }
    node_id(&into.pull)?;
    commit_id(&into.head)?;
    let started = graphql(
        "mutation($pull: ID!, $head: GitObjectID!) { addPullRequestReview(input: {pullRequestId: $pull, commitOID: $head}) { pullRequestReview { id } } }",
        &[("pull", Field::Text(into.pull.clone())), ("head", Field::Text(into.head.clone()))],
    )
    .and_then(graphql_ok);
    match started {
        Ok(v) => v
            .pointer("/data/addPullRequestReview/pullRequestReview/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| bad("GitHub did not start the review")),
        Err(error) => {
            let existing = graphql(
                "query($pull: ID!) { node(id: $pull) { ... on PullRequest { reviews(first: 1, states: [PENDING]) { nodes { id } } } } }",
                &[("pull", Field::Text(into.pull.clone()))],
            )
            .and_then(graphql_ok)
            .ok()
            .and_then(|v| v.pointer("/data/node/reviews/nodes/0/id").and_then(Value::as_str).map(str::to_string));
            existing.ok_or(error)
        }
    }
}

fn write_with(pr: &PullRequestUrl, change: Write, graphql: &mut GraphQl, post: &mut Post) -> Result<()> {
    let text = |s: &str| Field::Text(s.to_string());
    match change {
        Write::Reply { thread, comment, body, review } => {
            body_text(&body)?;
            match review {
                Some(into) => {
                    node_id(&thread)?;
                    let review = review_id(graphql, &into)?;
                    graphql(
                        "mutation($thread: ID!, $review: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, pullRequestReviewId: $review, body: $body}) { comment { id } } }",
                        &[("thread", text(&thread)), ("review", Field::Text(review)), ("body", Field::Text(body))],
                    )
                    .and_then(graphql_ok)?;
                }
                None => {
                    post(
                        &format!("{}/pulls/{}/comments/{comment}/replies", pr.endpoint(), pr.number),
                        &[("body", Field::Text(body))],
                    )?;
                }
            }
        }
        Write::Comment { path, side, start, end, body, head, review } => {
            body_text(&body)?;
            commit_id(&head)?;
            if path.is_empty() || path.chars().any(char::is_control) {
                return Err(bad("invalid file path"));
            }
            let side = match side.as_str() {
                "left" => "LEFT",
                "right" => "RIGHT",
                _ => return Err(bad("invalid side")),
            };
            let start = if start == 0 || start > end { end } else { start };
            match review {
                Some(into) => {
                    let review = review_id(graphql, &into)?;
                    let mut fields =
                        vec![("review", Field::Text(review)), ("path", text(&path)), ("body", Field::Text(body))];
                    if end == 0 {
                        fields.push(("subject", text("FILE")));
                    } else {
                        fields.extend([
                            ("subject", text("LINE")),
                            ("line", Field::Number(end.into())),
                            ("side", text(side)),
                        ]);
                        if start < end {
                            fields.extend([("start", Field::Number(start.into())), ("startSide", text(side))]);
                        }
                    }
                    graphql(
                        "mutation($review: ID!, $path: String!, $body: String!, $subject: PullRequestReviewThreadSubjectType, $line: Int, $side: DiffSide, $start: Int, $startSide: DiffSide) { addPullRequestReviewThread(input: {pullRequestReviewId: $review, path: $path, body: $body, subjectType: $subject, line: $line, side: $side, startLine: $start, startSide: $startSide}) { thread { id } } }",
                        &fields,
                    )
                    .and_then(graphql_ok)?;
                }
                None => {
                    let mut fields =
                        vec![("commit_id", Field::Text(head)), ("path", text(&path)), ("body", Field::Text(body))];
                    if end == 0 {
                        fields.push(("subject_type", text("file")));
                    } else {
                        fields.extend([("line", Field::Number(end.into())), ("side", text(side))]);
                        if start < end {
                            fields.extend([("start_line", Field::Number(start.into())), ("start_side", text(side))]);
                        }
                    }
                    post(&format!("{}/pulls/{}/comments", pr.endpoint(), pr.number), &fields)?;
                }
            }
        }
        Write::Resolve { thread, resolved } => {
            node_id(&thread)?;
            let query = if resolved {
                "mutation($thread: ID!) { resolveReviewThread(input: {threadId: $thread}) { thread { id } } }"
            } else {
                "mutation($thread: ID!) { unresolveReviewThread(input: {threadId: $thread}) { thread { id } } }"
            };
            graphql(query, &[("thread", Field::Text(thread))]).and_then(graphql_ok)?;
        }
        Write::Edit { comment, body } => {
            node_id(&comment)?;
            body_text(&body)?;
            graphql(
                "mutation($comment: ID!, $body: String!) { updatePullRequestReviewComment(input: {pullRequestReviewCommentId: $comment, body: $body}) { pullRequestReviewComment { id } } }",
                &[("comment", Field::Text(comment)), ("body", Field::Text(body))],
            )
            .and_then(graphql_ok)?;
        }
        Write::Delete { comment } => {
            node_id(&comment)?;
            graphql(
                "mutation($comment: ID!) { deletePullRequestReviewComment(input: {id: $comment}) { clientMutationId } }",
                &[("comment", Field::Text(comment))],
            )
            .and_then(graphql_ok)?;
        }
        Write::Submit { review, event, body } => {
            if !matches!(event.as_str(), "COMMENT" | "APPROVE" | "REQUEST_CHANGES") {
                return Err(bad("invalid review decision"));
            }
            if event == "REQUEST_CHANGES" && body.trim().is_empty() {
                return Err(bad("requesting changes needs a summary"));
            }
            if body.len() > MAX_BODY {
                return Err(bad("the summary is too long for GitHub"));
            }
            let review = review_id(graphql, &review)?;
            graphql(
                "mutation($review: ID!, $event: PullRequestReviewEvent!, $body: String) { submitPullRequestReview(input: {pullRequestReviewId: $review, event: $event, body: $body}) { pullRequestReview { id } } }",
                &[("review", Field::Text(review)), ("event", Field::Text(event)), ("body", Field::Text(body))],
            )
            .and_then(graphql_ok)?;
        }
        Write::Discard { review } => {
            node_id(&review)?;
            graphql(
                "mutation($review: ID!) { deletePullRequestReview(input: {pullRequestReviewId: $review}) { pullRequestReview { id } } }",
                &[("review", Field::Text(review))],
            )
            .and_then(graphql_ok)?;
        }
    }
    Ok(())
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
                } else if endpoint.contains("/files?") {
                    Ok(json!([]))
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
    fn conversation_reads_permissions_pending_review_and_commentable_lines() {
        let comment = json!({"id": "C_1", "databaseId": 101, "state": "PENDING", "viewerCanUpdate": true,
            "viewerCanDelete": true, "viewerDidAuthor": true, "author": {"login": "dave", "__typename": "User"},
            "body": "Why?", "createdAt": "2026-11-12T10:00:00Z", "url": "u"});
        let thread = json!({"id": "T_1", "viewerCanReply": true, "viewerCanResolve": false, "viewerCanUnresolve": false,
            "path": "src/a.rs", "line": 4, "startLine": null, "originalLine": 4, "diffSide": "RIGHT",
            "isResolved": false, "isOutdated": false, "subjectType": "LINE",
            "comments": {"pageInfo": {"hasNextPage": false}, "nodes": [comment]}});
        let pending = json!({"nodes": [{"id": "R_9", "databaseId": 9, "body": "", "author": {"login": "dave"},
            "commit": {"oid": HEAD}, "comments": {"totalCount": 2, "nodes": [{"id": "C_1"}, {"id": "C_2"}]}}]});
        let pull = json!({"id": "PR_42", "number": 42, "title": "T", "state": "OPEN", "url": "x", "headRefOid": HEAD,
            "locked": false, "viewerDidAuthor": false, "author": {"login": "alice", "__typename": "User"},
            "pending": pending,
            "reviews": {"pageInfo": {"hasNextPage": false}, "nodes": []},
            "comments": {"pageInfo": {"hasNextPage": false}, "nodes": []},
            "reviewThreads": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": [thread]}});
        let page = json!({"data": {"viewer": {"login": "dave"}, "repository": {"viewerPermission": "READ", "pullRequest": pull}}});
        let mut endpoints = Vec::new();
        let conv = conversation_with(&pr(), HEAD, &mut |_| Ok(page.clone()), &mut |endpoint| {
            endpoints.push(endpoint.to_string());
            if endpoint.contains("/reviews/9/comments") {
                Ok(json!([{"node_id": "C_2", "id": 102, "path": "src/b.rs", "line": null, "side": "RIGHT",
                    "subject_type": "file", "body": "Split this file", "created_at": "t", "html_url": "u",
                    "user": {"login": "dave", "type": "User"}}]))
            } else if endpoint.contains("/files?") {
                Ok(json!([{"filename": "src/a.rs", "patch": "@@ -1,3 +1,5 @@\n a\n+b"}, {"filename": "logo.png"}]))
            } else {
                Err(bad("no checks"))
            }
        })
        .unwrap();
        assert_eq!(
            (conv.id.as_str(), conv.viewer.as_deref(), conv.can_comment, conv.own),
            ("PR_42", Some("dave"), true, false)
        );
        let review = conv.review.as_ref().unwrap();
        assert_eq!((review.id.as_str(), review.comments, review.commit.as_deref()), ("R_9", 2, Some(HEAD)));
        let t = &conv.threads[0];
        assert_eq!((t.id.as_str(), t.can_reply, t.can_resolve, t.pending), ("T_1", true, false, true));
        let c = &t.comments[0];
        assert_eq!(
            (c.id.as_str(), c.database_id, c.own, c.can_delete, c.pending),
            ("C_1", Some(101), true, true, true)
        );
        // The pending file comment the threads did not carry comes from REST.
        let f = &conv.threads[1];
        assert_eq!(
            (f.path.as_str(), f.file_level, f.pending, f.comments[0].id.as_str()),
            ("src/b.rs", true, true, "C_2")
        );
        assert_eq!(conv.files["src/a.rs"], Commentable { left: vec![[1, 3]], right: vec![[1, 5]] });
        assert_eq!(conv.files["logo.png"], Commentable::default());
        assert!(endpoints.iter().any(|e| e == "repos/team/project/pulls/42/files?per_page=100&page=1"));

        // A newer head: lines of the compared one are not commentable, so not read.
        let mut endpoints = Vec::new();
        let conv = conversation_with(&pr(), TARGET, &mut |_| Ok(page.clone()), &mut |endpoint| {
            endpoints.push(endpoint.to_string());
            Ok(json!([]))
        })
        .unwrap();
        assert!(conv.files.is_empty() && !endpoints.iter().any(|e| e.contains("/files?")));
    }

    #[test]
    fn hunks_give_commentable_lines() {
        let c = hunks("@@ -21,8 +21,19 @@ fn x()\n ctx\n@@ -50 +61,0 @@\n-gone\n@@ -0,0 +1,3 @@\n+new");
        assert_eq!(c.left, [[21, 28], [50, 50]]);
        assert_eq!(c.right, [[21, 39], [1, 3]]);
        assert_eq!(hunks("Binary files differ"), Commentable::default());
    }

    #[test]
    fn writes_go_into_the_pending_review_or_post_at_once() {
        let text = |s: &str| Field::Text(s.to_string());
        let mut calls: Vec<(String, Vec<(String, Field)>)> = Vec::new();
        let into = IntoReview { id: None, pull: "PR_42".into(), head: HEAD.into() };
        let change = Write::Comment {
            path: "src/a.rs".into(),
            side: "right".into(),
            start: 3,
            end: 5,
            body: "Why?".into(),
            head: HEAD.into(),
            review: Some(into.clone()),
        };
        write_with(
            &pr(),
            change,
            &mut |q, f| {
                calls.push((q.to_string(), f.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()));
                Ok(if q.contains("addPullRequestReview(") {
                    json!({"data": {"addPullRequestReview": {"pullRequestReview": {"id": "R_1"}}}})
                } else {
                    json!({"data": {}})
                })
            },
            &mut |_, _| unreachable!("nothing is posted at once"),
        )
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].0.contains("addPullRequestReview(") && calls[0].1.contains(&("head".into(), text(HEAD))));
        let fields = &calls[1].1;
        assert!(calls[1].0.contains("addPullRequestReviewThread("));
        for f in
            [("review", text("R_1")), ("line", Field::Number(5)), ("start", Field::Number(3)), ("side", text("RIGHT"))]
        {
            assert!(fields.contains(&(f.0.to_string(), f.1.clone())), "{f:?} in {fields:?}");
        }

        // A review started meanwhile in the browser is used.
        let mut queries = Vec::new();
        let reply = Write::Reply { thread: "T_1".into(), comment: 101, body: "Done".into(), review: Some(into) };
        write_with(
            &pr(),
            reply,
            &mut |q, _| {
                queries.push(q.to_string());
                if q.contains("addPullRequestReview(") {
                    Ok(json!({"errors": [{"message": "User can only have one pending review per pull request"}]}))
                } else if q.contains("reviews(first: 1") {
                    Ok(json!({"data": {"node": {"reviews": {"nodes": [{"id": "R_7"}]}}}}))
                } else {
                    Ok(json!({"data": {}}))
                }
            },
            &mut |_, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(queries.len(), 3);
        assert!(queries[2].contains("addPullRequestReviewThreadReply"));

        // Without a review: REST, at once, with the compared commit.
        let mut posted = Vec::new();
        let change = Write::Comment {
            path: "src/a.rs".into(),
            side: "left".into(),
            start: 0,
            end: 0,
            body: "On the file".into(),
            head: HEAD.into(),
            review: None,
        };
        let mut no_graphql = |_: &str, _: &[(&str, Field)]| -> Result<Value> { unreachable!() };
        let mut post = |e: &str, f: &[(&str, Field)]| {
            posted.push((e.to_string(), f.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Vec<_>>()));
            Ok(Value::Null)
        };
        write_with(&pr(), change, &mut no_graphql, &mut post).unwrap();
        let reply = Write::Reply { thread: "T_1".into(), comment: 101, body: "Thanks".into(), review: None };
        write_with(&pr(), reply, &mut no_graphql, &mut post).unwrap();
        assert_eq!(posted[0].0, "repos/team/project/pulls/42/comments");
        assert!(posted[0].1.contains(&("subject_type".into(), text("file"))));
        assert!(posted[0].1.contains(&("commit_id".into(), text(HEAD))));
        assert_eq!(posted[1].0, "repos/team/project/pulls/42/comments/101/replies");

        // Refused before anything is sent.
        let known = IntoReview { id: Some("R_1".into()), pull: "PR_42".into(), head: HEAD.into() };
        for bad_change in [
            Write::Submit { review: known.clone(), event: "MERGE".into(), body: String::new() },
            Write::Submit { review: known.clone(), event: "REQUEST_CHANGES".into(), body: " ".into() },
            Write::Edit { comment: "C 1; drop".into(), body: "x".into() },
            Write::Reply { thread: "T_1".into(), comment: 1, body: "  ".into(), review: None },
            Write::Comment {
                path: "a".into(),
                side: "up".into(),
                start: 1,
                end: 1,
                body: "x".into(),
                head: HEAD.into(),
                review: None,
            },
            Write::Comment {
                path: "a".into(),
                side: "left".into(),
                start: 1,
                end: 1,
                body: "x".into(),
                head: "main".into(),
                review: None,
            },
        ] {
            assert!(write_with(&pr(), bad_change, &mut no_graphql, &mut |_, _| unreachable!()).is_err());
        }
        // Arguments: raw strings with -f, numbers with -F, never a file reference.
        assert_eq!(
            field_args(&[("body", text("@/etc/passwd")), ("line", Field::Number(3))]),
            ["-f", "body=@/etc/passwd", "-F", "line=3"]
        );
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
