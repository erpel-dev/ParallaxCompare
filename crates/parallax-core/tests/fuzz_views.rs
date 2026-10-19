//! Every content view and similar rule survives text that is not valid in its
//! language: unbalanced brackets, stray quotes, unfinished comments.

use parallax_core::convert;

struct Random(u64);

impl Random {
    fn next(&mut self) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize
    }
}

#[test]
fn views_survive_broken_source() {
    let pieces = [
        "let",
        "fn",
        "x",
        "=",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "<",
        ">",
        "'",
        "\"",
        "`",
        "#",
        "//",
        "/*",
        "*/",
        "\\",
        "r#\"",
        "$",
        "@",
        ";",
        ",",
        ":",
        "::",
        ".",
        "->",
        "=>",
        "|",
        "&",
        "*",
        "use",
        "mod",
        "match",
        "def",
        "class",
        "sub",
        "my",
        "<#if",
        "${",
        "«",
        "»",
        "DEFINE",
        "ENDDEFINE",
        "\n",
        "\n    ",
        "\t",
        "0x",
        "1",
        "'a",
        "q{",
        "<<EOF",
        "EOF",
        "=pod",
        "=cut",
        "import",
        "export",
        "<div",
        "/>",
        "{%",
        "%}",
    ];
    let names = [
        "a.rs",
        "a.ts",
        "a.tsx",
        "a.js",
        "a.py",
        "a.pl",
        "a.pm",
        "a.xpt",
        "a.ext",
        "a.chk",
        "a.ftl",
        "a.rst",
        "a.toml",
        "a.yaml",
        "a.json",
        "a.csv",
        "conanfile.txt",
        "a.a2l",
        "a.dbc",
        "Cargo.lock",
        "a.ipynb",
        "a.xml",
        "a.arxml",
    ];
    let mut random = Random(3);
    for round in 0..300 {
        let text = |random: &mut Random| {
            let n = 1 + random.next() % 120;
            (0..n).map(|_| pieces[random.next() % pieces.len()]).collect::<Vec<_>>().join(" ")
        };
        let (a, b) = (text(&mut random), text(&mut random));
        for name in names {
            for view in convert::views_for(name, a.as_bytes()) {
                let _ = convert::render(view, name, a.as_bytes());
            }
            let _ = convert::content_equal(name, a.as_bytes(), b.as_bytes());
            let _ = convert::content_equal(name, a.as_bytes(), a.as_bytes());
        }
        let _ = round;
    }
}

#[test]
fn legacy_word_documents_render() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office");
    for name in ["doc-report.doc", "doc-plain.doc", "doc-mixed.doc"] {
        let data = std::fs::read(format!("{dir}/{name}")).unwrap();
        // Read natively, so this does not depend on LibreOffice.
        let text = convert::render(convert::View::Document, name, &data).unwrap();
        assert!(text.lines().count() >= 3, "{name}: {text}");
        let _ = convert::content_equal(name, &data, &data);
    }
}
