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
