//! Hash & encrypt jobs: native pickers, digests, age encryption and decryption.
//!
//! Secrets cross this channel as plain `String`s. They arrive from a Slint text
//! property, which owns its own buffer and cannot be wiped, so zeroizing here
//! would protect nothing that is not already in the editor's memory.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use handybox_core::tools::crypto::{self, Checksum, KeyPair, Secret, Transfer};
use std::{path::PathBuf, sync::mpsc};

/// Which direction an age operation runs in. The two share a dialog, a staging
/// file and a report; only the core call and the suggested name differ.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Encrypt,
    Decrypt,
}

pub enum Command {
    Choose(Language),
    Checksum {
        path: PathBuf,
        expected: String,
    },
    Transfer {
        direction: Direction,
        language: Language,
        source: PathBuf,
        secret: Secret,
    },
    GenerateKey,
}

pub enum Event {
    Chosen(PathBuf),
    /// The save dialog is behind us and the work itself has started. The page
    /// only shows progress from here: a bar during a native dialog is a lie.
    Started,
    Checksummed(std::result::Result<Checksum, Failure>),
    /// `Ok(None)` when the save dialog was dismissed.
    Transferred(std::result::Result<Option<Transfer>, Failure>),
    Generated(KeyPair),
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Crypto(event));
    };
    match command {
        Command::Choose(language) => {
            let file = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose a file", "选择文件"))
                    .add_filter(language.text("All files", "所有文件"), &["*"])
                    .pick_file(),
            );
            match file {
                Some(file) => send(Event::Chosen(file.path().to_owned())),
                None => finish(events, Ok(Message::Cancelled)),
            }
        }
        Command::Checksum { path, expected } => send(Event::Checksummed(
            crypto::checksum(&path, &expected).map_err(Failure::crypto),
        )),
        Command::Transfer {
            direction,
            language,
            source,
            secret,
        } => transfer(direction, language, source, secret, events),
        Command::GenerateKey => send(Event::Generated(crypto::generate_key())),
    }
}

fn finish(events: &mpsc::Sender<Outgoing>, result: std::result::Result<Message, Failure>) {
    let _ = events.send(Outgoing::Finished(result));
}

fn transfer(
    direction: Direction,
    language: Language,
    source: PathBuf,
    secret: Secret,
    events: &mpsc::Sender<Outgoing>,
) {
    let send = |event| {
        let _ = events.send(Outgoing::Crypto(event));
    };
    let mut dialog = rfd::AsyncFileDialog::new();
    dialog = match direction {
        Direction::Encrypt => dialog
            .set_title(language.text("Save the encrypted file", "保存加密文件"))
            .set_file_name(crypto::encrypted_name(&source))
            .add_filter(
                language.text("age encrypted file", "age 加密文件"),
                &["age"],
            ),
        Direction::Decrypt => dialog
            .set_title(language.text("Save the decrypted file", "保存解密文件"))
            .set_file_name(crypto::decrypted_name(&source))
            .add_filter(language.text("All files", "所有文件"), &["*"]),
    };
    let Some(file) = futures_lite::future::block_on(dialog.save_file()) else {
        return send(Event::Transferred(Ok(None)));
    };
    send(Event::Started);
    // Do not silently change an extension after the native overwrite prompt:
    // the core refuses a destination it cannot name correctly instead.
    let result = match direction {
        Direction::Encrypt => crypto::encrypt(&source, file.path(), &secret),
        Direction::Decrypt => crypto::decrypt(&source, file.path(), &secret),
    };
    send(Event::Transferred(
        result.map(Some).map_err(Failure::crypto),
    ));
}
