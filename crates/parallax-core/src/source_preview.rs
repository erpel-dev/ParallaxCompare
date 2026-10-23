//! Cheap source detection for editable path fields. Never contacts a remote or
//! extracts an archive; those operations happen when the user opens the source.

use std::path::Path;

use serde::Serialize;

use crate::source::{SourceOptions, SourceSpec, expand_home, split_archive_path};
use crate::vfs::archive::detect_format;
use crate::vfs::git::GitRepo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewKind {
    Empty,
    Folder,
    File,
    Archive,
    ArchivePath,
    GitRev,
    Remote,
    NeedsRepository,
    Error,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePreview {
    pub kind: PreviewKind,
    pub spec: Option<SourceSpec>,
    pub git_root: Option<String>,
    /// Scope to retain when choosing a revision for a local file or subfolder.
    pub git_subpath: String,
    /// None when inspecting it would require network access or archive extraction.
    pub is_file: Option<bool>,
    pub message: Option<String>,
}

impl SourcePreview {
    fn new(kind: PreviewKind) -> Self {
        Self { kind, spec: None, git_root: None, git_subpath: String::new(), is_file: None, message: None }
    }

    fn error(message: impl ToString) -> Self {
        Self { message: Some(message.to_string()), ..Self::new(PreviewKind::Error) }
    }
}

pub fn inspect(text: &str) -> SourcePreview {
    let text = expand_home(text.trim());
    if text.is_empty() {
        return SourcePreview::new(PreviewKind::Empty);
    }
    let path = Path::new(&text);
    let metadata_error = match std::fs::metadata(path) {
        Ok(meta) => {
            // Existing paths win even when their names contain @ or !.
            let archive = meta.is_file() && detect_format(&text).is_some();
            let kind = if meta.is_dir() {
                PreviewKind::Folder
            } else if archive {
                PreviewKind::Archive
            } else {
                PreviewKind::File
            };
            let mut result = SourcePreview::new(kind);
            result.is_file = Some(!meta.is_dir() && !archive);
            let start = if meta.is_dir() { path } else { path.parent().unwrap_or(path) };
            if let Ok(git) = GitRepo::discover(start) {
                let root = git.workdir().unwrap_or_else(|| git.git_dir());
                if let (Ok(full), Ok(base)) = (std::fs::canonicalize(path), std::fs::canonicalize(&root))
                    && let Ok(rel) = full.strip_prefix(base)
                {
                    result.git_subpath =
                        rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                }
                result.git_root = Some(root.display().to_string());
            }
            result.spec = Some(SourceSpec::Local { path: text });
            return result;
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return SourcePreview::error(format!("Cannot read {text}: {e}"));
        }
        // Windows rejects ':' in revision subpaths and URLs as a filename.
        // Interpret supported source syntax before reporting these errors.
        Err(e) => e,
    };
    // SourceSpec::parse may query branch names for a web link. Do not call it
    // until Compare: typing, detection and retrying local detection stay offline.
    if crate::remote::is_remote(&text) {
        return SourcePreview::new(PreviewKind::Remote);
    }
    let spec = if text.contains('@') { SourceSpec::parse(&text) } else { SourceSpec::Local { path: text.clone() } };
    if matches!(spec, SourceSpec::GitRev { .. }) {
        return inspect_spec(spec);
    }
    let (outer, inner) = split_archive_path(&text);
    if inner.is_some() && detect_format(outer).is_some() {
        return match std::fs::metadata(outer) {
            Ok(meta) if meta.is_file() => {
                SourcePreview { spec: Some(spec), ..SourcePreview::new(PreviewKind::ArchivePath) }
            }
            Ok(_) => SourcePreview::error(format!("{outer} is not an archive file")),
            Err(e) => SourcePreview::error(format!("Cannot read {outer}: {e}")),
        };
    }
    // A missing relative word can be either a path or a revision. Offer the
    // repository picker without claiming it is a valid branch or guessing HEAD.
    let possible_rev = !path.is_absolute()
        && !text.starts_with('.')
        && !text.contains(['\\', ':', '!', '@', ' '])
        && (path.extension().is_none() || text.starts_with('v'));
    if possible_rev {
        return SourcePreview {
            message: Some("Source not found locally. If this is a Git revision, choose its repository.".into()),
            ..SourcePreview::new(PreviewKind::NeedsRepository)
        };
    }
    SourcePreview::error(format!("Cannot read {text}: {metadata_error}. Check the path and your access, then retry."))
}

/// An explicitly chosen revision keeps its intent even if its text happens to
/// name a real local path. Typed paths still use `inspect` and its path priority.
pub fn inspect_spec(spec: SourceSpec) -> SourcePreview {
    match &spec {
        SourceSpec::GitRev { repo, .. } if crate::remote::is_remote(repo) => {
            SourcePreview { spec: Some(spec), ..SourcePreview::new(PreviewKind::Remote) }
        }
        SourceSpec::GitRev { repo, rev, subpath } => {
            let parts: Vec<_> = subpath.split('/').collect();
            let inside_archive = parts
                .iter()
                .take(parts.len().saturating_sub(1))
                .any(|p| detect_format(p.trim_end_matches('!')).is_some());
            let shape = if inside_archive {
                // Validate the revision, but do not open an archive to inspect a
                // descendant's shape while the user edits the source field.
                GitRepo::discover(repo).and_then(|git| git.resolve(rev)).map(|_| None)
            } else {
                spec.open(SourceOptions::default()).and_then(|s| s.is_file()).map(Some)
            };
            match shape {
                Ok(is_file) => SourcePreview {
                    git_root: Some(repo.clone()),
                    is_file,
                    spec: Some(spec),
                    ..SourcePreview::new(PreviewKind::GitRev)
                },
                Err(e) => SourcePreview { git_root: Some(repo.clone()), spec: Some(spec), ..SourcePreview::error(e) },
            }
        }
        SourceSpec::Local { path } | SourceSpec::GitWorktree { repo: path } => inspect(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_shapes_and_preserves_existing_special_names() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["repo@main", "release!", "folder.zip"] {
            let path = tmp.path().join(name);
            std::fs::create_dir(&path).unwrap();
            let result = inspect(path.to_str().unwrap());
            assert_eq!(result.kind, PreviewKind::Folder);
            assert_eq!(result.is_file, Some(false));
            assert!(matches!(result.spec, Some(SourceSpec::Local { .. })));
        }
        for (name, kind, is_file) in [("a.txt", PreviewKind::File, true), ("a.zip", PreviewKind::Archive, false)] {
            let path = tmp.path().join(name);
            std::fs::write(&path, b"").unwrap();
            let result = inspect(path.to_str().unwrap());
            assert_eq!(result.kind, kind);
            assert_eq!(result.is_file, Some(is_file));
        }
        // Does not try to decompress the sample (which is deliberately invalid).
        let inside = format!("{}/a.zip!/nested/file.txt", tmp.path().display());
        assert_eq!(inspect(&inside).kind, PreviewKind::ArchivePath);
        assert_eq!(inspect(&inside).is_file, None);
    }

    #[test]
    fn missing_inputs_are_not_silently_assigned_a_type() {
        assert_eq!(inspect("  ").kind, PreviewKind::Empty);
        assert_eq!(inspect("main").kind, PreviewKind::NeedsRepository);
        assert_eq!(inspect("feature/source-selection").kind, PreviewKind::NeedsRepository);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(inspect(tmp.path().join("missing.txt").to_str().unwrap()).kind, PreviewKind::Error);
    }

    #[test]
    fn remote_preview_never_resolves_or_fetches_a_link() {
        // These hosts cannot resolve; inspection must still finish locally.
        for url in ["https://unreachable.invalid/o/r/tree/feature/branch/file", "git@unreachable.invalid:o/r.git@main"]
        {
            let result = inspect(url);
            assert_eq!(result.kind, PreviewKind::Remote);
            assert!(result.spec.is_none());
            assert_eq!(result.is_file, None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn access_errors_are_not_converted_to_missing_paths_or_revision_hints() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main");
        std::fs::write(&file, b"content").unwrap();
        let permissions = std::fs::metadata(dir.path()).unwrap().permissions();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o0)).unwrap();
        let denied = std::fs::metadata(&file).is_err();
        let result = inspect(file.to_str().unwrap());
        std::fs::set_permissions(dir.path(), permissions).unwrap();
        if denied {
            assert_eq!(result.kind, PreviewKind::Error);
            assert!(result.message.unwrap().contains("Cannot read"));
        }
    }
}
