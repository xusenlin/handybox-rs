//! Local checksums and age file encryption.
//!
//! Everything here streams: a file is read in fixed chunks and never held whole,
//! so the size limit is a patience budget rather than a memory one — an operation
//! cannot be cancelled once the worker has started it.
//!
//! Both digests are computed in the same pass. Reading the file dominates, and
//! having both is what lets a pasted checksum say which algorithm it is instead
//! of asking the person to pick one.
use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest as _, Sha256, Sha512};
use std::{
    fs::File,
    io::{self, BufReader, Read},
    iter,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoIssue {
    InputMissing,
    Read,
    NotFile,
    TooLarge,
    /// The pasted value is not a SHA-256 or SHA-512 digest in hexadecimal.
    Expected,
    /// A passphrase of no characters protects nothing.
    Passphrase,
    /// Not an `age1…` recipient.
    Recipient,
    /// Not an `AGE-SECRET-KEY-1…` identity.
    Identity,
    /// The file is not an age file at all.
    NotEncrypted,
    /// Passphrase-encrypted, but a key was offered.
    NeedsPassphrase,
    /// Encrypted to recipients, but a passphrase was offered.
    NeedsKey,
    /// The right kind of secret, but not the one this file was encrypted with.
    WrongSecret,
    /// The file asks for more scrypt work than this device is willing to do.
    ExcessiveWork,
    Encrypt,
    Decrypt,
    InvalidExtension,
    SourceOverwrite,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for CryptoIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InputMissing => "Input file could not be found",
            Self::Read => "Could not read the file",
            Self::NotFile => "Please choose a file, not a folder",
            Self::TooLarge => "File exceeds 2 GiB. Please choose a smaller file",
            Self::Expected => {
                "Enter a SHA-256 or SHA-512 checksum in hexadecimal, or leave the field empty"
            }
            Self::Passphrase => "Please enter a passphrase",
            Self::Recipient => "Recipients must be age public keys, starting with age1",
            Self::Identity => "Secret keys start with AGE-SECRET-KEY-1",
            Self::NotEncrypted => "This file is not an age encrypted file",
            Self::NeedsPassphrase => {
                "This file is protected by a passphrase, not by a key. Enter its passphrase"
            }
            Self::NeedsKey => {
                "This file is encrypted to a public key, not to a passphrase. Provide the matching secret key"
            }
            Self::WrongSecret => "This is not the passphrase or key this file was encrypted with",
            Self::ExcessiveWork => "This file asks for more work to open than this device will do",
            Self::Encrypt => "Could not encrypt the file",
            Self::Decrypt => "Could not decrypt the file: it may be damaged or incomplete",
            Self::InvalidExtension => "Encrypted files must use the .age extension",
            Self::SourceOverwrite => {
                "Cannot overwrite the file being read. Choose a different path"
            }
            Self::CreateOutput => "Could not create a file in the destination folder",
            Self::WriteOutput => "Could not write the file",
            Self::SaveOutput => "Could not save the destination file",
        })
    }
}
impl std::error::Error for CryptoIssue {}

/// There is no way to stop an operation in flight, so the cap is what keeps one
/// from occupying the worker for minutes. Nothing here is held in memory.
pub const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CHUNK_BYTES: usize = 1024 * 1024;
pub const ENCRYPTED_EXTENSION: &str = "age";

/// Whether a path names an age file by its extension alone. This is what routes
/// a dropped or command-line file to decryption rather than to another tool.
pub fn claims(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(ENCRYPTED_EXTENSION))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    pub const ALL: [Self; 2] = [Self::Sha256, Self::Sha512];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Sha512 => "SHA-512",
        }
    }

    /// Length of the digest written in hexadecimal. It is also how a pasted
    /// checksum names its own algorithm: nothing else distinguishes the two.
    pub fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    fn from_hex_len(len: usize) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|algorithm| algorithm.hex_len() == len)
    }
}

/// The result of comparing a pasted checksum against the file that was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Match(Algorithm),
    Mismatch(Algorithm),
}

#[derive(Debug)]
pub struct Checksum {
    pub source: PathBuf,
    pub bytes: u64,
    pub elapsed: Duration,
    pub sha256: String,
    pub sha512: String,
    /// `None` when nothing was given to verify against.
    pub verdict: Option<Verdict>,
}

impl Checksum {
    pub fn hex(&self, algorithm: Algorithm) -> &str {
        match algorithm {
            Algorithm::Sha256 => &self.sha256,
            Algorithm::Sha512 => &self.sha512,
        }
    }
}

/// What protects an age file. The two are exclusive: age refuses to mix them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Secret {
    Passphrase(String),
    /// One or more `age1…` recipients when encrypting; one `AGE-SECRET-KEY-1…`
    /// identity when decrypting.
    Key(String),
}

/// What holds the key to an age file. On encryption the count is how many
/// recipients the header was written for; on decryption it is the one key that
/// opened it, which is all the header tells a reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    Passphrase,
    Recipients(usize),
}

#[derive(Debug)]
pub struct Transfer {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub protection: Protection,
    pub elapsed: Duration,
}

/// An X25519 key pair, written the way age's own tools write it.
#[derive(Clone, Debug)]
pub struct KeyPair {
    pub public: String,
    pub secret: String,
}

/// A fresh identity for the public-key mode. The secret is the only way back to
/// anything encrypted to its public half, so the caller has to show it.
pub fn generate_key() -> KeyPair {
    let identity = age::x25519::Identity::generate();
    KeyPair {
        public: identity.to_public().to_string(),
        secret: identity.to_string().expose_secret().to_owned(),
    }
}

/// Digest the file, and compare against `expected` when it holds anything.
pub fn checksum(path: &Path, expected: &str) -> Result<Checksum> {
    let started = Instant::now();
    // Reject an unusable comparison before spending the read on it.
    let expected = parse_expected(expected)?;
    let (source, mut file, bytes) = open_source(path)?;
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buffer = vec![0; CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer).context(CryptoIssue::Read)?;
        if read == 0 {
            break;
        }
        sha256.update(&buffer[..read]);
        sha512.update(&buffer[..read]);
    }
    let checksum = Checksum {
        source,
        bytes,
        sha256: hex(&sha256.finalize()),
        sha512: hex(&sha512.finalize()),
        verdict: None,
        elapsed: started.elapsed(),
    };
    let verdict = expected.map(|(algorithm, value)| {
        if checksum.hex(algorithm) == value {
            Verdict::Match(algorithm)
        } else {
            Verdict::Mismatch(algorithm)
        }
    });
    Ok(Checksum {
        verdict,
        ..checksum
    })
}

/// Encrypt `source` into `destination`, which must be a `.age` path. Written
/// beside the destination and atomically persisted, so a failed run leaves
/// nothing half-encrypted behind. The caller owns overwrite confirmation.
pub fn encrypt(source: &Path, destination: &Path, secret: &Secret) -> Result<Transfer> {
    let started = Instant::now();
    ensure!(claims(destination), CryptoIssue::InvalidExtension);
    let (source, file, input_bytes) = open_source(source)?;
    let mut temp = stage_output(&source, destination)?;
    // Building the encryptor is where a passphrase costs its second of scrypt,
    // so do it after the cheap checks have all passed.
    let (encryptor, protection) = match secret {
        Secret::Passphrase(passphrase) => {
            ensure!(!passphrase.is_empty(), CryptoIssue::Passphrase);
            (
                age::Encryptor::with_user_passphrase(SecretString::from(passphrase.clone())),
                Protection::Passphrase,
            )
        }
        Secret::Key(keys) => {
            let recipients = parse_recipients(keys)?;
            let encryptor = age::Encryptor::with_recipients(
                recipients
                    .iter()
                    .map(|recipient| recipient as &dyn age::Recipient),
            )
            .map_err(|error| anyhow::anyhow!("{error}").context(CryptoIssue::Encrypt))?;
            (encryptor, Protection::Recipients(recipients.len()))
        }
    };
    let mut reader = BufReader::with_capacity(CHUNK_BYTES, file);
    let mut writer = encryptor
        .wrap_output(temp.as_file_mut())
        .context(CryptoIssue::Encrypt)?;
    io::copy(&mut reader, &mut writer).context(CryptoIssue::Encrypt)?;
    // Without `finish` the stream is truncated and the file will not decrypt.
    writer.finish().context(CryptoIssue::Encrypt)?;
    let output_bytes = persist(temp, destination)?;
    Ok(Transfer {
        source,
        destination: destination.to_owned(),
        input_bytes,
        output_bytes,
        protection,
        elapsed: started.elapsed(),
    })
}

/// Decrypt `source` into `destination`. The plaintext may be anything, so the
/// destination carries no required extension — only the refusal to overwrite
/// the file being read.
pub fn decrypt(source: &Path, destination: &Path, secret: &Secret) -> Result<Transfer> {
    let started = Instant::now();
    let (source, file, input_bytes) = open_source(source)?;
    let decryptor = age::Decryptor::new_buffered(BufReader::with_capacity(CHUNK_BYTES, file))
        .map_err(describe)?;
    // The header already says which kind of secret the file wants. Saying so is
    // worth more than letting both cases fail as "no matching keys".
    let protection = match (secret, decryptor.is_scrypt()) {
        (Secret::Key(_), true) => bail!(CryptoIssue::NeedsPassphrase),
        (Secret::Passphrase(_), false) => bail!(CryptoIssue::NeedsKey),
        (_, true) => Protection::Passphrase,
        (_, false) => Protection::Recipients(1),
    };
    let identity: Box<dyn age::Identity> = match secret {
        Secret::Passphrase(passphrase) => {
            ensure!(!passphrase.is_empty(), CryptoIssue::Passphrase);
            Box::new(age::scrypt::Identity::new(SecretString::from(
                passphrase.clone(),
            )))
        }
        Secret::Key(key) => Box::new(
            age::x25519::Identity::from_str(key.trim())
                .map_err(|reason| anyhow::anyhow!("{reason}").context(CryptoIssue::Identity))?,
        ),
    };
    let mut temp = stage_output(&source, destination)?;
    let mut reader = decryptor
        .decrypt(iter::once(identity.as_ref()))
        .map_err(describe)?;
    io::copy(&mut reader, temp.as_file_mut()).context(CryptoIssue::Decrypt)?;
    let output_bytes = persist(temp, destination)?;
    Ok(Transfer {
        source,
        destination: destination.to_owned(),
        input_bytes,
        output_bytes,
        protection,
        elapsed: started.elapsed(),
    })
}

/// What to call the encrypted result, so the save dialog opens on a name that
/// keeps the original one: `notes.pdf` becomes `notes.pdf.age`.
pub fn encrypted_name(source: &Path) -> String {
    format!(
        "{}.{ENCRYPTED_EXTENSION}",
        source.file_name().unwrap_or_default().to_string_lossy()
    )
}

/// The reverse, when the name allows it. Anything not named `.age` is marked
/// instead, so decrypting never proposes to overwrite the file it just read.
pub fn decrypted_name(source: &Path) -> String {
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    if claims(source) {
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        if !stem.is_empty() {
            return stem.into_owned();
        }
    }
    format!("{name}-decrypted")
}

/// An empty field means "just compute". Anything else has to be a digest this
/// tool can produce; `shasum` writes `<hex>  <name>`, so only the first field
/// is read and a pasted line from a checksum file works as it is.
fn parse_expected(expected: &str) -> Result<Option<(Algorithm, String)>> {
    let expected = expected.trim();
    if expected.is_empty() {
        return Ok(None);
    }
    let value = expected.split_whitespace().next().unwrap_or_default();
    let algorithm = Algorithm::from_hex_len(value.len())
        .filter(|_| value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context(CryptoIssue::Expected)?;
    Ok(Some((algorithm, value.to_ascii_lowercase())))
}

/// Accepts the several recipients a shared file usually has, written the way
/// people paste them: one per line, or separated by spaces or commas.
fn parse_recipients(keys: &str) -> Result<Vec<age::x25519::Recipient>> {
    let recipients = keys
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|key| !key.is_empty())
        .map(|key| {
            age::x25519::Recipient::from_str(key)
                .map_err(|reason| anyhow::anyhow!("{reason}").context(CryptoIssue::Recipient))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!recipients.is_empty(), CryptoIssue::Recipient);
    Ok(recipients)
}

/// Bounded handle on the file the user chose. The size is metadata, not a read.
fn open_source(path: &Path) -> Result<(PathBuf, File, u64)> {
    let source = path.canonicalize().context(CryptoIssue::InputMissing)?;
    let file = File::open(&source).context(CryptoIssue::Read)?;
    let meta = file.metadata().context(CryptoIssue::Read)?;
    ensure!(meta.is_file(), CryptoIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, CryptoIssue::TooLarge);
    Ok((source, file, meta.len()))
}

/// A temporary file beside the destination. `source` is already canonical.
fn stage_output(source: &Path, destination: &Path) -> Result<tempfile::NamedTempFile> {
    if let Ok(existing) = destination.canonicalize() {
        ensure!(existing != source, CryptoIssue::SourceOverwrite);
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tempfile::NamedTempFile::new_in(parent).context(CryptoIssue::CreateOutput)
}

fn persist(temp: tempfile::NamedTempFile, destination: &Path) -> Result<u64> {
    let bytes = temp
        .as_file()
        .metadata()
        .context(CryptoIssue::WriteOutput)?
        .len();
    temp.as_file()
        .sync_all()
        .context(CryptoIssue::WriteOutput)?;
    temp.persist(destination).context(CryptoIssue::SaveOutput)?;
    Ok(bytes)
}

/// age's own error text is kept as the cause; the category is what the desktop
/// translates. `DecryptError` is non-exhaustive, so unknown variants fall back.
fn describe(error: age::DecryptError) -> anyhow::Error {
    let issue = match error {
        age::DecryptError::InvalidHeader | age::DecryptError::UnknownFormat => {
            CryptoIssue::NotEncrypted
        }
        age::DecryptError::NoMatchingKeys
        | age::DecryptError::DecryptionFailed
        | age::DecryptError::KeyDecryptionFailed => CryptoIssue::WrongSecret,
        age::DecryptError::ExcessiveWork { .. } => CryptoIssue::ExcessiveWork,
        _ => CryptoIssue::Decrypt,
    };
    anyhow::anyhow!("{error}").context(issue)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}
