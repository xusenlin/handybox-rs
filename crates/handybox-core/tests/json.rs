use handybox_core::tools::json::{
    self, JsonIssue, Layout, Limit, MAX_INPUT_BYTES, MAX_OUTPUTS, Mode, ValueKind,
};
use std::{fs, path::Path};

#[test]
fn formats_and_minifies_the_same_document() {
    let input = r#"{"b":[1,2],"a":"世界"}"#;
    let pretty = json::run(input, "", Layout::Pretty, Mode::Stream).unwrap();
    assert_eq!(
        pretty.text,
        "{\n  \"b\": [\n    1,\n    2\n  ],\n  \"a\": \"世界\"\n}"
    );
    assert_eq!(pretty.outputs, 1);
    assert_eq!(pretty.inputs, 1);
    assert!(pretty.limit.is_none());
    let compact = json::run(input, ".", Layout::Compact, Mode::Stream).unwrap();
    assert_eq!(compact.text, r#"{"b":[1,2],"a":"世界"}"#);
}

#[test]
fn applies_jq_filters_including_the_standard_library() {
    let input = r#"{"items":[{"n":"a","v":2},{"n":"b","v":40}]}"#;
    let outcome = json::run(
        input,
        ".items | map(.v) | add",
        Layout::Compact,
        Mode::Stream,
    )
    .unwrap();
    assert_eq!(outcome.text, "42");
    let outcome = json::run(input, ".items[] | .n", Layout::Compact, Mode::Stream).unwrap();
    assert_eq!(outcome.text, "\"a\"\n\"b\"");
    assert_eq!(outcome.outputs, 2);
    // `ascii_upcase` comes from jaq-std, `tojson` from jaq-json: both are loaded.
    let outcome = json::run(
        input,
        ".items[0].n | ascii_upcase",
        Layout::Compact,
        Mode::Stream,
    )
    .unwrap();
    assert_eq!(outcome.text, "\"A\"");
}

#[test]
fn keeps_integers_that_do_not_fit_a_double() {
    let input = "12345678901234567890123";
    assert_eq!(
        json::run(input, ".", Layout::Compact, Mode::Stream)
            .unwrap()
            .text,
        input
    );
}

#[test]
fn reads_a_stream_of_values_like_jq_does() {
    let input = "{\"a\":1}\n{\"a\":2}\n";
    let summary = json::validate(input, Mode::Stream).unwrap();
    assert_eq!(summary.values, 2);
    assert_eq!(summary.kind, ValueKind::Stream);
    let outcome = json::run(input, ".a", Layout::Compact, Mode::Stream).unwrap();
    assert_eq!(outcome.text, "1\n2");
    assert_eq!(outcome.inputs, 2);
}

#[test]
fn reports_the_line_and_column_of_a_syntax_error() {
    let error = json::run("{\n  \"a\": 1,\n}", "", Layout::Pretty, Mode::Stream).unwrap_err();
    assert_eq!(
        error.downcast_ref::<JsonIssue>(),
        Some(&JsonIssue::Syntax { line: 3, column: 1 })
    );
    assert!(error.to_string().contains("line 3"));
    assert!(json::validate("   ", Mode::Stream).is_err());
    assert!(json::validate("{}", Mode::Stream).is_ok());
}

#[test]
fn separates_filter_errors_from_input_errors() {
    let filter = json::run("{}", ".items[", Layout::Pretty, Mode::Stream).unwrap_err();
    assert_eq!(filter.downcast_ref::<JsonIssue>(), Some(&JsonIssue::Filter));
    assert!(format!("{filter:#}").contains("column"));
    let undefined = json::run("{}", "nosuchfilter", Layout::Pretty, Mode::Stream).unwrap_err();
    assert_eq!(
        undefined.downcast_ref::<JsonIssue>(),
        Some(&JsonIssue::Filter)
    );
    let runtime = json::run("{}", "1 + \"a\"", Layout::Pretty, Mode::Stream).unwrap_err();
    assert_eq!(runtime.downcast_ref::<JsonIssue>(), Some(&JsonIssue::Query));
}

#[test]
fn bounds_an_endless_filter_instead_of_running_forever() {
    let outcome = json::run("1", "repeat(.)", Layout::Compact, Mode::Stream).unwrap();
    assert_eq!(outcome.outputs, MAX_OUTPUTS);
    assert_eq!(outcome.limit, Some(Limit::Outputs));
}

#[test]
fn halt_ends_the_run_without_ending_the_process() {
    let outcome = json::run("1", "., halt, .", Layout::Compact, Mode::Stream).unwrap();
    assert_eq!(outcome.text, "1");
    assert_eq!(outcome.outputs, 1);
}

#[test]
fn summarizes_a_document_for_the_status_line() {
    let summary = json::validate(r#"{"a":1,"b":2,"c":3}"#, Mode::Stream).unwrap();
    assert_eq!(summary.kind, ValueKind::Object);
    assert_eq!(summary.entries, 3);
    assert_eq!(summary.values, 1);
    assert_eq!(
        json::validate("[1,2]", Mode::Stream).unwrap().kind,
        ValueKind::Array
    );
    assert_eq!(
        json::validate("null", Mode::Stream).unwrap().kind,
        ValueKind::Null
    );
    assert_eq!(
        json::validate("\"x\"", Mode::Stream).unwrap().kind,
        ValueKind::String
    );
}

#[test]
fn refuses_oversized_input() {
    let input = format!("[{}]", "1,".repeat(MAX_INPUT_BYTES as usize));
    let error = json::run(&input, "", Layout::Pretty, Mode::Stream).unwrap_err();
    assert_eq!(
        error.downcast_ref::<JsonIssue>(),
        Some(&JsonIssue::TooLarge)
    );
}

#[test]
fn loads_only_readable_utf8_files_within_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("payload.json");
    fs::write(&source, r#"{"a":1}"#).unwrap();
    let loaded = json::load(&source).unwrap();
    assert_eq!(loaded.text, r#"{"a":1}"#);
    assert!(json::load(directory.path()).is_err());
    assert!(json::load(&directory.path().join("missing.json")).is_err());
    let binary = directory.path().join("binary.json");
    fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(
        json::load(&binary).unwrap_err().downcast_ref::<JsonIssue>(),
        Some(&JsonIssue::NotText)
    );
}

#[test]
fn strict_mode_refuses_a_stream_but_keeps_everything_else() {
    let stream = "{\"a\":1}\n{\"a\":2}\n";
    let error = json::validate(stream, Mode::Single).unwrap_err();
    assert_eq!(
        error.downcast_ref::<JsonIssue>(),
        Some(&JsonIssue::Multiple)
    );
    assert!(format!("{error:#}").contains("2 top-level values"));
    assert!(
        json::run(stream, ".a", Layout::Compact, Mode::Single)
            .unwrap_err()
            .downcast_ref::<JsonIssue>()
            == Some(&JsonIssue::Multiple)
    );
    // One value is one document either way.
    assert_eq!(
        json::run("{\"a\":1}", ".a", Layout::Compact, Mode::Single)
            .unwrap()
            .text,
        "1"
    );
    // A filter that yields several outputs is not a stream input.
    assert_eq!(
        json::run("[1,2]", ".[]", Layout::Compact, Mode::Single)
            .unwrap()
            .outputs,
        2
    );
}

#[test]
fn claims_only_paths_that_name_a_json_document() {
    for name in [
        "a.json",
        "b.JSON",
        "c.jsonl",
        "d.ndjson",
        "e.geojson",
        "f.jsonc",
    ] {
        assert!(json::claims(Path::new(name)), "{name}");
    }
    // A text file may hold JSON, but its name does not say so; the converter
    // keeps everything it can actually read.
    for name in ["notes.txt", "report.docx", "data", "archive.json.zip"] {
        assert!(!json::claims(Path::new(name)), "{name}");
    }
}

#[test]
fn export_never_overwrites_the_source_and_requires_a_json_extension() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.json");
    fs::write(&source, "original").unwrap();
    assert!(json::export(Some(&source), &source, "replacement").is_err());
    let invalid = directory.path().join("result.txt");
    assert!(json::export(Some(&source), &invalid, "{}").is_err());
    assert!(!invalid.exists());
    let output = directory.path().join("result.json");
    json::export(Some(&source), &output, "{}").unwrap();
    assert_eq!(fs::read_to_string(&output).unwrap(), "{}");
    // Text typed into the editor has no source file to protect.
    json::export(None, &output, "[]").unwrap();
    assert_eq!(fs::read_to_string(output).unwrap(), "[]");
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
}

#[test]
fn preview_is_unicode_safe_and_keeps_the_full_result() {
    let input = format!("\"{}\"", "界".repeat(json::PREVIEW_CHARS));
    let outcome = json::run(&input, ".", Layout::Compact, Mode::Stream).unwrap();
    let (preview, truncated) = outcome.preview();
    assert!(truncated);
    assert_eq!(preview.chars().count(), json::PREVIEW_CHARS);
    assert_eq!(outcome.text.chars().count(), json::PREVIEW_CHARS + 2);
}
