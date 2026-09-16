//! What a scan finds, and what stands between a selection and the trash.
//!
//! Nothing here calls `remove`: moving a file to the trash would leave it in
//! the trash of whoever ran the tests. What `remove` does before it trashes
//! anything is `verify`, and that is checked directly.
use handybox_core::tools::cleanup::{
    self, CleanupIssue, Limit, Mode, PARTIAL_BYTES, Plan, ScanOutcome, Target,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn write(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// The entries of one group, by the name each file was given.
fn names(outcome: &ScanOutcome, group: u32) -> Vec<String> {
    let mut found: Vec<String> = outcome
        .entries
        .iter()
        .filter(|entry| entry.group == Some(group))
        .map(|entry| {
            entry
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

#[test]
fn duplicates_are_found_by_content_rather_than_by_name_or_size() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    // The same content under two names, in different folders.
    write(&root.join("invoice.pdf"), b"the same bytes");
    write(&root.join("archive/copy of invoice.pdf"), b"the same bytes");
    // The same size, a different beginning: never read past the first block.
    write(&root.join("left.bin"), b"AAAAAAAAAAAAAA");
    write(&root.join("right.bin"), b"BBBBBBBBBBBBBB");
    // Long files that begin alike and end differently: the cheap hash puts
    // them together and the full read has to take them apart again.
    let mut shared = vec![b'x'; PARTIAL_BYTES as usize + 64];
    let mut other = shared.clone();
    *other.last_mut().unwrap() = b'y';
    write(&root.join("big-one.iso"), &shared);
    write(&root.join("big-two.iso"), &other);
    // …and two that really are the same, all the way down.
    shared.push(b'z');
    write(&root.join("media/song.flac"), &shared);
    write(&root.join("media/backup/song.flac"), &shared);

    let outcome = cleanup::scan(root, Mode::Duplicates).unwrap();
    assert_eq!(outcome.groups, 2, "{:?}", outcome.entries);
    assert!(outcome.stopped.is_none());
    assert_eq!(outcome.scanned, 8);
    // The most wasteful group is first: the pair of large files, not the pair
    // of small ones, whatever order the folder listed them in.
    assert_eq!(names(&outcome, 0), ["song.flac", "song.flac"]);
    assert_eq!(names(&outcome, 1), ["copy of invoice.pdf", "invoice.pdf"]);
    // One copy of each pair is what a cleanup would free.
    assert_eq!(outcome.reclaimable, shared.len() as u64 + 14);
    assert!(
        outcome
            .entries
            .iter()
            .all(|entry| !entry.directory && entry.group.is_some())
    );
}

#[test]
fn version_control_and_system_folders_are_left_alone() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("notes.txt"), b"same");
    // A repository keeps deliberate copies that mean something.
    write(&root.join(".git/objects/notes.txt"), b"same");
    let outcome = cleanup::scan(root, Mode::Duplicates).unwrap();
    assert_eq!(outcome.groups, 0);
    assert_eq!(outcome.scanned, 1, "the skipped folder was not walked");
}

#[test]
fn empty_files_and_folders_are_each_reported_once() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("empty.log"), b"");
    write(&root.join("kept/notes.txt"), b"something");
    fs::create_dir_all(root.join("cache/inner/deeper")).unwrap();
    fs::create_dir_all(root.join("alone")).unwrap();

    let outcome = cleanup::scan(root, Mode::Empty).unwrap();
    let found: Vec<(String, bool)> = outcome
        .entries
        .iter()
        .map(|entry| {
            (
                entry
                    .path
                    .strip_prefix(root.canonicalize().unwrap())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                entry.directory,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("empty.log".to_owned(), false),
            ("alone".to_owned(), true),
            // The tree is named at its top: removing it takes the rest, and
            // asking for the same folder three times is not three findings.
            ("cache".to_owned(), true),
        ]
    );
    // A folder holding a file is not empty, and neither is the root.
    assert!(
        !outcome
            .entries
            .iter()
            .any(|entry| entry.path.ends_with("kept"))
    );
    assert_eq!(outcome.reclaimable, 0, "removing nothing frees nothing");
}

#[test]
fn large_lists_the_biggest_first() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("small.bin"), &[0u8; 10]);
    write(&root.join("huge.bin"), &[0u8; 5000]);
    write(&root.join("nested/medium.bin"), &[0u8; 900]);
    let outcome = cleanup::scan(root, Mode::Large).unwrap();
    let sizes: Vec<u64> = outcome.entries.iter().map(|entry| entry.size).collect();
    assert_eq!(sizes, [5000, 900, 10]);
    assert_eq!(outcome.reclaimable, 5910);
    assert!(outcome.entries.iter().all(|entry| entry.group.is_none()));
}

#[test]
fn a_scan_that_is_asked_to_stop_says_so_instead_of_answering() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("one.txt"), b"copy");
    write(&root.join("two.txt"), b"copy");
    // Raised before the walk begins: nothing is read, and what comes back is
    // marked as the answer to a question nobody is asking any more.
    let outcome = cleanup::scan_until(root, Mode::Duplicates, &AtomicBool::new(true)).unwrap();
    assert_eq!(outcome.stopped, Some(Limit::Cancelled));
    assert!(outcome.entries.is_empty() && outcome.scanned == 0);
    // The same folder, left alone, still answers.
    let outcome = cleanup::scan_until(root, Mode::Duplicates, &AtomicBool::new(false)).unwrap();
    assert_eq!(outcome.stopped, None);
    assert_eq!(outcome.groups, 1);
}

#[test]
fn a_missing_folder_and_a_file_are_both_refused() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("document.txt");
    write(&file, b"not a folder");
    let issue = cleanup::scan(&file, Mode::Duplicates).unwrap_err();
    assert_eq!(
        issue.downcast_ref::<CleanupIssue>().copied(),
        Some(CleanupIssue::NotFolder)
    );
    let missing = cleanup::scan(&directory.path().join("nowhere"), Mode::Empty).unwrap_err();
    assert_eq!(
        missing.downcast_ref::<CleanupIssue>().copied(),
        Some(CleanupIssue::InputMissing)
    );
}

#[test]
fn a_plan_always_leaves_one_copy_of_every_group() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("one.txt"), b"copy");
    write(&root.join("two.txt"), b"copy");
    write(&root.join("three.txt"), b"copy");
    let outcome = cleanup::scan(root, Mode::Duplicates).unwrap();
    assert_eq!(outcome.entries.len(), 3);

    assert_eq!(
        cleanup::plan(&outcome, &[])
            .unwrap_err()
            .downcast_ref::<CleanupIssue>()
            .copied(),
        Some(CleanupIssue::NothingSelected)
    );
    // Selecting every copy is the one thing this tool must never do.
    assert_eq!(
        cleanup::plan(&outcome, &[0, 1, 2])
            .unwrap_err()
            .downcast_ref::<CleanupIssue>()
            .copied(),
        Some(CleanupIssue::WholeGroup)
    );
    let plan = cleanup::plan(&outcome, &[0, 1]).unwrap();
    assert_eq!(plan.targets.len(), 2);
    assert_eq!(plan.freed, 8);
    // An empty selection has no plan, whatever it was made of.
    assert!(Plan::default().is_empty());
}

#[test]
fn nothing_is_trashed_that_is_not_still_what_the_scan_saw() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("report.txt");
    write(&path, b"twelve bytes");
    let target = Target {
        path: path.clone(),
        size: 12,
        directory: false,
    };
    cleanup::verify(&root, &target).unwrap();

    // Grown, shrunk or replaced since the scan: leave it alone.
    fs::write(&path, b"a different length").unwrap();
    assert!(cleanup::verify(&root, &target).is_err());
    fs::remove_file(&path).unwrap();
    assert!(cleanup::verify(&root, &target).is_err());

    // Outside the folder that was scanned — only reachable through a stale
    // result, which is exactly when it matters.
    let elsewhere = Target {
        path: PathBuf::from("/etc/hosts"),
        size: 0,
        directory: false,
    };
    assert_eq!(
        cleanup::verify(&root, &elsewhere)
            .unwrap_err()
            .downcast_ref::<CleanupIssue>()
            .copied(),
        Some(CleanupIssue::Outside)
    );

    // A folder that is no longer empty is no longer the folder that was found.
    let folder = root.join("cache");
    fs::create_dir_all(&folder).unwrap();
    let target = Target {
        path: folder.clone(),
        size: 0,
        directory: true,
    };
    cleanup::verify(&root, &target).unwrap();
    write(&folder.join("new.txt"), b"appeared");
    assert!(cleanup::verify(&root, &target).is_err());
}

#[cfg(unix)]
#[test]
fn links_are_never_followed_listed_or_removed() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("real/file.txt"), b"content");
    // A link to a file with the same content is not a second copy of it.
    symlink(root.join("real/file.txt"), root.join("link.txt")).unwrap();
    // A link that points at its own parent would walk forever if followed.
    symlink(root.join("real"), root.join("real/loop")).unwrap();

    let outcome = cleanup::scan(root, Mode::Duplicates).unwrap();
    assert_eq!(outcome.groups, 0);
    assert_eq!(outcome.scanned, 1);

    // And a link standing where a file was is refused at the last moment.
    let root = root.canonicalize().unwrap();
    let target = Target {
        path: root.join("link.txt"),
        size: 7,
        directory: false,
    };
    assert!(cleanup::verify(&root, &target).is_err());
}
