//! End to end: the built binary, a real headless Chrome, the fixtures.
//!
//! Every test checks the two things the tool promises - fewer elements, and a result that
//! is pixel-identical to the original - through the tool's own report. Without a Chrome on
//! the machine the tests pass vacuously and say so.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

struct Run {
    code: i32,
    out: String,
    report: Value,
    stderr: String,
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name).to_string_lossy().into_owned()
}

fn run(args: &[&str]) -> Option<Run> {
    let dir = tempfile::tempdir().unwrap();
    let (out, report) = (dir.path().join("out.html"), dir.path().join("report.json"));
    let o = Command::new(env!("CARGO_BIN_EXE_canon")).args(args).arg("-o").arg(&out).arg("--report").arg(&report).output().unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    if stderr.contains("no Chrome or Chromium found") {
        eprintln!("skipped: no Chrome on this machine");
        return None;
    }
    Some(Run {
        code: o.status.code().unwrap_or(-1),
        out: std::fs::read_to_string(&out).unwrap_or_default(),
        report: std::fs::read_to_string(&report).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null),
        stderr,
    })
}

fn elements(r: &Run, which: &str) -> u64 {
    r.report["elements"][which].as_u64().unwrap_or(u64::MAX)
}

fn assert_identical(r: &Run) {
    assert_eq!(r.code, 0, "exit status; stderr:\n{}", r.stderr);
    let v = &r.report["verification"];
    assert_eq!(v["exact"], true, "report: {v}");
    assert_eq!(v["pixelsVsOriginal"]["different"], 0, "report: {v}");
    assert_eq!(v["geometryMismatches"], 0);
    assert_eq!(v["styleMismatches"], 0);
}

#[test]
fn div_soup_collapses_to_the_card() {
    let Some(r) = run(&[&fixture("div-soup.html")]) else { return };
    assert_identical(&r);
    assert_eq!(elements(&r, "rendered"), 29);
    assert!(elements(&r, "output") <= 12, "{} elements left", elements(&r, "output"));
    // wrappers are gone, the content is not
    assert!(r.out.contains("Quarterly report") && r.out.contains("Open report"));
    // the visually hidden helper was removed, on a screenshot's say-so
    assert!(!r.out.contains("End of card"));
    // shorthands, not twelve border longhands
    assert!(r.out.contains("border:1px solid #d1d5db"), "{}", r.out);
}

#[test]
fn dashboard_keeps_tables_controls_svg_and_generated_content() {
    let Some(r) = run(&[&fixture("dashboard.html")]) else { return };
    assert_identical(&r);
    assert!(elements(&r, "output") <= 74, "{} elements left", elements(&r, "output"));
    for needle in ["<table", "<select", "<input", "<svg", "::before{content:", "::placeholder{color:#7c8aa0"] {
        assert!(r.out.contains(needle), "missing {needle}");
    }
    // an auto-sized pseudo-element is not pinned to the pixels it happened to resolve to
    assert!(!r.out.contains("220.359px"), "{}", r.out);
}

#[test]
fn editor_span_soup_becomes_plain_text_runs() {
    let Some(r) = run(&[&fixture("rich-text.html")]) else { return };
    assert_identical(&r);
    assert!(elements(&r, "output") <= 40, "{} elements left", elements(&r, "output"));
    assert!(r.out.contains("colspan=\"2\""));
}

#[test]
fn literal_markup_inline_fragment() {
    let markup = "<section style=\"font:14px Arial\"><div><div><p style=\"margin:0;padding:8px;background:#eef\"><span><b>Hello</b> world</span></p></div></div></section>";
    let Some(r) = run(&[markup, "--style", "inline", "--fragment"]) else { return };
    assert_identical(&r);
    assert_eq!(elements(&r, "output"), 2);
    assert!(r.out.starts_with("<div style="), "{}", r.out);
    assert!(!r.out.contains("<style") && !r.out.contains("<body"));
}

#[test]
fn no_reduce_only_minimises_css() {
    let Some(r) = run(&[&fixture("div-soup.html"), "--no-reduce"]) else { return };
    assert_identical(&r);
    assert_eq!(elements(&r, "output"), elements(&r, "rendered"));
}

#[test]
fn selector_picks_a_subtree() {
    let Some(r) = run(&[&fixture("dashboard.html"), "--selector", ".stats"]) else { return };
    assert_identical(&r);
    assert_eq!(r.report["rootTag"], "section");
    assert!(r.out.contains("1,284,112") && !r.out.contains("Recent deployments"));
}

#[test]
fn a_selector_that_matches_nothing_is_an_error() {
    let Some(r) = run(&[&fixture("div-soup.html"), "--selector", "#nope", "--timeout", "1"]) else { return };
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("nothing matches"), "{}", r.stderr);
}
