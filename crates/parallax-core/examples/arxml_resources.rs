//! Reproducible large-tree render/diff workload. Run each revision in a fresh process
//! and collect peak resident memory with the operating system's process accounting.
use parallax_core::{
    convert::{self, View},
    text,
    textdiff::{self, DiffOptions},
};
use std::fmt::Write;
use std::time::Instant;
fn main() {
    let count: usize = std::env::args().nth(1).unwrap_or_else(|| "50000".into()).parse().unwrap();
    let mut left = String::from("<AUTOSAR><AR-PACKAGES><AR-PACKAGE><SHORT-NAME>VehiclePlatform</SHORT-NAME><ELEMENTS>");
    for i in 0..count {
        write!(left, "<APPLICATION-SW-COMPONENT-TYPE><SHORT-NAME>Component_{i:06}</SHORT-NAME><CATEGORY>APPLICATION</CATEGORY><PORTS><P-PORT-PROTOTYPE><SHORT-NAME>Status</SHORT-NAME><PROVIDED-INTERFACE-TREF DEST='SENDER-RECEIVER-INTERFACE'>/Interfaces/Status</PROVIDED-INTERFACE-TREF></P-PORT-PROTOTYPE></PORTS></APPLICATION-SW-COMPONENT-TYPE>").unwrap();
    }
    left.push_str("</ELEMENTS></AR-PACKAGE></AR-PACKAGES></AUTOSAR>");
    let right = left.replacen("<CATEGORY>APPLICATION</CATEGORY>", "<CATEGORY>SERVICE</CATEGORY>", 1);
    let started = Instant::now();
    let project =
        |bytes: &[u8]| text::decode(convert::render(View::ArxmlStructure, "system.arxml", bytes).unwrap().as_bytes());
    let (a, b) = (project(left.as_bytes()), project(right.as_bytes()));
    let render_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let diff = textdiff::diff_docs(&a, &b, &DiffOptions { keyed: true, ..Default::default() });
    assert_eq!((diff.stats.changed, diff.stats.added, diff.stats.removed), (1, 0, 0));
    println!(
        "{{\"components\":{count},\"source_bytes\":{},\"view_bytes\":{},\"lines\":{},\"render_ms\":{render_ms},\"diff_ms\":{},\"changed\":{},\"rows\":{}}}",
        left.len(),
        a.text.len(),
        a.len(),
        started.elapsed().as_millis(),
        diff.stats.changed,
        diff.rows.len()
    );
}
