//! Remote Git repositories as compare sources.
//!
//! A remote side is a [`SourceSpec::GitRev`] whose `repo` is a URL. It is opened by
//! fetching just the one commit (`git fetch --depth 1`) into a bare repository in
//! the cache folder, which is then read like any local repository. Fetching goes
//! through the installed `git`, so credential helpers, SSH keys and proxies work as
//! they do on the command line.
//!
//! Besides `url@rev:path`, links copied from the web pages of GitHub, GitLab,
//! Bitbucket and Gitea/Codeberg are understood (`…/blob/main/src/lib.rs`).

use std::path::PathBuf;
use std::process::Command;

use parking_lot::Mutex;

use crate::source::SourceSpec;
use crate::{Error, Result};

/// Whether `repo` names a remote repository rather than a folder.
pub fn is_remote(repo: &str) -> bool {
    ["https://", "http://", "ssh://", "git://", "file://"].iter().any(|p| repo.starts_with(p)) || scp_like(repo)
}

/// `git@github.com:owner/repo.git`: user, host, a colon and a path, no scheme.
fn scp_like(text: &str) -> bool {
    let Some((user_host, path)) = text.split_once(':') else { return false };
    let Some((user, host)) = user_host.split_once('@') else { return false };
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    word(user) && word(host) && host.contains('.') && !path.starts_with("//") && !path.is_empty()
}

/// A remote source from `text`: `url@rev:path`, a bare repository URL, or a link to
/// a file or folder on a Git hosting site. `None` when `text` is not remote.
pub fn parse(text: &str) -> Option<SourceSpec> {
    parse_with(text, &|url| ls_remote(url).unwrap_or_default())
}

/// [`parse`] with the branch and tag names of a repository coming from `refs`.
fn parse_with(text: &str, refs: &dyn Fn(&str) -> Vec<String>) -> Option<SourceSpec> {
    if let Some(spec) = web_link(text, refs) {
        return Some(spec);
    }
    // `url@rev:path`, where the URL may itself contain `@` (`git@host:…`, `user@host`).
    let split = text.rfind('@').map(|at| (&text[..at], &text[at + 1..]));
    let (repo, rev, subpath) = match split {
        Some((repo, rest)) if is_repo_url(repo) && rev_part(rest) => {
            let (rev, subpath) = rest.split_once(':').unwrap_or((rest, ""));
            (repo, rev, subpath)
        }
        _ if is_repo_url(text) => (text, "", ""),
        _ => return None,
    };
    Some(SourceSpec::GitRev {
        repo: repo.to_string(),
        rev: if rev.is_empty() { "HEAD".into() } else { rev.into() },
        subpath: subpath.trim_matches('/').to_string(),
    })
}

/// A remote URL with a host and a path, not just its start (`https://user`).
fn is_repo_url(text: &str) -> bool {
    is_remote(text)
        && match text.split_once("://") {
            Some((_, rest)) => rest.split_once('/').is_some_and(|(host, path)| !host.is_empty() && !path.is_empty()),
            None => true,
        }
}

/// Whether what follows the last `@` is a revision (`main`, `v1.4:src/lib`), not
/// part of the URL (`user@host/path`).
fn rev_part(rest: &str) -> bool {
    let rev = rest.split_once(':').map_or(rest, |(rev, _)| rev);
    !rev.is_empty() && !rev.contains("//") && !rev.contains('@')
}

/// A link to the web page of a repository, branch, folder or file.
fn web_link(text: &str, refs: &dyn Fn(&str) -> Vec<String>) -> Option<SourceSpec> {
    let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"))?;
    let scheme = &text[..text.len() - rest.len()];
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (host, path) = rest.split_once('/')?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| p.ends_with(".git") || p.contains('@')) {
        // A clone URL or `url@rev:path`, handled below.
        return None;
    }
    // raw.githubusercontent.com/owner/repo/<ref>/<path>
    if host == "raw.githubusercontent.com" && parts.len() >= 3 {
        let repo = format!("https://github.com/{}/{}", parts[0], parts[1]);
        return Some(with_ref(repo, &parts[2..], refs));
    }
    // GitLab: <group/…/repo>/-/(blob|tree|raw)/<ref>/<path>
    if let Some(dash) = parts.iter().position(|p| *p == "-")
        && dash >= 2
        && matches!(parts.get(dash + 1), Some(&("blob" | "tree" | "raw")))
    {
        let repo = format!("{scheme}{host}/{}", parts[..dash].join("/"));
        return Some(with_ref(repo, &parts[dash + 2..], refs));
    }
    if parts.len() < 2 {
        return None;
    }
    let repo = format!("{scheme}{host}/{}/{}", parts[0], parts[1]);
    match parts.get(2).copied() {
        // The repository's main page.
        None => Some(SourceSpec::GitRev { repo, rev: "HEAD".into(), subpath: String::new() }),
        // Gitea, Forgejo, Codeberg: src|raw/branch|tag|commit/<ref>/<path>.
        Some("src" | "raw") if matches!(parts.get(3), Some(&("branch" | "tag" | "commit"))) && parts.len() > 4 => {
            Some(with_ref(repo, &parts[4..], refs))
        }
        // GitHub (also Enterprise): blob, tree, raw.
        Some("blob" | "tree" | "raw") if parts.len() > 3 => Some(with_ref(repo, &parts[3..], refs)),
        // Bitbucket: src/<ref>/<path>.
        Some("src") if parts.len() > 3 => Some(with_ref(repo, &parts[3..], refs)),
        _ => None,
    }
}

/// Splits `ref/and/path` into the revision and the path in it. Branch names may
/// hold slashes (`feature/x/src/lib.rs`), so the longest branch or tag of the
/// repository that the parts start with wins; a commit id or an unknown name is
/// the first part.
fn with_ref(repo: String, parts: &[&str], refs: &dyn Fn(&str) -> Vec<String>) -> SourceSpec {
    let decoded: Vec<String> = parts.iter().map(|p| percent_decode(p)).collect();
    let first = decoded.first().cloned().unwrap_or_default();
    let is_commit = first.len() >= 7 && first.chars().all(|c| c.is_ascii_hexdigit());
    let mut take = 1;
    if !is_commit && decoded.len() > 1 {
        let known = refs(&repo);
        for n in (1..=decoded.len()).rev() {
            if known.iter().any(|r| *r == decoded[..n].join("/")) {
                take = n;
                break;
            }
        }
    }
    SourceSpec::GitRev {
        repo,
        rev: if first.is_empty() { "HEAD".into() } else { decoded[..take].join("/") },
        subpath: decoded[take.min(decoded.len())..].join("/"),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = text.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The URL without a user name or password, for display.
pub fn display(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('/') {
            Some((authority, path)) => {
                let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
                format!("{scheme}://{host}/{path}")
            }
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

/// Fetches `rev` of the repository at `url` into the cache and returns the local
/// bare repository and the fetched commit id.
pub fn fetch(url: &str, rev: &str) -> Result<(PathBuf, String)> {
    plain_arg("repository", url)?;
    plain_arg("revision", rev)?;
    // One fetch at a time: fetches into one cache repository would collide.
    static FETCHING: Mutex<()> = Mutex::new(());
    let _one = FETCHING.lock();
    let dir = cache_dir()?.join(cache_name(url));
    if !dir.join("HEAD").exists() {
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(dir.display().to_string(), e))?;
        git(&["init", "--bare", "--quiet"], Some(&dir))?;
    }
    // A full commit id that is here already needs no network.
    let full_id = rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit());
    if full_id && git(&["cat-file", "-e", &format!("{rev}^{{commit}}")], Some(&dir)).is_ok() {
        return Ok((dir, rev.to_string()));
    }
    git(&["fetch", "--quiet", "--depth", "1", "--no-tags", "--end-of-options", url, rev], Some(&dir)).map_err(|e| {
        match e {
            Error::Unsupported(msg) if !full_id && rev.len() < 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) => {
                Error::Unsupported(format!("{msg} (abbreviated commit ids cannot be fetched; use the full id)"))
            }
            e => e,
        }
    })?;
    let commit = git(&["rev-parse", "FETCH_HEAD^{commit}"], Some(&dir))?;
    Ok((dir, commit.trim().to_string()))
}

/// Keep an opened PR snapshot reachable when Git later runs cache maintenance.
pub(crate) fn pin_commit(dir: &std::path::Path, commit: &str) -> Result<()> {
    plain_arg("commit", commit)?;
    git(&["update-ref", &format!("refs/parallax/commits/{commit}"), commit], Some(dir))?;
    Ok(())
}

/// The branch and tag names of the repository at `url`.
pub fn ls_remote(url: &str) -> Result<Vec<String>> {
    plain_arg("repository", url)?;
    let out = git(&["ls-remote", "--heads", "--tags", "--end-of-options", url], None)?;
    Ok(out
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .filter_map(|r| r.strip_prefix("refs/heads/").or_else(|| r.strip_prefix("refs/tags/")))
        .map(|r| r.trim_end_matches("^{}").to_string())
        .collect())
}

/// The branches and tags of the repository at `url`, for picking a revision, and
/// which branch its HEAD is.
pub fn ref_list(url: &str) -> Result<crate::vfs::git::RefList> {
    use crate::vfs::git::{RefInfo, RefList};
    plain_arg("repository", url)?;
    let out = git(&["ls-remote", "--symref", "--end-of-options", url], None)?;
    let mut list =
        RefList { head: None, branches: Vec::new(), remotes: Vec::new(), tags: Vec::new(), recent_commits: Vec::new() };
    for line in out.lines() {
        if let Some(target) = line.strip_prefix("ref: ").and_then(|l| l.strip_suffix("\tHEAD")) {
            list.head = target.strip_prefix("refs/heads/").map(str::to_string);
            continue;
        }
        let Some((id, name)) = line.split_once('\t') else { continue };
        let info = |name: &str| RefInfo { name: name.to_string(), short_id: id.chars().take(7).collect() };
        if let Some(branch) = name.strip_prefix("refs/heads/") {
            list.branches.push(info(branch));
        } else if let Some(tag) = name.strip_prefix("refs/tags/") {
            // Annotated tags come twice; the peeled line (`^{}`) names the commit.
            match tag.strip_suffix("^{}") {
                Some(tag) => {
                    list.tags.retain(|t| t.name != tag);
                    list.tags.push(info(tag));
                }
                None => list.tags.push(info(tag)),
            }
        }
    }
    Ok(list)
}

/// A repository or revision from the user must not be read by Git as an option
/// (`@--upload-pack=…` would run a program).
fn plain_arg(what: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(Error::Unsupported(format!("not a valid {what}: {value:?}")));
    }
    Ok(())
}

/// Runs `git` and returns its output; never asks for a password in a terminal.
/// A server that does not answer is given up on: a lookup of the refs after
/// 30 seconds, a fetch after 10 minutes.
fn git(args: &[&str], dir: Option<&std::path::Path>) -> Result<String> {
    let command = args.first().copied().unwrap_or_default();
    let limit = std::time::Duration::from_secs(match command {
        "ls-remote" => 30,
        "fetch" => 600,
        _ => 120,
    });
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0");
    crate::process::run(&mut cmd, "git", &format!("git {command}"), limit)
}

/// Where fetched repositories are kept: `PARALLAX_CACHE`, else the user's cache folder.
fn cache_dir() -> Result<PathBuf> {
    let base = std::env::var_os("PARALLAX_CACHE")
        .map(PathBuf::from)
        .or_else(|| if cfg!(windows) { std::env::var_os("LOCALAPPDATA").map(PathBuf::from) } else { None })
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::home_dir().map(|h| h.join(if cfg!(target_os = "macos") { "Library/Caches" } else { ".cache" }))
        })
        .unwrap_or_else(std::env::temp_dir);
    Ok(base.join("ParallaxCompare").join("remotes"))
}

/// A readable folder name for `url` that differs for every URL.
fn cache_name(url: &str) -> String {
    let clean = display(url);
    let tail: String = clean
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    format!("{tail}-{:08x}", crc32fast::hash(url.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(repo: &str, rev: &str, subpath: &str) -> Option<SourceSpec> {
        Some(SourceSpec::GitRev { repo: repo.into(), rev: rev.into(), subpath: subpath.into() })
    }

    #[test]
    fn urls_with_revisions() {
        let none = |_: &str| Vec::new();
        assert_eq!(
            parse_with("https://github.com/o/r.git@v1.4:src/lib", &none),
            rev("https://github.com/o/r.git", "v1.4", "src/lib")
        );
        assert_eq!(parse_with("https://github.com/o/r.git", &none), rev("https://github.com/o/r.git", "HEAD", ""));
        assert_eq!(parse_with("git@github.com:o/r.git@main", &none), rev("git@github.com:o/r.git", "main", ""));
        assert_eq!(parse_with("git@github.com:o/r.git", &none), rev("git@github.com:o/r.git", "HEAD", ""));
        assert_eq!(
            parse_with("https://user:token@host.example/a/r.git@feature/x:doc", &none),
            rev("https://user:token@host.example/a/r.git", "feature/x", "doc")
        );
        assert_eq!(parse_with("/home/me/repo@main", &none), None);
        assert_eq!(parse_with("C:\\src\\repo@main", &none), None);
        assert_eq!(
            parse_with("https://user@host.example/a/r.git", &none),
            rev("https://user@host.example/a/r.git", "HEAD", "")
        );
        assert_eq!(display("https://user:token@host.example/a/r.git"), "https://host.example/a/r.git");
    }

    #[test]
    fn web_links() {
        let refs = |_: &str| vec!["main".to_string(), "feature/x".to_string(), "v1.0".to_string()];
        let gh = "https://github.com/o/r";
        assert_eq!(parse_with("https://github.com/o/r/blob/main/src/lib.rs", &refs), rev(gh, "main", "src/lib.rs"));
        assert_eq!(parse_with("https://github.com/o/r/tree/feature/x/src", &refs), rev(gh, "feature/x", "src"));
        assert_eq!(parse_with("https://github.com/o/r/blob/0123abcd/a%20b.txt", &refs), rev(gh, "0123abcd", "a b.txt"));
        assert_eq!(parse_with("https://github.com/o/r", &refs), rev(gh, "HEAD", ""));
        assert_eq!(parse_with("https://github.com/o/r/tree/v1.0?tab=readme#top", &refs), rev(gh, "v1.0", ""));
        assert_eq!(parse_with("https://raw.githubusercontent.com/o/r/main/a/b.c", &refs), rev(gh, "main", "a/b.c"));
        assert_eq!(
            parse_with("https://gitlab.com/g/sub/r/-/blob/feature/x/README.md", &refs),
            rev("https://gitlab.com/g/sub/r", "feature/x", "README.md")
        );
        assert_eq!(
            parse_with("https://codeberg.org/o/r/src/branch/main/docs/x.md", &refs),
            rev("https://codeberg.org/o/r", "main", "docs/x.md")
        );
        let cb = "https://codeberg.org/o/r";
        assert_eq!(
            parse_with("https://codeberg.org/o/r/raw/branch/main/src/lib.rs", &refs),
            rev(cb, "main", "src/lib.rs")
        );
        assert_eq!(parse_with("https://codeberg.org/o/r/raw/tag/v1.0/a.txt", &refs), rev(cb, "v1.0", "a.txt"));
        assert_eq!(
            parse_with("https://codeberg.org/o/r/raw/commit/0123abcd/a.txt", &refs),
            rev(cb, "0123abcd", "a.txt")
        );
        assert_eq!(
            parse_with("https://bitbucket.org/o/r/src/main/x.py", &refs),
            rev("https://bitbucket.org/o/r", "main", "x.py")
        );
        // As the app writes them back: `url@rev:path`.
        assert_eq!(parse_with("https://github.com/o/r@main", &refs), rev(gh, "main", ""));
        assert_eq!(parse_with("https://github.com/o/r@feature/x:src/a.rs", &refs), rev(gh, "feature/x", "src/a.rs"));
        // An unknown branch: the first part is the revision.
        assert_eq!(parse_with("https://github.com/o/r/blob/dev/x/y.rs", &refs), rev(gh, "dev", "x/y.rs"));
        // Any other address is taken as a repository; Git reports when it is none.
        assert_eq!(parse_with("https://example.com/about", &refs), rev("https://example.com/about", "HEAD", ""));
    }

    /// Fetches from a repository given by a file:// URL, as from a server.
    #[test]
    fn fetching_a_commit() {
        let origin = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").args(args).current_dir(origin.path()).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(origin.path().join("a.txt"), "one").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "one"]);
        // SAFETY: the variable is only read by this test's fetch.
        unsafe { std::env::set_var("PARALLAX_CACHE", cache.path()) };
        // file:///tmp/x, and file:///C:/x on Windows.
        let path = origin.path().display().to_string().replace('\\', "/");
        let url = if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") };
        let (dir, commit) = fetch(&url, "main").unwrap();
        assert!(dir.starts_with(cache.path()));
        assert_eq!(commit.len(), 40);
        // Known commits are not fetched again.
        assert_eq!(fetch(&url, &commit).unwrap().1, commit);
        assert_eq!(ls_remote(&url).unwrap(), vec!["main".to_string()]);
        let refs = ref_list(&url).unwrap();
        assert_eq!(refs.head.as_deref(), Some("main"));
        assert_eq!(refs.branches[0].name, "main");
        // A revision that Git would read as an option runs nothing.
        let marker = origin.path().join("ran");
        let option = format!("--upload-pack=touch {}; git-upload-pack", marker.display());
        assert!(fetch(&url, &option).is_err());
        assert!(fetch(&url, "-q").is_err() && ls_remote("--help").is_err());
        assert!(!marker.exists());
    }
}
