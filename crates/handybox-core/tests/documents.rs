use handybox_core::tools::documents::{self, ConvertedDocument, MAX_INPUT_BYTES, PREVIEW_CHARS};
use std::{fs, io::Write, path::PathBuf, time::Duration};

#[test]
fn csv_converts_unicode_tables_and_exports_full_result() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("report.CSV");
    let original = "Name,Language\nHandyBox,Rust\n世界,中文\n";
    fs::write(&source, original).unwrap();
    let document = documents::convert(&source).unwrap();
    assert_eq!(document.format, "Csv");
    assert!(document.markdown.contains("HandyBox"));
    assert!(document.markdown.contains("世界"));
    assert!(document.markdown.contains('|'));
    let output = directory.path().join(document.suggested_name());
    documents::export_markdown(&source, &output, &document.markdown).unwrap();
    assert_eq!(fs::read_to_string(output).unwrap(), document.markdown);
    assert_eq!(fs::read_to_string(source).unwrap(), original);
}

#[test]
fn detects_rtf_from_content_even_with_an_unknown_extension() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("note.data");
    fs::write(
        &source,
        br"{\rtf1\ansi Hello \b HandyBox\b0 .\par Second paragraph.}",
    )
    .unwrap();
    let document = documents::convert(&source).unwrap();
    assert_eq!(document.format, "Rtf");
    assert!(document.markdown.contains("HandyBox"));
    assert!(document.markdown.contains("Second paragraph"));
}

#[test]
fn converts_a_real_docx_container() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("document.docx");
    let mut archive = zip::ZipWriter::new(fs::File::create(&source).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    archive.start_file("[Content_Types].xml", options).unwrap();
    archive.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    archive.start_file("word/document.xml", options).unwrap();
    archive.write_all(br#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>HandyBox document fixture</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
    archive.finish().unwrap();
    assert!(
        documents::convert(&source)
            .unwrap()
            .markdown
            .contains("HandyBox document fixture")
    );
}

#[test]
fn rejects_missing_empty_unknown_and_malformed_files() {
    let directory = tempfile::tempdir().unwrap();
    assert!(documents::convert(&directory.path().join("missing.docx")).is_err());
    assert!(documents::convert(directory.path()).is_err());
    for (name, content) in [
        ("empty.csv", ""),
        ("unknown.bin", "random data"),
        ("broken.docx", "not a zip"),
    ] {
        let source = directory.path().join(name);
        fs::write(&source, content).unwrap();
        assert!(documents::convert(&source).is_err(), "accepted {name}");
    }
}

#[test]
fn refuses_oversized_input_before_parsing() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("large.csv");
    fs::File::create(&source)
        .unwrap()
        .set_len(MAX_INPUT_BYTES + 1)
        .unwrap();
    assert!(
        documents::convert(&source)
            .unwrap_err()
            .to_string()
            .contains("64 MiB")
    );
}

#[test]
fn export_never_overwrites_the_source_and_requires_markdown_extension() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "original").unwrap();
    assert!(documents::export_markdown(&source, &source, "replacement").is_err());
    let invalid = directory.path().join("result.csv");
    assert!(documents::export_markdown(&source, &invalid, "replacement").is_err());
    assert!(!invalid.exists());
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
}

#[test]
fn export_can_replace_a_confirmed_destination() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.csv");
    let output = directory.path().join("result.md");
    fs::write(&source, "original").unwrap();
    fs::write(&output, "old result").unwrap();
    documents::export_markdown(&source, &output, "new result").unwrap();
    assert_eq!(fs::read_to_string(output).unwrap(), "new result");
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
}

#[cfg(unix)]
#[test]
fn export_rejects_a_symlink_to_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.csv");
    let output = directory.path().join("result.md");
    fs::write(&source, "original").unwrap();
    std::os::unix::fs::symlink(&source, &output).unwrap();
    assert!(documents::export_markdown(&source, &output, "replacement").is_err());
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
}

#[test]
fn preview_is_unicode_safe_and_keeps_the_full_document() {
    let markdown = "界".repeat(PREVIEW_CHARS + 1);
    let document = ConvertedDocument {
        source: PathBuf::from("sample.csv"),
        markdown: markdown.clone(),
        format: "Csv".into(),
        input_bytes: 0,
        elapsed: Duration::ZERO,
    };
    let (preview, truncated) = document.preview();
    assert!(truncated);
    assert_eq!(preview.chars().count(), PREVIEW_CHARS);
    assert_eq!(document.markdown, markdown);
}

#[test]
fn catalog_has_unique_stable_routes_and_only_documents_is_ready() {
    use handybox_core::catalog::{TOOLS, ToolId};
    let mut keys = std::collections::HashSet::new();
    for tool in TOOLS {
        assert!(keys.insert(tool.key));
        assert_eq!(tool.available, tool.id == ToolId::Documents);
        assert!(!tool.libraries.is_empty());
    }
    assert_eq!(TOOLS.len(), 9);
}
