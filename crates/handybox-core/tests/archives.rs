//! What an archive says it holds, what actually comes out of it, and what this
//! tool refuses to write. The fixtures are built here: the tests pack the
//! folders they then read back, and write the hostile names by hand.
use handybox_core::tools::archives::{self, ArchiveIssue, Kind, MAX_ENTRIES};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn names<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> HashSet<String> {
    names.into_iter().map(Into::into).collect()
}

/// Every entry of a listing, which is what "extract all" means everywhere else.
fn everything(listing: &archives::Listing) -> HashSet<String> {
    listing
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

fn quiet(_done: usize) {}

/// A folder worth packing: nested, with something in every file, and one folder
/// with nothing in it at all.
fn folder(root: &Path) -> PathBuf {
    let source = root.join("project");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("empty")).unwrap();
    fs::write(source.join("README.md"), "# Project\n".repeat(200)).unwrap();
    fs::write(source.join("src/main.rs"), "fn main() {}\n".repeat(500)).unwrap();
    source
}

/// Pack a folder the way the tool does, and hand back the archive.
fn pack(folder: &Path, into: &Path, kind: Kind) -> PathBuf {
    let plan = archives::survey(folder).unwrap();
    let destination = into.join(archives::suggested_name(folder, kind));
    let wanted = plan
        .items
        .iter()
        .map(|item| item.name.clone())
        .collect::<HashSet<_>>();
    let creation = archives::create(
        &plan,
        &wanted,
        &destination,
        kind,
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap();
    assert_eq!(creation.path, destination);
    assert_eq!(creation.kind, kind);
    assert_eq!(creation.failed, 0);
    assert!(!creation.cancelled);
    destination
}

/// A ZIP written by hand, so the tests can record names no tool would produce.
fn hostile(path: &Path, entries: &[(&str, &str)]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, contents) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

/// A ZIP assembled byte by byte, because the thing under test is the bytes: no
/// writer will produce a name that is UTF-8 without saying so, or a name in a
/// code page that predates it. Stored, so there is no compressor in the way.
fn raw_zip(path: &Path, entries: &[(Vec<u8>, &str, bool)]) {
    let mut out: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    let mut count = 0u16;
    for (name, contents, utf8) in entries {
        let data = contents.as_bytes();
        let offset = out.len() as u32;
        let flags: u16 = if *utf8 { 0x0800 } else { 0 };
        let crc = crc32(data);
        let mut header = Vec::new();
        header.extend(0x04034b50u32.to_le_bytes());
        header.extend(20u16.to_le_bytes()); // version needed
        header.extend(flags.to_le_bytes());
        header.extend(0u16.to_le_bytes()); // stored
        header.extend([0, 0, 0x21, 0x58]); // 2024-01-01 00:00
        header.extend(crc.to_le_bytes());
        header.extend((data.len() as u32).to_le_bytes());
        header.extend((data.len() as u32).to_le_bytes());
        header.extend((name.len() as u16).to_le_bytes());
        header.extend(0u16.to_le_bytes()); // no extra field
        out.extend(&header);
        out.extend(name);
        out.extend(data);

        directory.extend(0x02014b50u32.to_le_bytes());
        directory.extend(20u16.to_le_bytes()); // version made by
        directory.extend(&header[4..30]);
        directory.extend(0u16.to_le_bytes()); // no comment
        directory.extend(0u16.to_le_bytes()); // first disk
        directory.extend(0u16.to_le_bytes()); // internal attributes
        directory.extend(0u32.to_le_bytes()); // external attributes
        directory.extend(offset.to_le_bytes());
        directory.extend(name);
        count += 1;
    }
    let start = out.len() as u32;
    out.extend(&directory);
    out.extend(0x06054b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend((directory.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    fs::write(path, out).unwrap();
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

/// The bug this tool existed for a day with: a ZIP whose names are UTF-8
/// without the flag that says so — which is what `zip`, Info-ZIP and macOS
/// write — was read through CP437, so `使用说明.md` was listed, and then
/// written to disk, as `Σ╜┐τö¿Φ»┤µÿÄ.md`. The file went somewhere; it just went
/// somewhere nobody could find.
#[test]
fn a_name_is_read_the_way_it_was_written_flag_or_no_flag() {
    let directory = tempfile::tempdir().unwrap();
    let chinese = "文档/使用说明.md";
    let (gbk, _, _) = encoding_rs::GBK.encode(chinese);

    let archive = directory.path().join("names.zip");
    raw_zip(
        &archive,
        &[
            // UTF-8 bytes, flagged, which is the easy half.
            (chinese.as_bytes().to_vec(), "flagged", true),
            // The same bytes, unflagged. Every archiver that writes UTF-8 and
            // forgets to say so lands here.
            ("未标记/说明.md".as_bytes().to_vec(), "unflagged", false),
            // Older than UTF-8, and with nothing in the file to say so.
            (gbk.into_owned(), "legacy", false),
            ("plain/ascii.txt".as_bytes().to_vec(), "ascii", false),
        ],
    );

    let listing = archives::list(&archive).unwrap();
    let listed: Vec<&str> = listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert!(listed.contains(&"文档/使用说明.md"), "{listed:?}");
    assert!(listed.contains(&"未标记/说明.md"), "{listed:?}");
    assert!(listed.contains(&"plain/ascii.txt"), "{listed:?}");
    // The GBK name and the flagged one are the same text, so it appears twice.
    assert_eq!(
        listed
            .iter()
            .filter(|name| **name == "文档/使用说明.md")
            .count(),
        2,
        "{listed:?}"
    );

    // And the name that is listed is the name that lands on disk.
    let into = directory.path().join("out");
    fs::create_dir(&into).unwrap();
    let extraction = archives::extract(
        &archive,
        &into,
        &names(["未标记/说明.md", "plain/ascii.txt"]),
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap();
    assert_eq!(extraction.files, 2, "{extraction:?}");
    assert_eq!(
        fs::read_to_string(into.join("未标记/说明.md")).unwrap(),
        "unflagged"
    );
    assert_eq!(
        fs::read_to_string(into.join("plain/ascii.txt")).unwrap(),
        "ascii"
    );

    // Two entries recording one name are two entries: the first is written and
    // the second finds the name taken. Reading them by name would have opened
    // the same one twice.
    let into = directory.path().join("twice");
    fs::create_dir(&into).unwrap();
    let extraction = archives::extract(
        &archive,
        &into,
        &names([chinese]),
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap();
    assert_eq!(extraction.files, 1, "{extraction:?}");
    assert_eq!(extraction.skipped, 1, "{extraction:?}");
    assert_eq!(
        fs::read_to_string(into.join(chinese)).unwrap(),
        "flagged",
        "the first entry of the two is the one that was written"
    );
}

/// The other half of the same question: what this tool writes has to come back
/// readable, from this tool and from anything else.
#[test]
fn a_chinese_name_survives_being_packed_and_read_back() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("项目");
    fs::create_dir_all(source.join("文档")).unwrap();
    fs::write(
        source.join("文档/使用说明.md").as_path(),
        "内容\n".repeat(50),
    )
    .unwrap();
    for kind in [Kind::Zip, Kind::SevenZ] {
        let archive = pack(&source, directory.path(), kind);
        let listing = archives::list(&archive).unwrap();
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["项目/文档/使用说明.md"],
            "{kind:?}"
        );
        let into = directory.path().join(format!("out-{}", kind.extension()));
        fs::create_dir(&into).unwrap();
        archives::extract(
            &archive,
            &into,
            &everything(&listing),
            &AtomicBool::new(false),
            &quiet,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(into.join("项目/文档/使用说明.md")).unwrap(),
            "内容\n".repeat(50),
            "{kind:?}"
        );
    }
}

#[test]
fn a_packed_folder_lists_what_went_into_it() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    for kind in [Kind::Zip, Kind::SevenZ] {
        let archive = pack(&source, directory.path(), kind);
        let listing = archives::list(&archive).unwrap();

        assert_eq!(listing.kind, kind);
        assert_eq!(listing.files, 2);
        // The folder with nothing in it is the one folder that has to be
        // recorded; the others are implied by the names under them.
        assert_eq!(listing.folders, 1);
        assert_eq!(listing.refused, 0);
        assert!(!listing.encrypted);
        assert_eq!(listing.left_out, 0);
        // The chosen folder's own name leads every entry, so unpacking gives
        // back one folder rather than loose files.
        assert!(
            listing
                .entries
                .iter()
                .all(|entry| entry.name.starts_with("project/")),
            "{:?}",
            listing.entries
        );
        // Listed by name, the way a file manager shows them.
        let listed: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        let mut sorted = listed.clone();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(listed, sorted);
        // Text that repeats is text that compresses: an archive bigger than
        // its contents would mean nothing was compressed at all.
        assert!(listing.size > listing.packed, "{listing:?}");
        assert!(listing.ratio() > 0.5, "{}", listing.ratio());
        assert!(
            listing
                .entries
                .iter()
                .all(|entry| entry.safe && !entry.encrypted)
        );
    }
}

#[test]
fn what_comes_out_is_what_went_in() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    for (index, kind) in [Kind::Zip, Kind::SevenZ].into_iter().enumerate() {
        let archive = pack(&source, directory.path(), kind);
        let listing = archives::list(&archive).unwrap();
        let into = directory.path().join(format!("out{index}"));
        fs::create_dir(&into).unwrap();

        let extraction = archives::extract(
            &archive,
            &into,
            &everything(&listing),
            &AtomicBool::new(false),
            &quiet,
        )
        .unwrap();

        assert_eq!(extraction.files, 2, "{extraction:?}");
        assert_eq!(
            extraction.skipped + extraction.refused + extraction.failed,
            0
        );
        assert!(!extraction.cancelled && !extraction.over_budget);
        for name in ["README.md", "src/main.rs"] {
            assert_eq!(
                fs::read(into.join("project").join(name)).unwrap(),
                fs::read(source.join(name)).unwrap(),
                "{name} in {kind:?}"
            );
        }
        // An empty folder survives the round trip as an empty folder.
        assert!(into.join("project/empty").is_dir());
        assert_eq!(
            extraction.bytes,
            fs::metadata(source.join("README.md")).unwrap().len()
                + fs::metadata(source.join("src/main.rs")).unwrap().len()
        );

        // Again, into the same folder: everything is already there, and this
        // tool replaces nothing.
        let again = archives::extract(
            &archive,
            &into,
            &everything(&listing),
            &AtomicBool::new(false),
            &quiet,
        )
        .unwrap();
        assert_eq!(again.files, 0);
        assert_eq!(again.skipped, 2, "{again:?}");
        assert_eq!(again.failed, 0);
        // And it can say which one, because a count on its own reads as "it did
        // not work" when the truth is that the file is already there.
        assert!(
            again
                .first_skipped
                .as_deref()
                .is_some_and(|name| name.starts_with("project/")),
            "{again:?}"
        );
    }
}

#[test]
fn only_the_chosen_entries_are_written() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    for (index, kind) in [Kind::Zip, Kind::SevenZ].into_iter().enumerate() {
        let archive = pack(&source, directory.path(), kind);
        let into = directory.path().join(format!("some{index}"));
        fs::create_dir(&into).unwrap();

        let extraction = archives::extract(
            &archive,
            &into,
            &names(["project/README.md"]),
            &AtomicBool::new(false),
            &quiet,
        )
        .unwrap();

        assert_eq!(extraction.files, 1);
        assert!(into.join("project/README.md").is_file());
        assert!(!into.join("project/src/main.rs").exists());
        // The parent folders of a chosen entry are made; the ones that hold
        // nothing chosen are not.
        assert!(!into.join("project/empty").exists());
    }
    // Nothing chosen is nothing to do, and says so rather than unpacking
    // everything.
    let archive = pack(&source, directory.path(), Kind::Zip);
    let into = directory.path().join("none");
    fs::create_dir(&into).unwrap();
    let refused = archives::extract(
        &archive,
        &into,
        &HashSet::new(),
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap_err();
    assert_eq!(
        refused.downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::NothingSelected)
    );
}

#[test]
fn a_name_that_climbs_out_of_the_folder_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("hostile.zip");
    hostile(
        &archive,
        &[
            ("../escaped.txt", "not yours"),
            ("nested/../../escaped.txt", "not yours either"),
            ("/etc/handybox.conf", "nor this"),
            ("C:/Windows/handybox.ini", "nor that"),
            ("kept.txt", "this one is fine"),
        ],
    );
    let listing = archives::list(&archive).unwrap();
    assert_eq!(listing.entries.len(), 5);
    assert_eq!(listing.refused, 4, "{:?}", listing.entries);

    let into = directory.path().join("out");
    fs::create_dir(&into).unwrap();
    let extraction = archives::extract(
        &archive,
        &into,
        &everything(&listing),
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap();

    assert_eq!(extraction.files, 1);
    assert_eq!(extraction.refused, 4, "{extraction:?}");
    assert_eq!(
        fs::read_to_string(into.join("kept.txt")).unwrap(),
        "this one is fine"
    );
    // Nothing was written beside the folder that was chosen, which is where
    // every one of those names was aiming.
    assert!(!directory.path().join("escaped.txt").exists());
    assert!(!into.join("etc").exists());
}

#[test]
fn the_rules_for_a_name_are_the_same_before_anything_is_written() {
    let into = Path::new("/tmp/out");
    assert_eq!(
        archives::destination(into, "docs/readme.md"),
        Some(into.join("docs/readme.md"))
    );
    // A trailing separator is a folder, and a `.` is a step that goes nowhere.
    assert_eq!(
        archives::destination(into, "docs/./images/"),
        Some(into.join("docs/images"))
    );
    // Windows wrote the separator the other way round.
    assert_eq!(
        archives::destination(into, "docs\\readme.md"),
        Some(into.join("docs/readme.md"))
    );
    for refused in [
        "../escape",
        "docs/../../escape",
        "/absolute",
        "C:/drive",
        "",
        "/",
        "..",
    ] {
        assert_eq!(archives::destination(into, refused), None, "{refused}");
    }
}

#[test]
fn a_file_that_is_not_an_archive_is_named_as_such() {
    let directory = tempfile::tempdir().unwrap();
    let text = directory.path().join("notes.zip");
    fs::write(&text, "PK is not enough").unwrap();
    let issue = archives::list(&text).unwrap_err();
    assert_eq!(
        issue.downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::NotArchive)
    );
    // A folder is not an archive either, and a missing file is missing.
    assert_eq!(
        archives::list(directory.path())
            .unwrap_err()
            .downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::NotFile)
    );
    assert_eq!(
        archives::list(&directory.path().join("nowhere.zip"))
            .unwrap_err()
            .downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::InputMissing)
    );

    // The bytes decide, not the extension: a 7z called .zip opens as a 7z.
    let source = folder(directory.path());
    let seven = pack(&source, directory.path(), Kind::SevenZ);
    let disguised = directory.path().join("disguised.zip");
    fs::rename(&seven, &disguised).unwrap();
    assert_eq!(archives::list(&disguised).unwrap().kind, Kind::SevenZ);

    assert!(archives::claims(Path::new("bundle.ZIP")));
    assert!(archives::claims(Path::new("bundle.7z")));
    assert!(!archives::claims(Path::new("bundle.tar.gz")));
}

#[test]
fn creating_an_archive_refuses_the_wrong_name_and_its_own_folder() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    let plan = archives::survey(&source).unwrap();
    let wanted: HashSet<String> = plan.items.iter().map(|item| item.name.clone()).collect();
    let issue = |destination: PathBuf, kind: Kind| {
        archives::create(
            &plan,
            &wanted,
            &destination,
            kind,
            &AtomicBool::new(false),
            &quiet,
        )
        .unwrap_err()
        .downcast_ref::<ArchiveIssue>()
        .copied()
    };

    // The extension has to be the format that is actually being written.
    assert_eq!(
        issue(directory.path().join("project.7z"), Kind::Zip),
        Some(ArchiveIssue::InvalidExtension)
    );
    assert_eq!(
        issue(directory.path().join("project"), Kind::Zip),
        Some(ArchiveIssue::InvalidExtension)
    );
    // An archive inside the folder it is packing would be asked to contain
    // itself.
    assert_eq!(
        issue(source.join("project.zip"), Kind::Zip),
        Some(ArchiveIssue::SourceOverwrite)
    );
    assert!(!source.join("project.zip").exists());
    // Nothing ticked is nothing to pack.
    assert_eq!(
        archives::create(
            &plan,
            &HashSet::new(),
            &directory.path().join("project.zip"),
            Kind::Zip,
            &AtomicBool::new(false),
            &quiet,
        )
        .unwrap_err()
        .downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::NothingSelected)
    );

    // A folder with nothing under it has nothing to pack.
    let bare = directory.path().join("bare");
    fs::create_dir(&bare).unwrap();
    assert_eq!(
        archives::survey(&bare)
            .unwrap_err()
            .downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::Empty)
    );
    // And a file is not a folder.
    assert_eq!(
        archives::survey(&source.join("README.md"))
            .unwrap_err()
            .downcast_ref::<ArchiveIssue>(),
        Some(&ArchiveIssue::NotFolder)
    );
}

#[test]
fn a_survey_names_files_the_way_the_archive_will() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    let plan = archives::survey(&source).unwrap();

    assert_eq!(plan.root, source.canonicalize().unwrap());
    assert_eq!(plan.left_out, 0);
    assert!(plan.items.len() <= MAX_ENTRIES);
    let listed: Vec<&str> = plan.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(
        listed,
        vec!["project/empty/", "project/README.md", "project/src/main.rs"]
    );
    assert_eq!(
        plan.size,
        fs::metadata(source.join("README.md")).unwrap().len()
            + fs::metadata(source.join("src/main.rs")).unwrap().len()
    );
    assert_eq!(
        archives::suggested_name(&source, Kind::SevenZ),
        "project.7z"
    );
}

#[test]
fn stopping_is_respected_in_both_directions() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    let plan = archives::survey(&source).unwrap();
    let wanted: HashSet<String> = plan.items.iter().map(|item| item.name.clone()).collect();
    let destination = directory.path().join("stopped.zip");

    let creation = archives::create(
        &plan,
        &wanted,
        &destination,
        Kind::Zip,
        &AtomicBool::new(true),
        &quiet,
    )
    .unwrap();
    // A stopped run still leaves a readable archive where it was asked to: an
    // empty one, and one that says it was stopped.
    assert!(creation.cancelled);
    assert_eq!(creation.files, 0);
    assert_eq!(archives::list(&destination).unwrap().entries.len(), 0);

    let archive = pack(&source, directory.path(), Kind::Zip);
    let listing = archives::list(&archive).unwrap();
    let into = directory.path().join("stopped");
    fs::create_dir(&into).unwrap();
    let extraction = archives::extract(
        &archive,
        &into,
        &everything(&listing),
        &AtomicBool::new(true),
        &quiet,
    )
    .unwrap();
    assert!(extraction.cancelled);
    assert_eq!(extraction.files, 0);
    assert_eq!(fs::read_dir(&into).unwrap().count(), 0);
}

#[test]
fn every_entry_carries_the_time_the_archive_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    for kind in [Kind::Zip, Kind::SevenZ] {
        let archive = pack(&source, directory.path(), kind);
        for entry in archives::list(&archive).unwrap().entries {
            let Some(modified) = entry.modified else {
                panic!("{kind:?} recorded no time for {}", entry.name);
            };
            // Shown as the archive reckons it, and only that far: a minute is
            // as precise as the oldest of these formats gets.
            assert_eq!(modified.len(), 16, "{modified}");
            assert!(
                modified.starts_with("20") && modified.as_bytes()[10] == b' ',
                "{modified}"
            );
            let (date, time) = modified.split_once(' ').unwrap();
            let parts: Vec<u32> = date.split('-').map(|part| part.parse().unwrap()).collect();
            assert!(
                (1..=12).contains(&parts[1]) && (1..=31).contains(&parts[2]),
                "{date}"
            );
            let clock: Vec<u32> = time.split(':').map(|part| part.parse().unwrap()).collect();
            assert!(clock[0] < 24 && clock[1] < 60, "{time}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_program_comes_back_out_a_program() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("tools");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("run.sh"), "#!/bin/sh\necho hello\n").unwrap();
    fs::write(source.join("notes.txt"), "plain\n").unwrap();
    fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();

    let archive = pack(&source, directory.path(), Kind::Zip);
    let listing = archives::list(&archive).unwrap();
    let executable: Vec<&str> = listing
        .entries
        .iter()
        .filter(|entry| entry.executable)
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(executable, vec!["tools/run.sh"]);

    let into = directory.path().join("out");
    fs::create_dir(&into).unwrap();
    archives::extract(
        &archive,
        &into,
        &everything(&listing),
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap();
    let mode = |name: &str| {
        fs::metadata(into.join("tools").join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("run.sh"), 0o755);
    // Everything else lands as an ordinary file, whatever the archive claimed.
    assert_eq!(mode("notes.txt") & 0o111, 0);
}

#[cfg(unix)]
#[test]
fn a_link_is_neither_packed_nor_unpacked() {
    let directory = tempfile::tempdir().unwrap();
    let source = folder(directory.path());
    let secret = directory.path().join("secret.txt");
    fs::write(&secret, "not in the archive").unwrap();
    std::os::unix::fs::symlink(&secret, source.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(directory.path(), source.join("upwards")).unwrap();

    // Following either link would pack a file from outside the folder, or the
    // folder's own parent.
    let plan = archives::survey(&source).unwrap();
    assert!(
        plan.items
            .iter()
            .all(|item| !item.name.contains("link") && !item.name.contains("upwards")),
        "{:?}",
        plan.items
    );

    // And an archive that holds one anyway is listed with it refused.
    let archive = directory.path().join("linked.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    writer
        .add_symlink(
            "escape",
            "../../../etc/passwd",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    writer.finish().unwrap();
    let listing = archives::list(&archive).unwrap();
    assert_eq!(listing.refused, 1);
    let into = directory.path().join("out");
    fs::create_dir(&into).unwrap();
    let extraction = archives::extract(
        &archive,
        &into,
        &everything(&listing),
        &AtomicBool::new(false),
        &quiet,
    )
    .unwrap();
    assert_eq!(extraction.refused, 1);
    assert_eq!(extraction.files, 0);
    assert!(!into.join("escape").exists());
}
