//! `parallax` — compare folders, archives, Git revisions and files from the terminal.
//!
//! Exit status follows `diff`: 0 identical, 1 different, 2 error.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use parallax_core::compare::{self, CompareMode, CompareOptions, Control, Node, Status};
use parallax_core::convert::{self, View};
use parallax_core::source::{Source, SourceOptions, SourceSpec, open_pair};
use parallax_core::text;
use parallax_core::textdiff::{self, DiffOptions, RowKind, WhitespaceMode};

#[derive(Parser)]
#[command(
    name = "parallax",
    version,
    about = "Compare folders, archives, Git revisions and files",
    after_help = "SOURCES:\n  path/to/folder        a folder or a file\n  release.7z            an archive (zip, 7z, tar.gz/xz/zst/bz2, jar, …)\n  release.7z!/bin       a folder inside an archive\n  repo@main             a Git branch, tag or commit (no checkout needed)\n  repo@v1.4:src/lib     a folder inside a Git revision\n\nEXAMPLES:\n  parallax ~/src/app@main ~/src/app@feature/x --merge-base\n  parallax dist/ release-1.4.7z\n  parallax old.txt new.txt"
)]
struct Cli {
    left: String,
    right: String,

    /// Compare a Git revision on the left against its merge-base with the right revision
    #[arg(long)]
    merge_base: bool,

    /// Compare by size and modification time only
    #[arg(long)]
    quick: bool,

    /// Treat CRLF and LF line endings as different
    #[arg(long)]
    strict_eol: bool,

    /// Compare documents and images by bytes only (no "similar" by text or pixels)
    #[arg(long)]
    bytes: bool,

    /// Exclude names or paths matching a glob (repeatable); replaces the default .git/.svn/.hg
    #[arg(long, short = 'x', value_name = "GLOB")]
    exclude: Vec<String>,

    /// Show archives as files instead of expanding them
    #[arg(long)]
    no_archives: bool,

    /// Do not apply .gitignore rules to local folders
    #[arg(long)]
    no_gitignore: bool,

    /// Also list identical entries
    #[arg(long, short = 'a')]
    all: bool,

    /// Machine-readable output
    #[arg(long)]
    json: bool,

    /// Whitespace handling for file compare
    #[arg(long, value_enum, default_value_t = Ws::Exact)]
    whitespace: Ws,

    /// Ignore case for file compare
    #[arg(long, short = 'i')]
    ignore_case: bool,

    /// Use the histogram diff algorithm instead of Myers for file compare
    #[arg(long)]
    histogram: bool,

    /// Compare files as raw text, without document, executable or firmware views
    #[arg(long)]
    raw: bool,

    /// Lines of context for file compare
    #[arg(long, short = 'U', default_value_t = 3)]
    context: usize,

    /// Colour output
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
}

#[derive(Clone, Copy, ValueEnum)]
enum Ws {
    Exact,
    Normalize,
    IgnoreAll,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

struct Paint(bool);

impl Paint {
    fn wrap(&self, code: &str, text: &str) -> String {
        if self.0 { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) => {
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("parallax: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<bool> {
    let source_opts = SourceOptions { expand_archives: !cli.no_archives, respect_gitignore: !cli.no_gitignore };
    let (left, right) =
        open_pair(&SourceSpec::parse(&cli.left), &SourceSpec::parse(&cli.right), source_opts, cli.merge_base)?;
    let paint = Paint(match cli.color {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    });
    match (left.is_file()?, right.is_file()?) {
        (true, true) => compare_files(cli, &left, &right, &paint),
        (false, false) => compare_folders(cli, &left, &right, &paint),
        _ => anyhow::bail!("cannot compare a file with a folder"),
    }
}

fn compare_folders(cli: &Cli, left: &Source, right: &Source, paint: &Paint) -> Result<bool> {
    let mut opts = CompareOptions {
        mode: if cli.quick { CompareMode::Quick } else { CompareMode::Content },
        ignore_line_endings: !cli.strict_eol,
        compare_contents: !cli.bytes,
        ..Default::default()
    };
    if !cli.exclude.is_empty() {
        opts.excludes = cli.exclude.clone();
    }
    let result = compare::compare(left, right, &opts, &Control::default())?;
    // Like `diff`, only differences that count under the active rules make the exit status 1.
    let identical = matches!(result.root.status, Status::Same | Status::Similar);
    let mut out = std::io::stdout().lock();
    if cli.json {
        serde_json::to_writer(&mut out, &result)?;
        writeln!(out)?;
        return Ok(identical);
    }
    print_tree(&mut out, &result.root, "", cli.all, paint)?;
    let s = &result.summary;
    writeln!(
        out,
        "\n{} files compared: {} same, {} similar, {} different, {} left only, {} right only{}  ({} ms{})",
        s.files,
        s.same,
        paint.wrap("33", &s.similar.to_string()),
        paint.wrap("31", &s.different.to_string()),
        paint.wrap("35", &s.left_only.to_string()),
        paint.wrap("36", &s.right_only.to_string()),
        if s.errors > 0 { format!(", {} errors", s.errors) } else { String::new() },
        s.elapsed_ms,
        if s.identical_subtrees > 0 {
            format!(", {} identical folders skipped via Git tree ids", s.identical_subtrees)
        } else {
            String::new()
        },
    )?;
    Ok(identical)
}

fn print_tree(out: &mut impl Write, node: &Node, prefix: &str, all: bool, paint: &Paint) -> Result<()> {
    for child in &node.children {
        if child.status == Status::Same && !all {
            continue;
        }
        let path = if prefix.is_empty() { child.name.clone() } else { format!("{prefix}/{}", child.name) };
        let is_dir = child.left.as_ref().or(child.right.as_ref()).is_some_and(|s| s.kind.is_container());
        let shown = if is_dir { format!("{path}/") } else { path.clone() };
        let line = match child.status {
            Status::Same => format!("  = {shown}"),
            Status::Similar => paint.wrap(
                "33",
                &format!("  ≈ {shown}{}", child.message.as_deref().map(|m| format!("  ({m})")).unwrap_or_default()),
            ),
            Status::Different => paint.wrap("31", &format!("  ≠ {shown}")),
            Status::LeftOnly => paint.wrap("35", &format!("  ← {shown}")),
            Status::RightOnly => paint.wrap("36", &format!("  → {shown}")),
            Status::Error => {
                paint.wrap("1;31", &format!("  ! {shown}: {}", child.message.as_deref().unwrap_or("error")))
            }
        };
        writeln!(out, "{line}")?;
        // Orphan folders are summarised by their top entry.
        if matches!(child.status, Status::Different | Status::Similar | Status::Error)
            || (all && child.status == Status::Same)
        {
            print_tree(out, child, &path, all, paint)?;
        }
    }
    Ok(())
}

fn compare_files(cli: &Cli, left: &Source, right: &Source, paint: &Paint) -> Result<bool> {
    let left_data = left.vfs.read(&left.root).with_context(|| format!("reading {}", left.root))?;
    let right_data = right.vfs.read(&right.root).with_context(|| format!("reading {}", right.root))?;
    let mut out = std::io::stdout().lock();
    if *left_data == *right_data {
        return Ok(true);
    }
    // Documents, executables, firmware and images are compared through their content view.
    let name = left.root.rsplit('/').next().unwrap_or_default();
    let view = if cli.raw {
        View::Text
    } else {
        convert::views_for_pair(Some((name, &left_data)), Some((name, &right_data)))[0]
    };
    if view == View::Image {
        let d = convert::image_diff(name, Some(&left_data), Some(&right_data))?;
        match (&d.note, d.different_pixels) {
            (Some(note), _) => writeln!(out, "Images differ: {note}")?,
            (None, 0) => writeln!(out, "Images have identical pixels; the files differ in encoding")?,
            (None, n) => writeln!(out, "Images differ in {n} of {} pixels, area {:?}", d.total_pixels, d.bounds)?,
        }
        return Ok(d.note.is_none() && d.different_pixels == 0);
    }
    if view == View::Hex || (view == View::Text && (text::is_binary(&left_data) || text::is_binary(&right_data))) {
        writeln!(out, "Binary files differ ({} vs {} bytes)", left_data.len(), right_data.len())?;
        return Ok(false);
    }
    let (a, b) = if view == View::Text {
        (text::decode(&left_data), text::decode(&right_data))
    } else {
        let render = |data: &[u8]| convert::render(view, name, data).with_context(|| format!("{} view", view.label()));
        (text::decode(render(&left_data)?.as_bytes()), text::decode(render(&right_data)?.as_bytes()))
    };
    if view != View::Text && !cli.json {
        writeln!(out, "Comparing as: {}", view.label())?;
    }
    let opts = DiffOptions {
        whitespace: match cli.whitespace {
            Ws::Exact => WhitespaceMode::Exact,
            Ws::Normalize => WhitespaceMode::Normalize,
            Ws::IgnoreAll => WhitespaceMode::IgnoreAll,
        },
        ignore_case: cli.ignore_case,
        histogram: cli.histogram,
    };
    let diff = textdiff::diff_docs(&a, &b, &opts);
    if cli.json {
        let value = serde_json::json!({
            "stats": diff.stats,
            "sections": diff.sections,
            "rows": diff.rows,
            "left": { "encoding": a.encoding, "lineEnding": a.line_ending },
            "right": { "encoding": b.encoding, "lineEnding": b.line_ending },
        });
        writeln!(out, "{value}")?;
        return Ok(diff.sections.is_empty());
    }
    if diff.sections.is_empty() {
        if a.line_ending != b.line_ending {
            writeln!(out, "Files differ only in line endings ({:?} vs {:?})", a.line_ending, b.line_ending)?;
        }
        return Ok(true);
    }
    writeln!(out, "{}", paint.wrap("1", &format!("--- {}", left.info.display)))?;
    writeln!(out, "{}", paint.wrap("1", &format!("+++ {}", right.info.display)))?;
    // Merge sections whose context overlaps, like `diff -u`.
    let rows = &diff.rows;
    let ctx = cli.context as u32;
    let mut groups: Vec<(u32, u32)> = Vec::new();
    for s in &diff.sections {
        let (start, end) = (s.start.saturating_sub(ctx), (s.end + ctx).min(rows.len() as u32));
        match groups.last_mut() {
            Some(last) if start <= last.1 => last.1 = end,
            _ => groups.push((start, end)),
        }
    }
    for (start, end) in groups {
        let slice = &rows[start as usize..end as usize];
        let count_l = slice.iter().filter(|r| r.left.is_some()).count();
        let count_r = slice.iter().filter(|r| r.right.is_some()).count();
        // Like `diff -u`: a side without lines in the hunk names the line before it.
        let before_l = rows[..start as usize].iter().filter(|r| r.left.is_some()).count();
        let before_r = rows[..start as usize].iter().filter(|r| r.right.is_some()).count();
        let first_l = before_l + usize::from(count_l > 0);
        let first_r = before_r + usize::from(count_r > 0);
        writeln!(out, "{}", paint.wrap("36", &format!("@@ -{first_l},{count_l} +{first_r},{count_r} @@")))?;
        // Within a section, print removed lines before added ones.
        let mut i = 0;
        while i < slice.len() {
            if matches!(slice[i].kind, RowKind::Equal | RowKind::Minor) {
                writeln!(out, " {}", a.line(slice[i].left.unwrap() as usize))?;
                i += 1;
                continue;
            }
            let j = slice[i..]
                .iter()
                .position(|r| matches!(r.kind, RowKind::Equal | RowKind::Minor))
                .map_or(slice.len(), |p| i + p);
            for row in &slice[i..j] {
                if let Some(l) = row.left {
                    writeln!(out, "{}", paint.wrap("31", &format!("-{}", a.line(l as usize))))?;
                }
            }
            for row in &slice[i..j] {
                if let Some(r) = row.right {
                    writeln!(out, "{}", paint.wrap("32", &format!("+{}", b.line(r as usize))))?;
                }
            }
            i = j;
        }
    }
    Ok(false)
}
