//! FlatBuffers data uses the schema beside each source, including inside Git
//! revisions and archives. Text schemas are compiled in an isolated temporary
//! directory with `flatc`; BFBS schemas and buffers are decoded natively.

mod decode;
mod schema;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::source::Source;
use crate::vfs::{EntryKind, join, normalize, split_parent};
use crate::{Error, Result};
use schema::{Schema, bad};

const MAX_SCHEMA: usize = 16 * 1024 * 1024;

pub fn is_candidate(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "bin" | "fb" | "flatbuffer" | "flatbuffers"))
}

fn sidecars(source: &Source, rel: &str) -> Vec<String> {
    let path = source.path(rel);
    let (dir, name) = split_parent(&path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    ["bfbs", "fbs"].iter().map(|ext| join(dir, &format!("{stem}.{ext}"))).collect()
}

pub fn has_schema(source: &Source, rel: &str) -> bool {
    is_candidate(&source.path(rel))
        && sidecars(source, rel).iter().any(|p| source.vfs.stat(p).is_ok_and(|s| s.kind == EntryKind::File))
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Location {
    Disk(PathBuf),
    Virtual(String),
}

impl Location {
    fn name(&self) -> &str {
        match self {
            Self::Disk(p) => p.file_name().and_then(|s| s.to_str()).unwrap_or("schema"),
            Self::Virtual(p) => split_parent(p).1,
        }
    }
}

fn virtual_include(path: &str, include: &str) -> Result<String> {
    let mut parts = split_parent(path).0.split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>();
    let boundary = parts.iter().rposition(|p| p.ends_with('!')).map_or(0, |i| i + 1);
    for part in include.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() <= boundary {
                    return Err(bad("schema include leaves its source tree or archive"));
                }
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    normalize(&parts.join("/")).ok_or_else(|| bad("invalid schema include path"))
}

struct Bundle<'a> {
    source: &'a Source,
    seen: BTreeMap<Location, String>,
    files: BTreeMap<String, Vec<u8>>,
    bytes: usize,
}

impl Bundle<'_> {
    fn read(&mut self, location: &Location) -> Result<Vec<u8>> {
        let (size, bytes) = match location {
            Location::Disk(path) => {
                let size = std::fs::metadata(path).map_err(|e| Error::io(path.display().to_string(), e))?.len();
                if size > MAX_SCHEMA as u64 {
                    return Err(bad("schema exceeds the 16 MiB limit"));
                }
                (size, std::fs::read(path).map_err(|e| Error::io(path.display().to_string(), e))?)
            }
            Location::Virtual(path) => {
                let stat = self.source.vfs.stat(path)?;
                if stat.size > MAX_SCHEMA as u64 {
                    return Err(bad("schema exceeds the 16 MiB limit"));
                }
                (stat.size, self.source.vfs.read(path)?.to_vec())
            }
        };
        self.bytes =
            self.bytes.checked_add(bytes.len().max(size as usize)).ok_or_else(|| bad("schema size overflow"))?;
        if self.bytes > MAX_SCHEMA {
            return Err(bad("schema and includes exceed the 16 MiB limit"));
        }
        Ok(bytes)
    }

    fn visit(&mut self, location: Location) -> Result<String> {
        if let Some(name) = self.seen.get(&location) {
            return Ok(name.clone());
        }
        if self.seen.len() >= 256 {
            return Err(bad("schema has too many includes"));
        }
        let ext = location.name().rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
        if !matches!(ext.as_str(), "fbs" | "bfbs") {
            return Err(bad("select an .fbs or .bfbs schema"));
        }
        let name = format!("s{}.{}", self.seen.len(), ext);
        self.seen.insert(location.clone(), name.clone());
        let bytes = self.read(&location)?;
        if ext == "bfbs" {
            self.files.insert(name.clone(), bytes);
            return Ok(name);
        }
        let doc = crate::text::decode(&bytes);
        if !doc.encode().is_ok_and(|v| v == bytes) {
            return Err(bad("schema text has a damaged encoding"));
        }
        let text = doc.text.trim_start_matches('\u{feff}');
        let tokens = super::definitions::lex(text, true)?;
        let mut edits = Vec::new();
        for pair in tokens.windows(2) {
            if pair[0].raw != "include" || !pair[1].raw.starts_with('"') {
                continue;
            }
            let include: String = serde_json::from_str(&pair[1].raw).map_err(|_| bad("invalid include filename"))?;
            let include = include.replace('\\', "/");
            if include.contains(['\0', '\n', '\r']) || Path::new(&include).is_absolute() || include.contains(':') {
                return Err(bad("schema includes must use relative file paths"));
            }
            let child = match &location {
                Location::Disk(path) => Location::Disk(
                    std::fs::canonicalize(path.parent().unwrap_or(Path::new(".")).join(&include))
                        .map_err(|e| Error::io(include.clone(), e))?,
                ),
                Location::Virtual(path) => {
                    let child = virtual_include(path, &include)?;
                    let mut parent = split_parent(path).0;
                    while !parent.is_empty() {
                        if crate::vfs::archive::detect_format(split_parent(parent).1).is_some()
                            && self
                                .source
                                .vfs
                                .stat(parent)
                                .is_ok_and(|e| matches!(e.kind, EntryKind::File | EntryKind::Archive))
                            && !child.starts_with(&format!("{parent}/"))
                        {
                            return Err(bad("schema include leaves its archive"));
                        }
                        parent = split_parent(parent).0;
                    }
                    if let Some(end) = path.rfind("!/")
                        && !child.starts_with(&path[..end + 2])
                    {
                        return Err(bad("schema include leaves its archive"));
                    }
                    Location::Virtual(child)
                }
            };
            let target = self.visit(child)?;
            edits.push((pair[1].start, pair[1].end, serde_json::to_string(&target).expect("filename")));
        }
        let mut rewritten = text.to_string();
        for (start, end, target) in edits.into_iter().rev() {
            rewritten.replace_range(start..end, &target);
        }
        self.files.insert(name.clone(), rewritten.into_bytes());
        Ok(name)
    }
}

type SchemaCache = Mutex<VecDeque<([u8; 32], Arc<Schema>, usize)>>;
static CACHE: OnceLock<SchemaCache> = OnceLock::new();
static PREPARING: Mutex<()> = Mutex::new(());

fn prepare(bundle: &Bundle<'_>, root: &str) -> Result<Arc<Schema>> {
    let mut hash = Sha256::new();
    for (name, bytes) in &bundle.files {
        hash.update(name.len().to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(bytes.len().to_le_bytes());
        hash.update(bytes);
    }
    let key: [u8; 32] = hash.finalize().into();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((_, schema, _)) = cache.lock().iter().find(|(id, _, _)| *id == key) {
        return Ok(schema.clone());
    }
    // Avoid launching a compiler for every parallel file on a cold schema cache.
    let _preparing = PREPARING.lock();
    if let Some((_, schema, _)) = cache.lock().iter().find(|(id, _, _)| *id == key) {
        return Ok(schema.clone());
    }
    let compiled = if root.ends_with(".bfbs") {
        bundle.files[root].clone()
    } else {
        let temp = tempfile::Builder::new()
            .prefix("parallax-flatbuffers-")
            .tempdir()
            .map_err(|e| Error::io("temporary schema directory", e))?;
        for (name, bytes) in &bundle.files {
            std::fs::write(temp.path().join(name), bytes).map_err(|e| Error::io(name.clone(), e))?;
        }
        let mut cmd = Command::new("flatc");
        cmd.current_dir(temp.path()).args(["--binary", "--schema", "-o", ".", root]);
        crate::process::run(&mut cmd, "flatc", "flatc schema compilation", Duration::from_secs(30))?;
        let output = temp.path().join(root.replace(".fbs", ".bfbs"));
        if std::fs::metadata(&output).map_err(|e| Error::io("compiled schema", e))?.len() > MAX_SCHEMA as u64 {
            return Err(bad("compiled schema exceeds the 16 MiB limit"));
        }
        std::fs::read(output).map_err(|e| Error::io("compiled schema", e))?
    };
    let schema = Arc::new(Schema::read(&compiled)?);
    let mut entries = cache.lock();
    entries.push_front((key, schema.clone(), compiled.len()));
    while entries.len() > 8 || entries.iter().map(|(_, _, size)| *size).sum::<usize>() > MAX_SCHEMA {
        entries.pop_back();
    }
    Ok(schema)
}

/// Decode with a local override, or the source's own sidecar and includes.
pub fn data(source: &Source, rel: &str, bytes: &[u8], schema_path: Option<&str>) -> Result<String> {
    let location = if let Some(path) = schema_path {
        Location::Disk(std::fs::canonicalize(path).map_err(|e| Error::io(path, e))?)
    } else {
        let path = sidecars(source, rel)
            .into_iter()
            .find(|p| source.vfs.stat(p).is_ok_and(|e| e.kind == EntryKind::File))
            .ok_or_else(|| {
                bad("no matching schema found. Put a same-name .fbs/.bfbs beside the binary, or select a schema")
            })?;
        match source.vfs.local_path(&path) {
            Some(path) => {
                Location::Disk(std::fs::canonicalize(&path).map_err(|e| Error::io(path.display().to_string(), e))?)
            }
            None => Location::Virtual(path),
        }
    };
    let mut bundle = Bundle { source, seen: BTreeMap::new(), files: BTreeMap::new(), bytes: 0 };
    let root = bundle.visit(location)?;
    let schema = prepare(&bundle, &root)?;
    let value = decode::decode(&schema, bytes)?;
    let signature = Sha256::digest(serde_json::to_vec(&*schema).map_err(|e| bad(e.to_string()))?);
    let signature = signature.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(format!(
        "=== FlatBuffers data ===\nRoot: {}\nSchema: {signature}\n\n{}",
        schema.objects[schema.root].name,
        super::score::decoded_configuration(value)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SourceOptions, SourceSpec};
    const BFBS: &[u8] = include_bytes!("../../../tests/fixtures/score/config.bfbs");
    const ORIGINAL: &[u8] = include_bytes!("../../../tests/fixtures/score/data-v1.bin");
    const REPACKED: &[u8] = include_bytes!("../../../tests/fixtures/score/data-repacked.bin");
    const CHANGED: &[u8] = include_bytes!("../../../tests/fixtures/score/data-v2.bin");
    const PREFIXED: &[u8] = include_bytes!("../../../tests/fixtures/score/data-size-prefixed.bin");
    const UNKNOWN: &[u8] = include_bytes!("../../../tests/fixtures/score/data-unknown.bin");

    #[test]
    fn includes_resolve_parents_without_leaving_the_source_or_archive() {
        assert_eq!(virtual_include("schemas/config.fbs", "../types/common.fbs").unwrap(), "types/common.fbs");
        assert_eq!(
            virtual_include("bundle.zip!/schemas/config.fbs", "../common.fbs").unwrap(),
            "bundle.zip!/common.fbs"
        );
        assert!(virtual_include("config.fbs", "../common.fbs").is_err());
        assert!(virtual_include("bundle.zip!/schemas/config.fbs", "../../common.fbs").is_err());
    }

    #[test]
    fn native_decoding_preserves_exact_values_structs_enums_arrays_and_unions() {
        let schema = Schema::read(BFBS).unwrap();
        let a = decode::decode(&schema, ORIGINAL).unwrap();
        let b = decode::decode(&schema, REPACKED).unwrap();
        assert_ne!(ORIGINAL, REPACKED);
        assert_eq!(a, b);
        assert_eq!(a, decode::decode(&schema, PREFIXED).unwrap());
        assert_ne!(a, decode::decode(&schema, CHANGED).unwrap());
        let view = a.compact();
        for expected in
            ["9007199254740993", "10.5", "Demo.Mode.ON", "Grüße 🚙", "hello", "-32768", "32767", "1.25", "9", "null"]
        {
            assert!(view.contains(expected), "{expected}\n{view}");
        }
    }

    #[test]
    fn unknown_fields_wrong_identifiers_and_damaged_schemas_are_rejected() {
        let schema = Schema::read(BFBS).unwrap();
        assert!(decode::decode(&schema, UNKNOWN).unwrap_err().to_string().contains("does not describe field"));
        let expanded = Schema::read(include_bytes!("../../../tests/fixtures/score/config-expanded.bfbs")).unwrap();
        assert!(decode::decode(&expanded, UNKNOWN).unwrap().compact().contains("\"extra\": 7"));
        let mut wrong = ORIGINAL.to_vec();
        wrong[4..8].copy_from_slice(b"FAIL");
        assert!(decode::decode(&schema, &wrong).unwrap_err().to_string().contains("identifier"));
        assert!(Schema::read(b"bad schema").is_err());
        for end in 0..ORIGINAL.len() {
            assert!(std::panic::catch_unwind(|| decode::decode(&schema, &ORIGINAL[..end])).is_ok());
        }
        for pos in 0..ORIGINAL.len() {
            let mut bytes = ORIGINAL.to_vec();
            bytes[pos] ^= 0xff;
            assert!(std::panic::catch_unwind(|| decode::decode(&schema, &bytes)).is_ok(), "buffer mutation at {pos}");
        }
        for pos in (0..BFBS.len()).step_by(7) {
            let mut bytes = BFBS.to_vec();
            bytes[pos] ^= 0xff;
            assert!(std::panic::catch_unwind(|| Schema::read(&bytes)).is_ok(), "schema mutation at {pos}");
        }
    }

    #[test]
    fn source_sidecars_and_schema_changes_are_read_in_their_own_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), ORIGINAL).unwrap();
        std::fs::write(dir.path().join("data.bfbs"), BFBS).unwrap();
        let source = SourceSpec::Local { path: dir.path().join("data.bin").display().to_string() }
            .open(SourceOptions::default())
            .unwrap();
        assert!(has_schema(&source, ""));
        let a = data(&source, "", ORIGINAL, None).unwrap();
        let b = data(&source, "", REPACKED, None).unwrap();
        assert_eq!(a, b);
        std::fs::write(
            dir.path().join("data.bfbs"),
            include_bytes!("../../../tests/fixtures/score/config-expanded.bfbs"),
        )
        .unwrap();
        assert_ne!(a, data(&source, "", ORIGINAL, None).unwrap());
        assert!(data(&source, "", ORIGINAL, Some("/parallax-nonexistent-schema.fbs")).is_err());
    }

    #[test]
    fn text_schema_compilation_supports_includes_or_explains_the_missing_tool() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/score");
        let source = SourceSpec::Local { path: fixture.join("data-v1.bin").display().to_string() }
            .open(SourceOptions::default())
            .unwrap();
        let result = data(&source, "", ORIGINAL, Some(fixture.join("config.fbs").to_str().unwrap()));
        if Command::new("flatc").arg("--version").output().is_ok_and(|o| o.status.success()) {
            let expected = result.unwrap();
            assert_eq!(
                expected,
                data(&source, "", ORIGINAL, Some(fixture.join("config.bfbs").to_str().unwrap())).unwrap()
            );
            // Includes keep the source's archive context, including ../ paths.
            use std::io::Write;
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("bundle.zip");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let text = std::fs::read_to_string(fixture.join("config.fbs"))
                .unwrap()
                .replace("types/common.fbs", "../types/common.fbs");
            let common = std::fs::read(fixture.join("types/common.fbs")).unwrap();
            for (name, bytes) in [
                ("schemas/config.bin", ORIGINAL),
                ("schemas/config.fbs", text.as_bytes()),
                ("types/common.fbs", common.as_slice()),
            ] {
                zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap();
            let packed =
                SourceSpec::Local { path: archive.display().to_string() }.open(SourceOptions::default()).unwrap();
            assert_eq!(expected, data(&packed, "schemas/config.bin", ORIGINAL, None).unwrap());
            let nested =
                SourceSpec::Local { path: dir.path().display().to_string() }.open(SourceOptions::default()).unwrap();
            assert_eq!(expected, data(&nested, "bundle.zip/schemas/config.bin", ORIGINAL, None).unwrap());
            // A malformed include must not resolve to a file outside that archive.
            let outside = text.replace("../types/common.fbs", "../../outside.fbs");
            let mut bundle = Bundle { source: &nested, seen: BTreeMap::new(), files: BTreeMap::new(), bytes: 0 };
            // Store a second archive whose only schema tries to leave it.
            let escaped = dir.path().join("escape.zip");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&escaped).unwrap());
            zip.start_file("schemas/config.fbs", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(outside.as_bytes()).unwrap();
            zip.finish().unwrap();
            assert!(
                bundle
                    .visit(Location::Virtual("escape.zip/schemas/config.fbs".into()))
                    .unwrap_err()
                    .to_string()
                    .contains("leaves its archive")
            );
        } else {
            assert!(result.unwrap_err().to_string().contains("flatc"));
        }
    }
}
