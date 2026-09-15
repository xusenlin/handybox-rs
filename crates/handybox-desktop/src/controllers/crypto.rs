//! Hash & encrypt: one source file, three operations, and the secret each needs.
//!
//! The retained outcome is the engine's own value, never rendered text, so a
//! language change re-renders a checksum or an encryption report in place.
use super::{Shell, kib};
use crate::{
    AppWindow, CryptoState,
    locale::{Language, Message},
    worker::{Command, crypto::Command as Job, crypto::Direction, crypto::Event},
};
use handybox_core::tools::crypto::{
    Algorithm, Checksum, KeyPair, Protection, Secret, Transfer, Verdict,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// Which question is being asked about the file. Stored in the UI as the index
/// of the segmented control, because that is all the page needs it for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Checksum,
    Encrypt,
    Decrypt,
}

impl Mode {
    fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Encrypt,
            2 => Self::Decrypt,
            _ => Self::Checksum,
        }
    }

    fn index(self) -> i32 {
        match self {
            Self::Checksum => 0,
            Self::Encrypt => 1,
            Self::Decrypt => 2,
        }
    }
}

/// The last thing this tool produced. Only one of them can be on screen: asking
/// a different question about the file discards the answer to the previous one.
#[derive(Default)]
enum Outcome {
    #[default]
    None,
    Checksum(Checksum),
    Transfer(Direction, Transfer),
    Key(KeyPair),
}

#[derive(Default)]
struct State {
    source: Option<PathBuf>,
    outcome: Outcome,
}

#[derive(Clone)]
pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
}

impl Controller {
    pub fn new(ui: &AppWindow) -> Self {
        Self {
            ui: ui.as_weak(),
            state: Rc::new(RefCell::new(State::default())),
        }
    }

    pub fn bind(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CryptoState>();

        let bound = shell.clone();
        view.on_choose(move || {
            bound.submit(
                Command::Crypto(Job::Choose(bound.language())),
                Message::Picking,
            );
        });
        let (bound, controller) = (shell.clone(), self.clone());
        // A digest is an answer to the question that was asked, so changing the
        // question puts the panel back to empty rather than leaving it stale.
        view.on_mode_selected(move |_| controller.clear(&bound));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_run(move || controller.run(&bound));
        let bound = shell.clone();
        view.on_generate_key(move || {
            bound.submit(Command::Crypto(Job::GenerateKey), Message::Running);
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy(move || {
            let text = controller.report(bound.language()).join("\n");
            if !text.is_empty() {
                bound.submit(Command::Copy(text), Message::Copying);
            }
        });
    }

    /// Adopt a path the user already chose, from a drop or the command line.
    /// `mode` is set only when the file's own name asked for one.
    pub fn open(&self, shell: &Shell, path: PathBuf, mode: Option<Mode>) {
        shell.navigate("crypto");
        if let (Some(ui), Some(mode)) = (self.ui.upgrade(), mode) {
            ui.global::<CryptoState>().set_mode(mode.index());
        }
        self.adopt(shell, path);
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CryptoState>();
        match event {
            Event::Chosen(path) => {
                self.adopt(shell, path);
                shell.finish(Ok(Message::Ready));
            }
            Event::Started => {
                view.set_working(true);
                shell.notify(match self.mode() {
                    Mode::Decrypt => Message::Decrypting,
                    _ => Message::Encrypting,
                });
            }
            Event::Checksummed(outcome) => {
                view.set_working(false);
                match outcome {
                    Ok(checksum) => {
                        self.state.borrow_mut().outcome = Outcome::Checksum(checksum);
                        self.refresh(shell.language());
                        shell.finish(Ok(Message::Ready));
                    }
                    Err(failure) => shell.finish(Err(failure)),
                }
            }
            Event::Transferred(outcome) => {
                view.set_working(false);
                match outcome {
                    Ok(Some(transfer)) => {
                        let destination = transfer.destination.clone();
                        let direction = match self.mode() {
                            Mode::Decrypt => Direction::Decrypt,
                            _ => Direction::Encrypt,
                        };
                        self.state.borrow_mut().outcome = Outcome::Transfer(direction, transfer);
                        self.refresh(shell.language());
                        shell.finish(Ok(Message::Saved(destination)));
                    }
                    Ok(None) => shell.finish(Ok(Message::Cancelled)),
                    Err(failure) => shell.finish(Err(failure)),
                }
            }
            Event::Generated(pair) => {
                // Offer the public half where it is about to be used, without
                // dropping recipients that were already there.
                let existing = view.get_recipients();
                let existing = existing.trim_end();
                view.set_recipients(
                    if existing.is_empty() {
                        pair.public.clone()
                    } else {
                        format!("{existing}\n{}", pair.public)
                    }
                    .into(),
                );
                self.state.borrow_mut().outcome = Outcome::Key(pair);
                self.refresh(shell.language());
                shell.finish(Ok(Message::KeyGenerated));
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CryptoState>();
        let lines = self.report(lang);
        let state = self.state.borrow();
        view.set_detail(match &state.source {
            Some(source) => source.display().to_string().into(),
            None => SharedString::from(lang.text(
                "Any file can be hashed, encrypted or decrypted",
                "任何文件都可以计算摘要、加密或解密",
            )),
        });
        view.set_has_result(!matches!(state.outcome, Outcome::None));
        view.set_result_title(
            match &state.outcome {
                // Empty until there is something to name: the page falls back to
                // a neutral title, rather than announcing an answer it lacks.
                Outcome::None => "",
                Outcome::Checksum(_) => lang.text("Checksums", "摘要"),
                Outcome::Transfer(Direction::Encrypt, _) => lang.text("Encrypted", "加密结果"),
                Outcome::Transfer(Direction::Decrypt, _) => lang.text("Decrypted", "解密结果"),
                Outcome::Key(_) => lang.text("Key pair", "密钥对"),
            }
            .into(),
        );
        view.set_result_lines(ModelRc::new(VecModel::from(
            lines
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )));
        view.set_stats(stats(&state.outcome, lang).into());
        let (status, tone) = status(&state.outcome, lang);
        view.set_status(status.into());
        view.set_status_tone(tone);
    }

    fn mode(&self) -> Mode {
        match self.ui.upgrade() {
            Some(ui) => Mode::from_index(ui.global::<CryptoState>().get_mode()),
            None => Mode::Checksum,
        }
    }

    /// Take a file as the input for whichever operation is selected.
    fn adopt(&self, shell: &Shell, path: PathBuf) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CryptoState>();
        view.set_filename(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
        );
        {
            let mut state = self.state.borrow_mut();
            state.source = Some(path);
            // A result belongs to the file it was computed from.
            state.outcome = Outcome::None;
        }
        self.refresh(shell.language());
    }

    fn clear(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        // Never carry an unmasked secret across to the next question.
        ui.global::<CryptoState>().set_reveal(false);
        self.state.borrow_mut().outcome = Outcome::None;
        self.refresh(shell.language());
    }

    fn run(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CryptoState>();
        let Some(source) = self.state.borrow().source.clone() else {
            return;
        };
        let mode = self.mode();
        let (command, message) = match mode {
            Mode::Checksum => (
                Job::Checksum {
                    path: source,
                    expected: view.get_expected().to_string(),
                },
                Message::Hashing,
            ),
            Mode::Encrypt | Mode::Decrypt => (
                Job::Transfer {
                    direction: match mode {
                        Mode::Decrypt => Direction::Decrypt,
                        _ => Direction::Encrypt,
                    },
                    language: shell.language(),
                    source,
                    secret: self.secret(mode),
                },
                // The native save dialog comes first; the work itself only
                // starts once it returns, and says so with its own event.
                Message::Saving,
            ),
        };
        let accepted = shell.submit(Command::Crypto(command), message);
        // `submit` refuses while another operation runs; do not claim otherwise.
        view.set_working(accepted && mode == Mode::Checksum);
    }

    fn secret(&self, mode: Mode) -> Secret {
        let Some(ui) = self.ui.upgrade() else {
            return Secret::Passphrase(String::new());
        };
        let view = ui.global::<CryptoState>();
        match mode {
            Mode::Encrypt if view.get_encrypt_with_key() => {
                Secret::Key(view.get_recipients().to_string())
            }
            Mode::Decrypt if view.get_decrypt_with_key() => {
                Secret::Key(view.get_identity().to_string())
            }
            _ => Secret::Passphrase(view.get_passphrase().to_string()),
        }
    }

    /// The panel's lines. Also what the copy action puts on the clipboard, so
    /// a checksum can be pasted straight into a message or a checksum file.
    fn report(&self, lang: Language) -> Vec<String> {
        let state = self.state.borrow();
        match &state.outcome {
            Outcome::None => Vec::new(),
            Outcome::Checksum(checksum) => Algorithm::ALL
                .into_iter()
                .map(|algorithm| format!("{}  {}", algorithm.label(), checksum.hex(algorithm)))
                .collect(),
            Outcome::Transfer(direction, transfer) => vec![
                format!(
                    "{}  {}",
                    match direction {
                        Direction::Encrypt => lang.text("Encrypted", "已加密"),
                        Direction::Decrypt => lang.text("Decrypted", "已解密"),
                    },
                    transfer
                        .source
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
                format!(
                    "{}  {}",
                    lang.text("Saved to", "已保存到"),
                    transfer.destination.display()
                ),
                format!(
                    "{}  {}",
                    match direction {
                        Direction::Encrypt => lang.text("Protected by", "保护方式"),
                        Direction::Decrypt => lang.text("Opened with", "打开方式"),
                    },
                    protection(*direction, transfer.protection, lang)
                ),
            ],
            Outcome::Key(pair) => vec![
                format!("{}  {}", lang.text("Public key", "公钥"), pair.public),
                format!("{}  {}", lang.text("Secret key", "私钥"), pair.secret),
            ],
        }
    }
}

fn protection(direction: Direction, protection: Protection, lang: Language) -> String {
    match (direction, protection) {
        (_, Protection::Passphrase) => lang.text("a passphrase (scrypt)", "密码（scrypt）").into(),
        // Decryption only proves that one key fits, not how many the file holds.
        (Direction::Decrypt, _) => lang.text("a secret key", "私钥").into(),
        (Direction::Encrypt, Protection::Recipients(count)) => format!(
            "{count} {}",
            if count == 1 {
                lang.text("public key", "个公钥")
            } else {
                lang.text("public keys", "个公钥")
            }
        ),
    }
}

fn stats(outcome: &Outcome, lang: Language) -> String {
    match outcome {
        Outcome::None => String::new(),
        Outcome::Checksum(checksum) => format!(
            "{} · {} ms",
            kib(checksum.bytes as usize),
            checksum.elapsed.as_millis()
        ),
        Outcome::Transfer(_, transfer) => format!(
            "{} {} · {} {} · {} ms",
            kib(transfer.input_bytes as usize),
            lang.text("in", "输入"),
            kib(transfer.output_bytes as usize),
            lang.text("out", "输出"),
            transfer.elapsed.as_millis()
        ),
        Outcome::Key(_) => lang
            .text(
                "This key pair exists only here. Nothing was written to disk.",
                "该密钥对仅存在于此处，未写入磁盘。",
            )
            .into(),
    }
}

/// The one line people look at after a verification, and the only place a
/// verdict is stated. Tone 1 is confirmed, tone 2 asks for attention.
fn status(outcome: &Outcome, lang: Language) -> (String, i32) {
    match outcome {
        Outcome::Checksum(checksum) => match checksum.verdict {
            Some(Verdict::Match(algorithm)) => (
                format!(
                    "{} {}",
                    algorithm.label(),
                    lang.text(
                        "checksum matches this file.",
                        "校验和与该文件一致。"
                    )
                ),
                1,
            ),
            Some(Verdict::Mismatch(algorithm)) => (
                format!(
                    "{} {}",
                    algorithm.label(),
                    lang.text(
                        "checksum does not match this file. Do not trust it.",
                        "校验和与该文件不一致，请勿信任该文件。"
                    )
                ),
                2,
            ),
            None => (String::new(), 0),
        },
        Outcome::Key(_) => (
            lang.text(
                "Save the secret key now: it is the only way back to anything encrypted to this public key.",
                "请立即保存私钥：它是打开加密给该公钥的文件的唯一途径。",
            )
            .into(),
            2,
        ),
        _ => (String::new(), 0),
    }
}
