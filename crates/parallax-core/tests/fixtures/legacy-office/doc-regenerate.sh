#!/bin/sh
# Regenerates the .doc fixtures and their .docx counterparts with LibreOffice.
set -e
cd "$(dirname "$0")"
soffice --headless --convert-to doc doc-report.fodt doc-plain.fodt
cargo test -p parallax-core regenerate_mixed -- --ignored
soffice --headless --convert-to docx doc-report.doc doc-plain.doc doc-mixed.doc
