use handybox_core::tools::crypto::{self, Algorithm, CryptoIssue, Protection, Secret, Verdict};
use std::{fs, path::Path};

/// The published digests of the empty input: if the streaming loop ever stops
/// feeding one hasher, this is what notices.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const EMPTY_SHA512: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

/// 0x07 repeated over three read chunks and a partial fourth. Taken from
/// `shasum -a 256` and `shasum -a 512`, not from this crate: a test that
/// compares the loop against itself would agree with any chunking bug.
const PATTERN_BYTES: usize = 3 * 1024 * 1024 + 11;
const PATTERN_SHA256: &str = "5712b68d57efc6bfc532d522ce9471c2c263541b4ce4836ff00821c9b32c1feb";
const PATTERN_SHA512: &str = "e102c7d805b1e38d855671aa0e894dfc76b6b6f4fb296a446bfe1f9389b0a48b896390ed96e09883501f64bd3acc51b8c4fcb3235381f74a2f082cf02d77e58c";

#[test]
fn computes_both_digests_in_one_pass() {
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty.bin");
    fs::write(&empty, []).unwrap();
    let checksum = crypto::checksum(&empty, "").unwrap();
    assert_eq!(checksum.sha256, EMPTY_SHA256);
    assert_eq!(checksum.sha512, EMPTY_SHA512);
    assert_eq!(checksum.bytes, 0);
    assert!(checksum.verdict.is_none());
    assert_eq!(checksum.hex(Algorithm::Sha256), EMPTY_SHA256);
    assert_eq!(checksum.hex(Algorithm::Sha512), EMPTY_SHA512);

    // Several read chunks plus a partial one: the digest has to be the value
    // an outside tool computes, not merely a digest-shaped string. A loop that
    // fed a whole buffer instead of `&buffer[..read]`, or dropped the tail,
    // would still produce the right length.
    let big = directory.path().join("big.bin");
    fs::write(&big, vec![7u8; PATTERN_BYTES]).unwrap();
    let checksum = crypto::checksum(&big, "").unwrap();
    assert_eq!(checksum.bytes, PATTERN_BYTES as u64);
    assert_eq!(checksum.sha256, PATTERN_SHA256);
    assert_eq!(checksum.sha512, PATTERN_SHA512);
    // And the same file verifies against its own published checksum.
    assert_eq!(
        crypto::checksum(&big, PATTERN_SHA512).unwrap().verdict,
        Some(Verdict::Match(Algorithm::Sha512))
    );
}

#[test]
fn a_pasted_checksum_names_its_own_algorithm() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("empty.bin");
    fs::write(&file, []).unwrap();

    let matched = crypto::checksum(&file, EMPTY_SHA512).unwrap();
    assert_eq!(matched.verdict, Some(Verdict::Match(Algorithm::Sha512)));
    // Case and surrounding space are how a checksum arrives from a web page.
    let matched = crypto::checksum(&file, &format!("  {}  ", EMPTY_SHA256.to_uppercase())).unwrap();
    assert_eq!(matched.verdict, Some(Verdict::Match(Algorithm::Sha256)));
    // A whole line of `shasum` output: only the digest field is read.
    let line = format!("{EMPTY_SHA256}  empty.bin");
    assert_eq!(
        crypto::checksum(&file, &line).unwrap().verdict,
        Some(Verdict::Match(Algorithm::Sha256))
    );

    let wrong = EMPTY_SHA256.replace('e', "f");
    let mismatch = crypto::checksum(&file, &wrong).unwrap();
    assert_eq!(mismatch.verdict, Some(Verdict::Mismatch(Algorithm::Sha256)));

    // Neither length nor alphabet of a digest this tool can produce.
    for expected in ["not-a-checksum", "abc123", &EMPTY_SHA256.replace('e', "z")] {
        let error = crypto::checksum(&file, expected).unwrap_err();
        assert_eq!(
            error.downcast_ref::<CryptoIssue>(),
            Some(&CryptoIssue::Expected),
            "{expected}"
        );
    }
}

#[test]
fn checksum_reads_only_files_within_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    assert!(crypto::checksum(directory.path(), "").is_err());
    assert_eq!(
        crypto::checksum(&directory.path().join("missing.bin"), "")
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::InputMissing)
    );
}

#[test]
fn a_passphrase_round_trips_through_an_age_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("notes.txt");
    let plaintext = "私の秘密 · my secret\n";
    fs::write(&source, plaintext).unwrap();
    let encrypted = directory.path().join("notes.txt.age");
    let secret = Secret::Passphrase("correct horse battery staple".into());

    let sealed = crypto::encrypt(&source, &encrypted, &secret).unwrap();
    assert_eq!(sealed.protection, Protection::Passphrase);
    assert_eq!(sealed.input_bytes, plaintext.len() as u64);
    assert!(sealed.output_bytes > sealed.input_bytes, "header and MAC");
    // The ciphertext must not be the plaintext, and must be an age file.
    let bytes = fs::read(&encrypted).unwrap();
    assert!(bytes.starts_with(b"age-encryption.org/v1"));
    assert!(!bytes.windows(6).any(|window| window == b"secret"));

    let opened = directory.path().join("notes.txt");
    let opened = opened.with_file_name("opened.txt");
    let transfer = crypto::decrypt(&encrypted, &opened, &secret).unwrap();
    assert_eq!(transfer.protection, Protection::Passphrase);
    assert_eq!(transfer.output_bytes, plaintext.len() as u64);
    assert_eq!(fs::read_to_string(&opened).unwrap(), plaintext);

    // The right kind of secret, but not the right one.
    let wrong = Secret::Passphrase("incorrect horse".into());
    assert_eq!(
        crypto::decrypt(&encrypted, &opened, &wrong)
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::WrongSecret)
    );
    // An empty passphrase protects nothing, and is refused on both sides.
    let empty = Secret::Passphrase(String::new());
    assert_eq!(
        crypto::encrypt(&source, &directory.path().join("x.age"), &empty)
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::Passphrase)
    );
}

#[test]
fn a_generated_key_pair_round_trips_and_accepts_several_recipients() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("payload.bin");
    fs::write(&source, vec![3u8; 4096]).unwrap();
    let alice = crypto::generate_key();
    let bob = crypto::generate_key();
    assert!(alice.public.starts_with("age1"));
    assert!(alice.secret.starts_with("AGE-SECRET-KEY-1"));
    assert_ne!(alice.public, bob.public);

    // The way recipients are pasted: one per line, or separated by commas.
    let encrypted = directory.path().join("payload.bin.age");
    let recipients = Secret::Key(format!("{}\n{}", alice.public, bob.public));
    let sealed = crypto::encrypt(&source, &encrypted, &recipients).unwrap();
    assert_eq!(sealed.protection, Protection::Recipients(2));

    // Either recipient can open it, and neither needs the other's key.
    for identity in [&alice.secret, &bob.secret] {
        let opened = directory.path().join("opened.bin");
        crypto::decrypt(&encrypted, &opened, &Secret::Key(identity.clone())).unwrap();
        assert_eq!(fs::read(&opened).unwrap(), vec![3u8; 4096]);
    }
    let stranger = crypto::generate_key();
    assert_eq!(
        crypto::decrypt(
            &encrypted,
            &directory.path().join("nope.bin"),
            &Secret::Key(stranger.secret)
        )
        .unwrap_err()
        .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::WrongSecret)
    );
}

#[test]
fn keys_and_recipients_are_checked_before_anything_is_written() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("payload.bin");
    fs::write(&source, b"data").unwrap();
    let destination = directory.path().join("payload.bin.age");

    for keys in ["", "   ", "not-a-key", "age1bogus"] {
        let error = crypto::encrypt(&source, &destination, &Secret::Key(keys.into())).unwrap_err();
        assert_eq!(
            error.downcast_ref::<CryptoIssue>(),
            Some(&CryptoIssue::Recipient),
            "{keys}"
        );
    }
    assert!(!destination.exists(), "a refused run leaves no output");

    let key = crypto::generate_key();
    crypto::encrypt(&source, &destination, &Secret::Key(key.public)).unwrap();
    let error = crypto::decrypt(
        &destination,
        &directory.path().join("out.bin"),
        &Secret::Key("AGE-SECRET-KEY-1NOPE".into()),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::Identity)
    );
}

#[test]
fn says_which_kind_of_secret_a_file_actually_wants() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("payload.bin");
    fs::write(&source, b"data").unwrap();
    let output = directory.path().join("out.bin");
    let key = crypto::generate_key();

    let by_key = directory.path().join("by-key.age");
    crypto::encrypt(&source, &by_key, &Secret::Key(key.public)).unwrap();
    assert_eq!(
        crypto::decrypt(&by_key, &output, &Secret::Passphrase("anything".into()))
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::NeedsKey)
    );

    let by_passphrase = directory.path().join("by-passphrase.age");
    crypto::encrypt(
        &source,
        &by_passphrase,
        &Secret::Passphrase("a passphrase".into()),
    )
    .unwrap();
    assert_eq!(
        crypto::decrypt(&by_passphrase, &output, &Secret::Key(key.secret))
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::NeedsPassphrase)
    );

    // Something that was never encrypted at all.
    assert_eq!(
        crypto::decrypt(&source, &output, &Secret::Passphrase("a passphrase".into()))
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::NotEncrypted)
    );
}

#[test]
fn never_overwrites_the_file_it_is_reading() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("payload.age");
    fs::write(&source, b"original").unwrap();
    let secret = Secret::Passphrase("a passphrase".into());
    assert_eq!(
        crypto::encrypt(&source, &source, &secret)
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::SourceOverwrite)
    );
    // Encrypted output has to be named as such; nothing is written otherwise.
    let plain = directory.path().join("result.bin");
    assert_eq!(
        crypto::encrypt(&source, &plain, &secret)
            .unwrap_err()
            .downcast_ref::<CryptoIssue>(),
        Some(&CryptoIssue::InvalidExtension)
    );
    assert!(!plain.exists());
    assert_eq!(fs::read(&source).unwrap(), b"original");
}

#[test]
fn suggested_names_keep_the_original_and_never_propose_the_source() {
    assert_eq!(
        crypto::encrypted_name(Path::new("notes.pdf")),
        "notes.pdf.age"
    );
    assert_eq!(crypto::encrypted_name(Path::new("archive")), "archive.age");
    assert_eq!(
        crypto::decrypted_name(Path::new("notes.pdf.age")),
        "notes.pdf"
    );
    assert_eq!(crypto::decrypted_name(Path::new("secrets.AGE")), "secrets");
    // Nothing to strip, so the name is marked rather than reused as it is.
    assert_eq!(
        crypto::decrypted_name(Path::new("payload.bin")),
        "payload.bin-decrypted"
    );
}

#[test]
fn claims_only_paths_that_name_an_age_file() {
    for name in ["notes.age", "NOTES.AGE", "archive.tar.age"] {
        assert!(crypto::claims(Path::new(name)), "{name}");
    }
    for name in ["notes.txt", "age", "notes.age.txt", "data"] {
        assert!(!crypto::claims(Path::new(name)), "{name}");
    }
}
