use std::{
    collections::BTreeSet,
    fs,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Limits, find::run as find, grep::run as grep, ls::run as ls};
use crate::local_tools::discovery::output::MAX_OUTPUT_BYTES;

type Operation = fn(&Path, &Value, &CancellationToken, &Limits) -> ion_core::CodingToolOutput;

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ion-discovery-{name}-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&path).unwrap();
    path.canonicalize().unwrap()
}

fn run(
    operation: fn(&Path, &Value, &CancellationToken, &Limits) -> ion_core::CodingToolOutput,
    root: &Path,
    arguments: Value,
) -> Value {
    let output = operation(
        root,
        &arguments,
        &CancellationToken::new(),
        &Limits::default(),
    );
    assert!(!output.is_error, "{}", output.value);
    output.value
}

fn find_paths(value: &Value) -> BTreeSet<String> {
    value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap().to_owned())
        .collect()
}

fn grep_matches(value: &Value) -> &[Value] {
    value["matches"].as_array().unwrap()
}

#[test]
fn ls_lists_hidden_sorted_typed_entries_and_explicit_partial_subset() {
    let root = root("ls");
    fs::write(root.join("b.txt"), "b").unwrap();
    fs::write(root.join("a.txt"), "a").unwrap();
    fs::write(root.join(".hidden"), "h").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
    let fifo = root.join("pipe");
    let fifo_ready = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|status| status.success());

    let value = run(ls, &root, json!({}));
    let entries = value["entries"].as_array().unwrap();
    let names: Vec<_> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    let mut expected = vec![".hidden", "a.txt", "b.txt", "link", "sub"];
    if fifo_ready {
        expected.push("pipe");
        expected.sort_unstable();
    }
    assert_eq!(names, expected);
    assert_eq!(value["exhaustive"], true, "{value}");
    assert_eq!(value["root"], root.to_str().unwrap());
    for entry in entries {
        match entry["name"].as_str().unwrap() {
            "a.txt" | "b.txt" | ".hidden" => assert_eq!(entry["type"], "file"),
            "sub" => assert_eq!(entry["type"], "dir"),
            "link" => assert_eq!(entry["type"], "symlink"),
            "pipe" => assert_eq!(entry["type"], "other"),
            other => panic!("unexpected entry {other}"),
        }
    }

    let value = run(ls, &root, json!({"limit": 2}));
    let names: Vec<_> = value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec![".hidden", "a.txt"]);
    assert_eq!(value["returned"], 2);
    assert_eq!(value["exhaustive"], false);
    assert!(
        value["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice.as_str().unwrap().contains("entry limit 2"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn find_honors_basename_and_nested_path_globs_with_ignore_rules() {
    let base = root("find-ignore");
    let root = base.join("work");
    let outside = base.join("outside");
    fs::create_dir_all(root.join("src/nested/deep")).unwrap();
    fs::create_dir_all(root.join("ignored")).unwrap();
    fs::create_dir_all(root.join("ignored2")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("escaped.rs"), "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    fs::write(root.join(".gitignore"), "ignored/\n*.tmp\n!keep.tmp\n").unwrap();
    fs::write(root.join(".ignore"), "ignored2/\n").unwrap();
    fs::write(root.join(".git/secret.rs"), "secret\n").unwrap();
    fs::write(root.join("ignored/hidden.rs"), "ignored\n").unwrap();
    fs::write(root.join("ignored2/hidden.rs"), "ignored\n").unwrap();
    fs::write(root.join("src/a.rs"), "a\n").unwrap();
    fs::write(root.join("src/nested/b.rs"), "b\n").unwrap();
    fs::write(root.join("src/nested/deep/c.rs"), "c\n").unwrap();
    fs::write(root.join("top.rs"), "top\n").unwrap();
    fs::write(root.join(".hidden.rs"), "hidden\n").unwrap();
    fs::write(root.join("note.tmp"), "ignored\n").unwrap();
    fs::write(root.join("keep.tmp"), "kept\n").unwrap();

    let value = run(find, &root, json!({"pattern": "*.rs"}));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from([
            ".hidden.rs".into(),
            "src/a.rs".into(),
            "src/nested/b.rs".into(),
            "src/nested/deep/c.rs".into(),
            "top.rs".into(),
        ]),
        "{value}"
    );
    assert_eq!(value["exhaustive"], true, "{value}");

    let value = run(find, &root, json!({"pattern": "src/**/*.rs"}));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from([
            "src/a.rs".into(),
            "src/nested/b.rs".into(),
            "src/nested/deep/c.rs".into(),
        ]),
        "{value}"
    );
    let value = run(find, &root, json!({"pattern": "src/*.rs"}));
    assert_eq!(find_paths(&value), BTreeSet::from(["src/a.rs".into()]));

    let value = run(find, &root, json!({"pattern": "*.tmp"}));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from(["keep.tmp".into()]),
        "{value}"
    );

    let value = run(find, &root, json!({"pattern": "*.rs", "path": "src"}));
    assert!(value["root"].as_str().unwrap().ends_with("/src"));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from([
            "a.rs".into(),
            "nested/b.rs".into(),
            "nested/deep/c.rs".into()
        ])
    );

    let value = run(find, &root, json!({"pattern": "*.rs", "path": "escape"}));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from(["escaped.rs".into()]),
        "{value}"
    );
    let value = run(find, &root, json!({"pattern": "*"}));
    assert!(
        !find_paths(&value)
            .iter()
            .any(|path| path.starts_with("escape/")),
        "{value}"
    );
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn find_keeps_comment_and_negation_prefixes_as_literal_globs() {
    let root = root("find-prefixes");
    fs::write(root.join("#tag.rs"), "").unwrap();
    fs::write(root.join("!important.rs"), "").unwrap();
    fs::write(root.join("normal.rs"), "").unwrap();

    let value = run(find, &root, json!({"pattern": "#*"}));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from(["#tag.rs".into()]),
        "{value}"
    );
    assert_eq!(value["exhaustive"], true, "{value}");
    let value = run(find, &root, json!({"pattern": "!*.rs"}));
    assert_eq!(
        find_paths(&value),
        BTreeSet::from(["!important.rs".into()]),
        "{value}"
    );
    assert_eq!(value["exhaustive"], true, "{value}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn find_records_ignore_parser_failures_without_hiding_valid_files() {
    let root = root("find-ignore-error");
    fs::write(root.join("valid.rs"), "valid\n").unwrap();
    // An invalid character range makes the ignore library report a partial
    // parse failure on an otherwise traversable entry.
    fs::write(root.join(".gitignore"), "[z-a]\n").unwrap();
    let value = run(find, &root, json!({"pattern": "*.rs"}));
    assert!(find_paths(&value).contains("valid.rs"), "{value}");
    assert_eq!(value["exhaustive"], false, "{value}");
    assert!(
        value["skipped"]["filesystem_errors"].as_u64().unwrap() >= 1,
        "{value}"
    );
    assert!(
        !value["diagnostics"].as_array().unwrap().is_empty(),
        "{value}"
    );
    let value = run(grep, &root, json!({"pattern": "valid"}));
    assert_eq!(grep_matches(&value)[0]["path"], "valid.rs", "{value}");
    assert_eq!(value["exhaustive"], false, "{value}");
    assert!(
        value["skipped"]["filesystem_errors"].as_u64().unwrap() >= 1,
        "{value}"
    );
    assert!(
        !value["diagnostics"].as_array().unwrap().is_empty(),
        "{value}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn find_reports_result_caps_depth_and_non_utf8_paths_without_losing_partial_results() {
    let root = root("find-bounds");
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("a.rs"), "a\n").unwrap();
    fs::write(root.join("nested/b.rs"), "b\n").unwrap();
    let invalid = root.join(std::ffi::OsString::from_vec(b"bad\xff.rs".to_vec()));
    let invalid_ready = fs::write(&invalid, "bad\n").is_ok();

    let value = run(find, &root, json!({"pattern": "*.rs", "limit": 1}));
    assert_eq!(value["returned"], 1);
    assert_eq!(value["exhaustive"], false);
    assert!(
        value["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice.as_str().unwrap().contains("result limit 1"))
    );

    let limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    let output = find(
        &root,
        &json!({"pattern": "*.rs"}),
        &CancellationToken::new(),
        &limits,
    );
    let value = output.value;
    assert!(!output.is_error, "{value}");
    assert!(find_paths(&value).contains("a.rs"), "{value}");
    assert_eq!(value["exhaustive"], false);
    assert!(value["omissions"].as_array().unwrap().iter().any(|notice| {
        notice
            .as_str()
            .unwrap()
            .contains("maximum traversal depth 1")
    }));

    if invalid_ready {
        let value = run(find, &root, json!({"pattern": "*.rs"}));
        assert!(
            value["skipped"]["non_utf8_paths"].as_u64().unwrap() >= 1,
            "{value}"
        );
        assert_eq!(value["exhaustive"], false);
    }

    let limits = Limits {
        work: 2,
        ..Limits::default()
    };
    let output = find(
        &root,
        &json!({"pattern": "*"}),
        &CancellationToken::new(),
        &limits,
    );
    let value = output.value;
    assert!(!output.is_error, "{value}");
    assert!(value["visited_entries"].as_u64().unwrap() <= 2, "{value}");
    assert_eq!(value["exhaustive"], false);
    assert!(
        value["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice.as_str().unwrap().contains("work budget"))
    );

    let value = find(
        &root,
        &json!({"pattern": "*", "limit": 1001}),
        &CancellationToken::new(),
        &Limits::default(),
    )
    .value;
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("between 1 and 1000")
    );
    let value = find(
        &root,
        &json!({"pattern": "", "limit": 1}),
        &CancellationToken::new(),
        &Limits::default(),
    )
    .value;
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("must not be empty")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn grep_distinguishes_regex_literal_case_utf8_and_glob_filters() {
    let root = root("grep-modes");
    fs::write(root.join("literal.txt"), "a.b\naxb\n").unwrap();
    fs::write(root.join("unicode.txt"), "café\nCAFÉ\n").unwrap();
    fs::write(root.join("script.rs"), "a.b in rust\n").unwrap();
    fs::write(
        root.join("binary.bin"),
        [0xff, 0xfe, b'n', b'e', b'e', b'd', b'l', b'e'],
    )
    .unwrap();
    fs::write(root.join("nul.txt"), b"before\0needle\nafter\n").unwrap();

    let value = run(grep, &root, json!({"pattern": "a.b"}));
    assert_eq!(grep_matches(&value).len(), 3, "{value}");
    assert_eq!(value["exhaustive"], false, "binary/NUL files are reported");
    assert_eq!(value["skipped"]["binary"], 2, "{value}");
    let value = run(grep, &root, json!({"pattern": "a.b", "literal": true}));
    assert_eq!(grep_matches(&value).len(), 2, "{value}");
    assert!(
        grep_matches(&value)
            .iter()
            .all(|item| item["text"].as_str().unwrap().contains("a.b"))
    );
    let value = run(
        grep,
        &root,
        json!({"pattern": "a.b", "literal": true, "glob": "*.rs", "path": "script.rs"}),
    );
    assert_eq!(grep_matches(&value).len(), 1, "{value}");
    assert_eq!(grep_matches(&value)[0]["path"], "script.rs");

    let value = run(grep, &root, json!({"pattern": "café"}));
    assert_eq!(grep_matches(&value).len(), 1, "{value}");
    assert_eq!(grep_matches(&value)[0]["line"], 1);
    assert_eq!(grep_matches(&value)[0]["text"], "café");
    let value = run(grep, &root, json!({"pattern": "café", "ignore_case": true}));
    assert_eq!(grep_matches(&value).len(), 2, "{value}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn grep_long_lines_return_a_bounded_snippet_and_exact_byte_location() {
    let root = root("grep-long");
    let prefix = "x".repeat(5000);
    let suffix = "y".repeat(5000);
    let line = format!("{prefix}needle{suffix}\n");
    fs::write(root.join("long.txt"), &line).unwrap();
    let value = run(
        grep,
        &root,
        json!({"pattern": "needle", "glob": "long.txt"}),
    );
    let item = &grep_matches(&value)[0];
    assert_eq!(item["path"], "long.txt");
    assert_eq!(item["line"], 1);
    assert_eq!(item["byte_offset"], 5000);
    assert_eq!(item["truncated"], true, "{item}");
    let snippet = item["text"].as_str().unwrap();
    assert!(snippet.len() <= 1024, "{} bytes", snippet.len());
    assert!(snippet.contains("needle"), "{snippet}");
    assert!(
        snippet.starts_with('…') && snippet.ends_with('…'),
        "{snippet}"
    );
    let bytes = fs::read(root.join("long.txt")).unwrap();
    assert_eq!(&bytes[5000..5006], b"needle");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn grep_respects_ignore_rules_symlinks_and_explicit_file_roots() {
    let base = root("grep-walk");
    let root = base.join("work");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::create_dir_all(root.join("ignored")).unwrap();
    fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("outside.txt"), "needle outside\n").unwrap();
    std::os::unix::fs::symlink(base.join("outside.txt"), root.join("escape.txt")).unwrap();
    fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    fs::write(root.join("nested/note.txt"), "first\nneedle here\n").unwrap();
    fs::write(root.join("ignored/note.txt"), "needle ignored\n").unwrap();
    fs::write(root.join("escape-dir"), "").unwrap();
    let value = run(grep, &root, json!({"pattern": "needle"}));
    let matches = grep_matches(&value);
    assert_eq!(matches.len(), 1, "{value}");
    assert_eq!(matches[0]["path"], "nested/note.txt");
    assert_eq!(matches[0]["line"], 2);
    assert_eq!(value["root_kind"], "directory");
    assert_eq!(value["exhaustive"], true, "{value}");

    let value = run(
        grep,
        &root,
        json!({"pattern": "^needle", "path": "nested/note.txt"}),
    );
    assert_eq!(grep_matches(&value).len(), 1, "{value}");
    assert_eq!(grep_matches(&value)[0]["path"], "note.txt");
    assert!(
        value["root"]
            .as_str()
            .unwrap()
            .ends_with("/nested/note.txt")
    );
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn grep_reports_match_file_byte_and_work_bounds() {
    let bounds = root("grep-bounds");
    fs::write(bounds.join("many.txt"), "needle one\nneedle two\n").unwrap();
    fs::write(bounds.join("large.txt"), "needle large\n").unwrap();

    let value = run(grep, &bounds, json!({"pattern": "needle", "limit": 1}));
    assert_eq!(value["returned"], 1);
    assert_eq!(value["exhaustive"], false);
    assert!(
        value["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice.as_str().unwrap().contains("match limit 1"))
    );

    let limits = Limits {
        grep_file_bytes: 4,
        ..Limits::default()
    };
    let output = grep(
        &bounds,
        &json!({"pattern": "needle"}),
        &CancellationToken::new(),
        &limits,
    );
    let value = output.value;
    assert!(!output.is_error, "{value}");
    assert_eq!(value["returned"], 0);
    assert_eq!(value["skipped"]["oversized"], 2, "{value}");
    assert_eq!(value["exhaustive"], false);
    assert!(
        value["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice.as_str().unwrap().contains("per-file byte bound"))
    );

    let limits = Limits {
        grep_total_bytes: 4,
        grep_file_bytes: 1024,
        ..Limits::default()
    };
    let output = grep(
        &bounds,
        &json!({"pattern": "needle"}),
        &CancellationToken::new(),
        &limits,
    );
    let value = output.value;
    assert!(!output.is_error, "{value}");
    assert!(
        value["skipped"]["byte_budget"].as_u64().unwrap() >= 1,
        "{value}"
    );
    assert_eq!(value["exhaustive"], false);

    let exact = root("grep-exact-budget");
    fs::write(exact.join("one.txt"), "needle\n").unwrap();
    fs::write(exact.join("two.txt"), "other!\n").unwrap();
    let limits = Limits {
        grep_total_bytes: 7,
        grep_file_bytes: 1024,
        ..Limits::default()
    };
    let output = grep(
        &exact,
        &json!({"pattern": "needle"}),
        &CancellationToken::new(),
        &limits,
    );
    let value = output.value;
    assert!(!output.is_error, "{value}");
    assert_eq!(value["inspected_bytes"], 7, "{value}");
    assert_eq!(value["skipped"]["byte_budget"], 0, "{value}");
    assert_eq!(value["exhaustive"], false, "{value}");
    assert!(
        value["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice
                .as_str()
                .unwrap()
                .contains("total inspected-byte budget 7")),
        "{value}"
    );
    fs::remove_dir_all(exact).unwrap();

    let limits = Limits {
        work: 2,
        ..Limits::default()
    };
    let output = grep(
        &bounds,
        &json!({"pattern": "needle"}),
        &CancellationToken::new(),
        &limits,
    );
    let value = output.value;
    assert!(!output.is_error, "{value}");
    assert!(value["visited_entries"].as_u64().unwrap() <= 2, "{value}");
    assert_eq!(value["exhaustive"], false);
    fs::remove_dir_all(bounds).unwrap();
}

#[test]
fn discovery_rejects_invalid_inputs_and_pre_cancelled_work() {
    let root = root("errors");
    fs::write(root.join("file.txt"), "needle\n").unwrap();

    let cases: Vec<(Operation, Value, &str)> = vec![
        (ls, json!({"limit": 0}), "between 1 and 1000"),
        (ls, json!({"limit": 1001}), "between 1 and 1000"),
        (ls, json!({"path": null}), "invalid ls arguments"),
        (find, json!({"pattern": "["}), "invalid glob"),
        (
            find,
            json!({"pattern": "*.txt", "limit": 0}),
            "between 1 and 1000",
        ),
        (find, json!({"pattern": null}), "invalid find arguments"),
        (grep, json!({"pattern": "["}), "invalid regex"),
        (grep, json!({"pattern": "x", "glob": "["}), "invalid glob"),
        (
            grep,
            json!({"pattern": "x", "ignore_case": null}),
            "invalid grep arguments",
        ),
        (
            grep,
            json!({"pattern": "x", "limit": 1001}),
            "between 1 and 1000",
        ),
        (
            grep,
            json!({"pattern": "x", "path": "missing"}),
            "cannot resolve search root",
        ),
    ];
    for (operation, arguments, expected) in cases {
        let stop = CancellationToken::new();
        let limits = Limits::default();
        let output = operation(&root, &arguments, &stop, &limits);
        assert!(output.is_error, "{} {arguments}", output.value);
        assert!(
            output.value["error"].as_str().unwrap().contains(expected),
            "{}",
            output.value
        );
    }
    let file_root = find(
        &root,
        &json!({"pattern": "*", "path": "file.txt"}),
        &CancellationToken::new(),
        &Limits::default(),
    );
    assert!(file_root.is_error);
    assert!(
        file_root.value["error"]
            .as_str()
            .unwrap()
            .contains("not a directory")
    );

    let stop = CancellationToken::new();
    stop.cancel();
    for (operation, arguments) in [
        (ls as Operation, json!({})),
        (find, json!({"pattern": "needle"})),
        (grep, json!({"pattern": "needle"})),
    ] {
        let limits = Limits::default();
        let output = operation(&root, &arguments, &stop, &limits);
        assert!(output.is_error, "{}", output.value);
        assert!(
            output.value["error"]
                .as_str()
                .unwrap()
                .contains("cancelled")
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discovery_error_outputs_are_bounded_for_untrusted_arguments() {
    let root = root("error-bounds");
    let huge = "x".repeat(100_000);
    let mut unknown = serde_json::Map::new();
    unknown.insert(huge.clone(), json!(1));
    let mut unknown_arguments = Value::Object(unknown);
    unknown_arguments["pattern"] = json!("*");

    let mut cases: Vec<(Operation, Value)> = vec![
        (ls, json!({"path": huge})),
        (ls, json!({"path": "\u{1}".repeat(4096)})),
        (ls, json!({"limit": huge})),
        (find, unknown_arguments),
        (grep, json!({"pattern": "x", "path": huge})),
    ];
    let huge_glob = "y".repeat(100_000);
    cases.push((grep, json!({"pattern": "x", "glob": huge_glob})));
    for (operation, arguments) in cases {
        let stop = CancellationToken::new();
        let limits = Limits::default();
        let output = operation(&root, &arguments, &stop, &limits);
        assert!(output.is_error, "{}", output.value);
        let serialized = serde_json::to_vec(&output.value).unwrap();
        assert!(
            serialized.len() <= MAX_OUTPUT_BYTES,
            "{} > {MAX_OUTPUT_BYTES}",
            serialized.len()
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discovery_output_stays_within_the_serialized_byte_bound() {
    let root = root("output");
    for index in 0..350 {
        let name = format!("{index:04}-{}", "x".repeat(215));
        fs::write(root.join(name), "").unwrap();
    }
    let value = run(find, &root, json!({"pattern": "*"}));
    assert_eq!(value["output_limited"], true, "{value}");
    assert_eq!(value["exhaustive"], false);
    assert!(value["returned"].as_u64().unwrap() < 350);
    let serialized = serde_json::to_vec(&value).unwrap();
    assert!(
        serialized.len() <= MAX_OUTPUT_BYTES,
        "{} > {MAX_OUTPUT_BYTES}",
        serialized.len()
    );
    fs::remove_dir_all(root).unwrap();
}
