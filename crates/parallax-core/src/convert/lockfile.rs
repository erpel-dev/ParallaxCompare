//! Lockfiles of package managers, as the list of packages they pin.
//!
//! A lockfile diff is mostly noise: hashes, resolved URLs, dependency lists of
//! every package, and layout changes when the tool's version changes (npm's
//! lockfile v1 → v3, pnpm 8 → 9). The "Dependencies" view reduces each file to
//! one line per package and version, sorted and without duplicates, with the
//! source only when it is not the ecosystem's default registry (a git checkout,
//! a local path, another registry). So an update shows as `- chalk 4.1.1` /
//! `+ chalk 4.1.2`, and a re-generated lockfile with the same versions shows no
//! change at all.
//!
//! Files are recognised by their name: `package-lock.json`, `npm-shrinkwrap.json`
//! (v1 nested `dependencies`, v2/v3 `packages`), `yarn.lock` (v1 and Berry),
//! `pnpm-lock.yaml` (v5 to v9), `Cargo.lock`, `poetry.lock`, `uv.lock`,
//! `Pipfile.lock`, `composer.lock`, `Gemfile.lock`/`gems.locked` and `go.sum`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;

use super::data::{YamlRef, yaml_document};
use crate::{Error, Result};

/// Which lockfile format a file name stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Npm,
    Yarn,
    Pnpm,
    Cargo,
    Poetry,
    Uv,
    Pipenv,
    Composer,
    Bundler,
    GoSum,
}

pub fn format_of(name: &str) -> Option<Format> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    Some(match base.as_str() {
        "package-lock.json" | "npm-shrinkwrap.json" => Format::Npm,
        "yarn.lock" => Format::Yarn,
        "pnpm-lock.yaml" => Format::Pnpm,
        "cargo.lock" => Format::Cargo,
        "poetry.lock" => Format::Poetry,
        "uv.lock" => Format::Uv,
        "pipfile.lock" => Format::Pipenv,
        "composer.lock" => Format::Composer,
        "gemfile.lock" | "gems.locked" => Format::Bundler,
        "go.sum" => Format::GoSum,
        _ => return None,
    })
}

/// A pinned package; `source` only when it is not the default registry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
}

fn pkg(name: impl Into<String>, version: impl Into<String>, source: Option<String>) -> Package {
    Package { name: name.into(), version: version.into(), source }
}

/// One line per package and version, sorted, with a header of counts.
pub fn dependencies(name: &str, data: &[u8]) -> Result<String> {
    let format = format_of(name).ok_or_else(|| Error::Unsupported(format!("{name} is not a known lockfile")))?;
    let (label, packages) = parse(format, data)?;
    let mut out = String::with_capacity(packages.len() * 32 + 200);
    let line = |out: &mut String, label: &str, value: &dyn std::fmt::Display| {
        let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
    };
    line(&mut out, "Lockfile", &label);
    line(&mut out, "Packages", &packages.len());
    let mut sources: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &packages {
        let kind = p.source.as_deref().map_or("registry", |s| s.split([':', ' ', '+']).next().unwrap_or(s));
        *sources.entry(kind).or_default() += 1;
    }
    if sources.len() > 1 || !sources.contains_key("registry") {
        let list: Vec<String> = sources.iter().map(|(k, n)| format!("{k} {n}")).collect();
        line(&mut out, "Sources", &list.join(", "));
    }
    out.push_str("\n=== Packages ===\n");
    for p in &packages {
        out.push_str(&p.name);
        out.push(' ');
        out.push_str(&p.version);
        if let Some(s) = &p.source {
            out.push_str("  (");
            out.push_str(s);
            out.push(')');
        }
        out.push('\n');
    }
    Ok(out)
}

/// Whether two lockfiles pin the same packages and versions (and sources).
/// `None` when either is not readable.
pub fn same_packages(name: &str, left: &[u8], right: &[u8]) -> Option<bool> {
    let format = format_of(name)?;
    Some(parse(format, left).ok()?.1 == parse(format, right).ok()?.1)
}

/// The format label and the packages, sorted and deduplicated.
fn parse(format: Format, data: &[u8]) -> Result<(String, Vec<Package>)> {
    let text = || std::str::from_utf8(data).map_err(|_| bad("not UTF-8"));
    let (label, mut packages) = match format {
        Format::Npm => npm(data)?,
        Format::Yarn => yarn(text()?)?,
        Format::Pnpm => pnpm(data)?,
        Format::Cargo => ("Cargo.lock".into(), cargo(text()?)?),
        Format::Poetry => ("poetry.lock".into(), poetry(text()?)?),
        Format::Uv => ("uv.lock".into(), uv(text()?)?),
        Format::Pipenv => ("Pipfile.lock".into(), pipenv(data)?),
        Format::Composer => ("composer.lock".into(), composer(data)?),
        Format::Bundler => ("Gemfile.lock".into(), bundler(text()?)),
        Format::GoSum => ("go.sum".into(), go_sum(text()?)),
    };
    packages.sort_unstable();
    packages.dedup();
    Ok((label, packages))
}

fn bad(why: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("not a readable lockfile: {why}"))
}

// ---- npm ----

#[derive(Deserialize)]
struct NpmLock {
    #[serde(rename = "lockfileVersion", default)]
    version: u32,
    packages: Option<BTreeMap<String, NpmPackage>>,
    dependencies: Option<BTreeMap<String, NpmDependency>>,
}

#[derive(Deserialize)]
struct NpmPackage {
    name: Option<String>,
    version: Option<String>,
    resolved: Option<String>,
    #[serde(default)]
    link: bool,
}

#[derive(Deserialize)]
struct NpmDependency {
    version: Option<String>,
    resolved: Option<String>,
    dependencies: Option<BTreeMap<String, NpmDependency>>,
}

/// The source of an npm-style package when it is not the public registry.
fn npm_source(resolved: Option<&str>) -> Option<String> {
    let r = resolved?;
    let default = ["https://registry.npmjs.org/", "https://registry.yarnpkg.com/", "https://registry.npmjs.com/"];
    if default.iter().any(|d| r.starts_with(d)) {
        return None;
    }
    if r.starts_with("http://") || r.starts_with("https://") {
        // Another registry: its host is what matters, not the tarball path.
        let host = r.split('/').nth(2).unwrap_or(r);
        return Some(format!("registry {host}"));
    }
    Some(r.to_string())
}

fn npm(data: &[u8]) -> Result<(String, Vec<Package>)> {
    let lock: NpmLock = serde_json::from_slice(data).map_err(bad)?;
    let label = format!("package-lock.json v{}", lock.version.max(1));
    let mut out = Vec::new();
    if let Some(packages) = lock.packages {
        for (path, p) in packages {
            if path.is_empty() || p.link {
                continue;
            }
            let (name, source) = match path.rfind("node_modules/") {
                Some(i) => (path[i + "node_modules/".len()..].to_string(), npm_source(p.resolved.as_deref())),
                // A workspace folder.
                None => (p.name.clone().unwrap_or_else(|| path.clone()), Some(format!("workspace {path}"))),
            };
            out.push(pkg(name, p.version.unwrap_or_default(), source));
        }
    } else if let Some(deps) = lock.dependencies {
        fn walk(deps: BTreeMap<String, NpmDependency>, out: &mut Vec<Package>) {
            for (name, d) in deps {
                out.push(pkg(name, d.version.unwrap_or_default(), npm_source(d.resolved.as_deref())));
                if let Some(nested) = d.dependencies {
                    walk(nested, out);
                }
            }
        }
        walk(deps, &mut out);
    } else {
        return Err(bad("neither packages nor dependencies"));
    }
    Ok((label, out))
}

// ---- yarn ----

/// `@scope/name@^1.0` → `@scope/name`; the first `@` after the first character splits.
fn split_at_version(spec: &str) -> (&str, &str) {
    match spec.get(1..).and_then(|s| s.find('@')) {
        Some(i) => (&spec[..i + 1], &spec[i + 2..]),
        None => (spec, ""),
    }
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"')
}

fn yarn(text: &str) -> Result<(String, Vec<Package>)> {
    if text.contains("\n__metadata:") || text.starts_with("__metadata:") {
        return yarn_berry(text);
    }
    let mut out = Vec::new();
    let mut current: Option<(String, Option<String>, Option<String>)> = None;
    let flush = |current: &mut Option<(String, Option<String>, Option<String>)>, out: &mut Vec<Package>| {
        if let Some((name, Some(version), resolved)) = current.take() {
            let source = npm_source(resolved.as_deref().map(|r| r.split('#').next().unwrap_or(r)));
            out.push(pkg(name, version, source));
        }
    };
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            flush(&mut current, &mut out);
            let first = line.trim_end_matches(':').split(", ").next().unwrap_or_default();
            let (name, _) = split_at_version(unquote(first));
            current = Some((name.to_string(), None, None));
        } else if let Some(cur) = current.as_mut()
            && line.starts_with("  ")
            && !line.starts_with("   ")
        {
            if let Some(v) = line.trim_start().strip_prefix("version ") {
                cur.1 = Some(unquote(v).to_string());
            } else if let Some(r) = line.trim_start().strip_prefix("resolved ") {
                cur.2 = Some(unquote(r).to_string());
            }
        }
    }
    flush(&mut current, &mut out);
    if out.is_empty() && !text.contains("yarn lockfile") {
        return Err(bad("no entries"));
    }
    Ok(("yarn.lock v1".into(), out))
}

fn yarn_berry(text: &str) -> Result<(String, Vec<Package>)> {
    let doc = yaml_document(text.as_bytes()).ok_or_else(|| bad("not readable as YAML"))?;
    let root = doc.root();
    let version = root.get("__metadata").and_then(|m| m.get("version")).and_then(YamlRef::as_str);
    let mut out = Vec::new();
    for (key, entry) in root.entries() {
        if key == "__metadata" {
            continue;
        }
        let resolution = entry.get("resolution").and_then(YamlRef::as_str).unwrap_or_else(|| key.clone());
        let (name, reference) = split_at_version(&resolution);
        let version = entry.get("version").and_then(YamlRef::as_str).unwrap_or_default();
        let source = match reference.split_once(':') {
            Some(("npm", _)) | None => None,
            Some((protocol, rest)) => Some(format!("{protocol}:{}", rest.split("::").next().unwrap_or(rest))),
        };
        out.push(pkg(name, version, source));
    }
    Ok((format!("yarn.lock (Berry, v{})", version.unwrap_or_else(|| "?".into())), out))
}

// ---- pnpm ----

fn pnpm(data: &[u8]) -> Result<(String, Vec<Package>)> {
    let doc = yaml_document(data).ok_or_else(|| bad("not readable as YAML"))?;
    let root = doc.root();
    let version = root.get("lockfileVersion").and_then(YamlRef::as_str).ok_or_else(|| bad("no lockfileVersion"))?;
    let old = version.split('.').next().and_then(|v| v.parse::<u32>().ok()).is_some_and(|v| v < 6);
    let mut out = Vec::new();
    for (key, entry) in root.get("packages").into_iter().flat_map(YamlRef::entries) {
        let key = key.strip_prefix('/').unwrap_or(&key);
        // Peer dependency suffixes: `(react@18.2.0)` since v6, `_react@18.2.0` before.
        let key = key.split('(').next().unwrap_or(key);
        let (name, version) = if old {
            match key.rsplit_once('/') {
                Some((name, version)) => (name, version.split('_').next().unwrap_or(version)),
                None => (key, ""),
            }
        } else {
            split_at_version(key)
        };
        let resolution = entry.get("resolution");
        let source = resolution
            .and_then(|r| r.get("tarball").or(r.get("repo")).or(r.get("directory")))
            .and_then(YamlRef::as_str)
            .and_then(|s| npm_source(Some(&s)));
        let version = entry.get("version").and_then(YamlRef::as_str).unwrap_or_else(|| version.to_string());
        out.push(pkg(name, version, source));
    }
    Ok((format!("pnpm-lock.yaml v{version}"), out))
}

// ---- Rust and Python (TOML) ----

#[derive(Deserialize)]
struct TomlLock<P> {
    #[serde(default = "Vec::new")]
    package: Vec<P>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    source: Option<String>,
}

fn cargo(text: &str) -> Result<Vec<Package>> {
    let lock: TomlLock<CargoPackage> = toml::from_str(text).map_err(|e| bad(e.message()))?;
    Ok(lock
        .package
        .into_iter()
        .map(|p| {
            let source = match p.source.as_deref() {
                Some("registry+https://github.com/rust-lang/crates.io-index" | "sparse+https://index.crates.io/") => {
                    None
                }
                Some(s) => Some(s.to_string()),
                None => Some("path".to_string()),
            };
            pkg(p.name, p.version, source)
        })
        .collect())
}

#[derive(Deserialize)]
struct PoetryPackage {
    name: String,
    version: String,
    source: Option<BTreeMap<String, toml::Value>>,
}

fn poetry(text: &str) -> Result<Vec<Package>> {
    let lock: TomlLock<PoetryPackage> = toml::from_str(text).map_err(|e| bad(e.message()))?;
    Ok(lock
        .package
        .into_iter()
        .map(|p| {
            let source = p.source.map(|s| {
                let get = |k: &str| s.get(k).and_then(toml::Value::as_str).unwrap_or_default().to_string();
                let rev = get("resolved_reference");
                let rev = if rev.is_empty() { get("reference") } else { rev };
                let mut s = format!("{} {}", get("type"), get("url"));
                if !rev.is_empty() {
                    s = format!("{s}#{rev}");
                }
                s
            });
            pkg(p.name, p.version, source)
        })
        .collect())
}

#[derive(Deserialize)]
struct UvPackage {
    name: String,
    version: Option<String>,
    source: Option<BTreeMap<String, toml::Value>>,
}

fn uv(text: &str) -> Result<Vec<Package>> {
    let lock: TomlLock<UvPackage> = toml::from_str(text).map_err(|e| bad(e.message()))?;
    Ok(lock
        .package
        .into_iter()
        .map(|p| {
            let source = p.source.and_then(|s| {
                let (kind, value) = s.iter().next()?;
                let value = value.as_str().map_or_else(|| value.to_string(), str::to_string);
                (!(kind == "registry" && value == "https://pypi.org/simple")).then(|| format!("{kind} {value}"))
            });
            pkg(p.name, p.version.unwrap_or_default(), source)
        })
        .collect())
}

// ---- Pipenv and Composer (JSON) ----

#[derive(Deserialize)]
struct PipfileLock {
    #[serde(default)]
    default: BTreeMap<String, PipfileEntry>,
    #[serde(default)]
    develop: BTreeMap<String, PipfileEntry>,
}

#[derive(Deserialize)]
struct PipfileEntry {
    version: Option<String>,
    git: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
    path: Option<String>,
    file: Option<String>,
}

fn pipenv(data: &[u8]) -> Result<Vec<Package>> {
    let lock: PipfileLock = serde_json::from_slice(data).map_err(bad)?;
    Ok(lock
        .default
        .into_iter()
        .chain(lock.develop)
        .map(|(name, e)| {
            let version = e.version.as_deref().map(|v| v.trim_start_matches("==").to_string()).unwrap_or_default();
            let source = match (e.git, e.path, e.file) {
                (Some(git), _, _) => {
                    Some(format!("git {git}{}", e.reference.map(|r| format!("#{r}")).unwrap_or_default()))
                }
                (_, Some(path), _) => Some(format!("path {path}")),
                (_, _, Some(file)) => Some(format!("file {file}")),
                _ => None,
            };
            pkg(name, version, source)
        })
        .collect())
}

#[derive(Deserialize)]
struct ComposerLock {
    #[serde(default)]
    packages: Vec<ComposerPackage>,
    #[serde(rename = "packages-dev", default)]
    packages_dev: Vec<ComposerPackage>,
}

#[derive(Deserialize)]
struct ComposerPackage {
    name: String,
    version: String,
    source: Option<ComposerSource>,
}

#[derive(Deserialize)]
struct ComposerSource {
    reference: Option<String>,
}

fn composer(data: &[u8]) -> Result<Vec<Package>> {
    let lock: ComposerLock = serde_json::from_slice(data).map_err(bad)?;
    Ok(lock
        .packages
        .into_iter()
        .chain(lock.packages_dev)
        .map(|p| {
            // A branch (`dev-main`) is pinned by its commit.
            let commit = p.source.and_then(|s| s.reference).filter(|_| p.version.starts_with("dev-"));
            let version = match commit {
                Some(c) => format!("{}#{}", p.version, c.get(..12).unwrap_or(&c)),
                None => p.version,
            };
            pkg(p.name, version, None)
        })
        .collect())
}

// ---- Bundler and Go (line formats) ----

fn bundler(text: &str) -> Vec<Package> {
    let mut out = Vec::new();
    let (mut section, mut remote, mut revision) = ("", String::new(), String::new());
    for line in text.lines() {
        if !line.starts_with(' ') {
            section = line.trim();
            remote.clear();
            revision.clear();
            continue;
        }
        if !matches!(section, "GEM" | "GIT" | "PATH" | "PLUGIN SOURCE") {
            continue;
        }
        if let Some(r) = line.strip_prefix("  remote: ") {
            remote = r.trim().to_string();
        } else if let Some(r) = line.strip_prefix("  revision: ") {
            revision = r.trim().to_string();
        } else if let Some(spec) = line.strip_prefix("    ").filter(|s| !s.starts_with(' '))
            && let Some((name, version)) = spec.split_once(" (")
        {
            let source = match section {
                "GEM" if remote.trim_end_matches('/') == "https://rubygems.org" => None,
                "GIT" => Some(format!("git {remote}#{}", revision.get(..12).unwrap_or(&revision))),
                "PATH" => Some(format!("path {remote}")),
                _ => Some(format!("remote {remote}")),
            };
            out.push(pkg(name, version.trim_end_matches(')'), source));
        }
    }
    out
}

fn go_sum(text: &str) -> Vec<Package> {
    // module → version → whether the module's code (not only its go.mod) is listed.
    let mut modules: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(module), Some(version)) = (parts.next(), parts.next()) else { continue };
        let (version, code) = match version.strip_suffix("/go.mod") {
            Some(v) => (v, false),
            None => (version, true),
        };
        *modules.entry((module, version)).or_default() |= code;
    }
    modules.into_iter().map(|((m, v), code)| pkg(m, v, (!code).then(|| "go.mod only".to_string()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lockfile/", $name))
        };
    }

    fn packages(name: &str, data: &[u8]) -> String {
        let d = dependencies(name, data).unwrap();
        d.split("=== Packages ===\n").nth(1).unwrap().to_string()
    }

    const NPM: &str = "@types/node 20.11.0\nansi-styles 4.3.0\nchalk 4.1.2\ncolor-convert 2.0.1\ncolor-name 1.1.4\nhas-flag 4.0.0\nleft-pad 1.3.0\nms 2.1.3\nsupports-color 7.2.0\nundici-types 5.26.5\n";

    #[test]
    fn npm_yarn_and_pnpm_agree() {
        assert_eq!(packages("package-lock.json", fixture!("npm-v3/package-lock.json")), NPM);
        assert_eq!(packages("package-lock.json", fixture!("npm-v2/package-lock.json")), NPM);
        assert_eq!(packages("package-lock.json", fixture!("npm-v1/package-lock.json")), NPM);
        assert_eq!(packages("yarn.lock", fixture!("yarn-v1/yarn.lock")), NPM);
        assert_eq!(packages("pnpm-lock.yaml", fixture!("pnpm-v9/pnpm-lock.yaml")), NPM);
        assert_eq!(packages("pnpm-lock.yaml", fixture!("pnpm-v6/pnpm-lock.yaml")), NPM);
        assert_eq!(packages("pnpm-lock.yaml", fixture!("pnpm-v5/pnpm-lock.yaml")), NPM);
        let berry = dependencies("yarn.lock", fixture!("yarn-berry/yarn.lock")).unwrap();
        assert!(berry.starts_with("Lockfile:          yarn.lock (Berry, v8)\n"), "{berry}");
        assert!(berry.contains("\ndemo 0.0.0-use.local  (workspace:.)\n"), "{berry}");
        for line in NPM.lines() {
            assert!(berry.contains(&format!("\n{line}\n")), "{line}\n{berry}");
        }
        let header = dependencies("package-lock.json", fixture!("npm-v3/package-lock.json")).unwrap();
        assert!(header.starts_with("Lockfile:          package-lock.json v3\nPackages:          10\n\n"), "{header}");
    }

    #[test]
    fn an_update_shows_as_changed_lines() {
        let (a, b) = (
            packages("package-lock.json", fixture!("npm-v3/package-lock.json")),
            packages("package-lock.json", fixture!("npm-v3-updated/package-lock.json")),
        );
        let removed: Vec<&str> = a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect();
        let added: Vec<&str> = b.lines().filter(|l| !a.lines().any(|m| m == *l)).collect();
        assert_eq!(removed, ["ms 2.1.3"]);
        assert_eq!(added, ["debug 4.3.4", "ms 2.1.2"]);
    }

    #[test]
    fn similar_when_versions_match() {
        let (v2, v3) = (fixture!("npm-v2/package-lock.json"), fixture!("npm-v3/package-lock.json"));
        assert_eq!(same_packages("package-lock.json", v2, v3), Some(true));
        assert_eq!(same_packages("package-lock.json", v3, fixture!("npm-v3-updated/package-lock.json")), Some(false));
        assert_eq!(same_packages("package-lock.json", v3, b"{"), None);
    }

    #[test]
    fn other_ecosystems() {
        let c = dependencies("Cargo.lock", fixture!("cargo/Cargo.lock")).unwrap();
        assert!(c.contains("\nlocaldep 0.3.0  (path)\n") && c.contains("\nserde 1.0."), "{c}");
        assert!(c.contains("Sources:           path 2, registry "), "{c}");
        let p = dependencies("poetry.lock", fixture!("poetry/poetry.lock")).unwrap();
        assert!(p.contains("\nrequests 2.32.3\n") && p.contains("\nattrs 26.1.0\n"), "{p}");
        let u = dependencies("uv.lock", fixture!("uv/uv.lock")).unwrap();
        assert!(u.contains("\nrequests 2.32.3\n") && u.contains("\ndemo 0.1.0  (virtual .)\n"), "{u}");
        let pe = dependencies("Pipfile.lock", fixture!("pipenv/Pipfile.lock")).unwrap();
        assert!(pe.contains("\nrequests 2.32.3\n") && pe.contains("\npytest "), "{pe}");
        let co = dependencies("composer.lock", fixture!("composer/composer.lock")).unwrap();
        assert!(co.contains("\npsr/log 3.") && co.contains("\nmonolog/monolog 3."), "{co}");
        let g = dependencies("Gemfile.lock", fixture!("bundler/Gemfile.lock")).unwrap();
        assert!(g.contains("\nnokogiri 1.19.4-x86_64-linux-gnu\n") && g.contains("\nrack 3."), "{g}");
        let go = dependencies("go.sum", fixture!("go/go.sum")).unwrap();
        assert!(go.contains("\ngithub.com/google/uuid v1.6.0\n"), "{go}");
        assert_eq!(
            packages("go.sum", b"a v1 h1:x\na v1/go.mod h1:y\nb v2/go.mod h1:z\n"),
            "a v1\nb v2  (go.mod only)\n"
        );
    }

    #[test]
    fn detection_and_damaged_input() {
        assert_eq!(format_of("x/y/Cargo.lock"), Some(Format::Cargo));
        assert_eq!(format_of("package.json"), None);
        assert_eq!(format_of("my-yarn.lock"), None);
        for (name, data) in [
            ("package-lock.json", &fixture!("npm-v3/package-lock.json")[..]),
            ("Cargo.lock", fixture!("cargo/Cargo.lock")),
            ("pnpm-lock.yaml", fixture!("pnpm-v9/pnpm-lock.yaml")),
            ("yarn.lock", fixture!("yarn-berry/yarn.lock")),
            ("composer.lock", fixture!("composer/composer.lock")),
        ] {
            for n in [0, 1, 7, data.len() / 3, data.len() - 1] {
                let _ = dependencies(name, &data[..n]);
            }
            let _ = dependencies(name, b"\x00\xff garbage");
        }
        assert!(dependencies("Cargo.lock", b"[[package]]\nname = 1\n").is_err());
    }
}
