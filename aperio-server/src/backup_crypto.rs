//! Encryption for scheduled snapshots (`planned_features.md` #54).
//!
//! A snapshot is a copy of the whole store: hashed credentials, sessions,
//! organizations, tokens. It is written to a directory an operator chose,
//! which is usually somewhere with a longer life and a wider audience than
//! the data directory, an NFS mount, an object-store sync, a tape rotation.
//! So there is a case for encrypting it, and the entry this comes from names
//! the condition for that case being worth anything: **a key sitting next to
//! the backup is decoration.** The key handling below is the part that had to
//! be real; the cipher is the easy half.
//!
//! **AES-256-GCM through `ring`**, which rustls already puts in this binary,
//! so this adds an algorithm rather than a crypto stack.
//!
//! ## The file
//!
//! ```text
//! "APERIOBK"        8 bytes, so a mistyped path fails loudly
//! version           1 byte, currently 1
//! chunk_size        4 bytes, big endian
//! nonce_prefix      8 bytes, random per file
//! chunk*            length (4 bytes) then that many bytes of ciphertext+tag
//! ```
//!
//! The per-chunk length is what makes the framing unambiguous: the last data
//! chunk is short, and without a length a reader cannot tell where it ends and
//! the marker after it begins. The length is authenticated too, so changing it
//! is a tag failure rather than a reader that walks off into the next chunk.
//!
//! Each chunk's nonce is `nonce_prefix || counter`, and the counter and a
//! final-chunk flag are authenticated as associated data. That is what makes
//! the three attacks on a chunked format fail rather than go unnoticed:
//! reordering and duplication change the counter, and **truncation** is caught
//! because the file always ends with a zero-length chunk whose flag is set. A
//! reader that hits the end of the file without having seen it reports a
//! truncated backup instead of handing back a shorter database.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

/// File magic, and the suffix that says a snapshot is encrypted.
const MAGIC: &[u8; 8] = b"APERIOBK";
pub(crate) const ENCRYPTED_SUFFIX: &str = ".db.enc";
const VERSION: u8 = 1;
const TAG_LEN: usize = 16;
const NONCE_PREFIX_LEN: usize = 8;

/// Plaintext bytes per chunk. Large enough that the per-chunk tag is noise
/// (16 bytes per megabyte), small enough that neither side holds much.
const CHUNK: usize = 1024 * 1024;

/// A 32-byte key, kept out of `Debug` so it cannot be logged by accident.
pub(crate) struct BackupKey([u8; 32]);

impl std::fmt::Debug for BackupKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("BackupKey(<redacted>)")
  }
}

impl BackupKey {
  /// Parses a key written as 64 hex characters or as base64.
  ///
  /// Both, because the two places a key comes from spell it differently: a
  /// secret manager writes base64, and `openssl rand -hex 32` is what an
  /// operator types.
  pub(crate) fn parse(raw: &str) -> Result<Self, String> {
    let raw = raw.trim();
    if raw.is_empty() {
      return Err("the backup key is empty".into());
    }
    let bytes = if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
      (0..32)
        .map(|i| u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| "the backup key is not valid hex".to_string())?
    } else {
      use base64::Engine;
      base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| "the backup key is neither 64 hex characters nor valid base64".to_string())?
    };
    let len = bytes.len();
    let array: [u8; 32] = bytes.try_into().map_err(|_| {
      format!("the backup key must be 32 bytes (256 bits); this one decodes to {len}")
    })?;
    Ok(BackupKey(array))
  }

  fn sealing(&self) -> LessSafeKey {
    LessSafeKey::new(
      UnboundKey::new(&AES_256_GCM, &self.0)
        .expect("a 32-byte key is the right length for AES-256"),
    )
  }
}

/// Where a key came from, and why it was refused when it was.
///
/// **The refusals are the point of this type.** A backup key that lives in the
/// directory it protects is not a key, and a configuration that says
/// "encrypted" while producing something anyone holding the backup can read is
/// worse than one that never claimed to.
pub(crate) fn load_key(
  key_inline: Option<&str>,
  key_file: Option<&str>,
  backup_dir: &Path,
) -> Result<Option<BackupKey>, String> {
  match (key_inline, key_file) {
    (None, None) => Ok(None),
    (Some(_), Some(_)) => Err(
      "both a backup key and a backup key file are configured; set exactly one, \
       so there is no question which one is in force"
        .into(),
    ),
    (Some(inline), None) => BackupKey::parse(inline).map(Some),
    (None, Some(path)) => {
      let path = PathBuf::from(path.trim());
      let canonical_key = path.canonicalize().unwrap_or_else(|_| path.clone());
      let canonical_dir = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());
      if canonical_key.starts_with(&canonical_dir) {
        return Err(format!(
          "the backup key file {} is inside the backup directory {}: anyone who \
           has the backups has the key, which is the one arrangement encryption \
           cannot survive",
          canonical_key.display(),
          canonical_dir.display()
        ));
      }
      let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read the backup key file {}: {e}", path.display()))?;
      warn_if_world_readable(&path);
      BackupKey::parse(&raw).map(Some)
    }
  }
}

/// Says so when the key file is readable by everyone. A warning rather than a
/// refusal: modes vary with how a secret is mounted, and refusing to start
/// over a permission bit is its own outage.
fn warn_if_world_readable(path: &Path) {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
      let mode = meta.permissions().mode() & 0o077;
      if mode != 0 {
        tracing::warn!(
          "The backup key file {} is readable beyond its owner (mode {:o}); chmod 600 it",
          path.display(),
          meta.permissions().mode() & 0o777
        );
      }
    }
  }
}

/// Encrypts `plaintext_path` to `out_path`, streaming.
pub(crate) fn encrypt_file(
  key: &BackupKey,
  plaintext_path: &Path,
  out_path: &Path,
) -> Result<u64, String> {
  let sealing = key.sealing();
  let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
  SystemRandom::new()
    .fill(&mut nonce_prefix)
    .map_err(|_| "no randomness available for the backup nonce".to_string())?;

  let input = File::open(plaintext_path)
    .map_err(|e| format!("cannot read the snapshot {}: {e}", plaintext_path.display()))?;
  let mut reader = BufReader::new(input);
  let output =
    File::create(out_path).map_err(|e| format!("cannot write {}: {e}", out_path.display()))?;
  restrict(&output);
  let mut writer = BufWriter::new(output);

  writer.write_all(MAGIC).map_err(io)?;
  writer.write_all(&[VERSION]).map_err(io)?;
  writer
    .write_all(&(CHUNK as u32).to_be_bytes())
    .map_err(io)?;
  writer.write_all(&nonce_prefix).map_err(io)?;

  let mut counter: u32 = 0;
  let mut buf = vec![0u8; CHUNK];
  loop {
    let mut filled = 0;
    while filled < CHUNK {
      match reader.read(&mut buf[filled..]).map_err(io)? {
        0 => break,
        n => filled += n,
      }
    }
    if filled == 0 {
      break;
    }
    let mut chunk = buf[..filled].to_vec();
    seal(&sealing, &nonce_prefix, counter, false, &mut chunk)?;
    writer
      .write_all(&(chunk.len() as u32).to_be_bytes())
      .map_err(io)?;
    writer.write_all(&chunk).map_err(io)?;
    counter = counter
      .checked_add(1)
      .ok_or("snapshot too large to encrypt")?;
  }

  // The end marker: an empty chunk whose flag is set. Without it a truncated
  // file decrypts to a shorter database and nothing says so.
  let mut end = Vec::new();
  seal(&sealing, &nonce_prefix, counter, true, &mut end)?;
  writer
    .write_all(&(end.len() as u32).to_be_bytes())
    .map_err(io)?;
  writer.write_all(&end).map_err(io)?;
  writer.flush().map_err(io)?;
  drop(writer);

  std::fs::metadata(out_path)
    .map(|m| m.len())
    .map_err(|e| format!("cannot stat {}: {e}", out_path.display()))
}

/// Decrypts `in_path` to `out_path`, streaming. Refuses a truncated file.
///
/// **Nothing appears at `out_path` unless the whole file decrypted.** The work
/// goes to a temporary file beside it and is renamed at the end, because the
/// caller of this is restoring a database: a half-written one left behind by a
/// failed decrypt is the thing most likely to be restored by mistake, and it
/// looks exactly like a small database rather than like an error.
pub(crate) fn decrypt_file(
  key: &BackupKey,
  in_path: &Path,
  out_path: &Path,
) -> Result<u64, String> {
  let partial = out_path.with_extension("partial");
  let result = decrypt_into(key, in_path, &partial);
  match result {
    Ok(size) => {
      std::fs::rename(&partial, out_path)
        .map_err(|e| format!("cannot move the decrypted snapshot into place: {e}"))?;
      Ok(size)
    }
    Err(e) => {
      let _ = std::fs::remove_file(&partial);
      Err(e)
    }
  }
}

fn decrypt_into(key: &BackupKey, in_path: &Path, out_path: &Path) -> Result<u64, String> {
  let sealing = key.sealing();
  let input = File::open(in_path).map_err(|e| format!("cannot read {}: {e}", in_path.display()))?;
  let mut reader = BufReader::new(input);

  let mut magic = [0u8; 8];
  reader.read_exact(&mut magic).map_err(|_| {
    format!(
      "{} is too short to be an encrypted snapshot",
      in_path.display()
    )
  })?;
  if &magic != MAGIC {
    return Err(format!(
      "{} is not an encrypted Aperio snapshot",
      in_path.display()
    ));
  }
  let mut version = [0u8; 1];
  reader.read_exact(&mut version).map_err(io)?;
  if version[0] != VERSION {
    return Err(format!(
      "{} was written in format version {}, which this build does not read",
      in_path.display(),
      version[0]
    ));
  }
  let mut chunk_size = [0u8; 4];
  reader.read_exact(&mut chunk_size).map_err(io)?;
  let chunk_size = u32::from_be_bytes(chunk_size) as usize;
  if chunk_size == 0 || chunk_size > 64 * 1024 * 1024 {
    return Err(format!(
      "{} declares an unusable chunk size",
      in_path.display()
    ));
  }
  let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
  reader.read_exact(&mut nonce_prefix).map_err(io)?;

  let output =
    File::create(out_path).map_err(|e| format!("cannot write {}: {e}", out_path.display()))?;
  restrict(&output);
  let mut writer = BufWriter::new(output);

  let mut counter: u32 = 0;
  loop {
    let mut frame = [0u8; 4];
    reader.read_exact(&mut frame).map_err(|_| {
      format!(
        "{} ends without its final marker: the backup is truncated",
        in_path.display()
      )
    })?;
    let len = u32::from_be_bytes(frame) as usize;
    if len < TAG_LEN || len > chunk_size + TAG_LEN {
      return Err(format!(
        "{} declares a chunk of {len} bytes at chunk {counter}, which cannot be right",
        in_path.display()
      ));
    }
    let mut chunk = vec![0u8; len];
    reader.read_exact(&mut chunk).map_err(|_| {
      format!(
        "{} ends part-way through chunk {counter}: the backup is truncated",
        in_path.display()
      )
    })?;

    // Whether this is the end marker is decided by the tag, not by the length:
    // the flag is authenticated, so a chunk cannot be passed off as the end of
    // the file, nor the end of the file hidden.
    let last = len == TAG_LEN;
    let plain = open(&sealing, &nonce_prefix, counter, last, &mut chunk).map_err(|_| {
      format!(
        "{} failed to decrypt at chunk {counter}: wrong key, or the file was \
         altered, reordered or truncated",
        in_path.display()
      )
    })?;
    if last {
      writer.flush().map_err(io)?;
      drop(writer);
      return std::fs::metadata(out_path)
        .map(|m| m.len())
        .map_err(|e| format!("cannot stat {}: {e}", out_path.display()));
    }
    writer.write_all(plain).map_err(io)?;
    counter = counter
      .checked_add(1)
      .ok_or("snapshot too large to decrypt")?;
  }
}

/// Seals `data` in place, appending the tag.
fn seal(
  key: &LessSafeKey,
  nonce_prefix: &[u8; NONCE_PREFIX_LEN],
  counter: u32,
  last: bool,
  data: &mut Vec<u8>,
) -> Result<(), String> {
  let len = (data.len() + TAG_LEN) as u32;
  key
    .seal_in_place_append_tag(nonce(nonce_prefix, counter), aad(counter, last, len), data)
    .map_err(|_| "failed to encrypt a backup chunk".to_string())
}

/// Opens `data` in place, returning the plaintext slice.
fn open<'a>(
  key: &LessSafeKey,
  nonce_prefix: &[u8; NONCE_PREFIX_LEN],
  counter: u32,
  last: bool,
  data: &'a mut [u8],
) -> Result<&'a [u8], ()> {
  let len = data.len() as u32;
  key
    .open_in_place(nonce(nonce_prefix, counter), aad(counter, last, len), data)
    .map(|plain| &*plain)
    .map_err(|_| ())
}

/// `nonce_prefix || counter`: unique per chunk, and per file through the
/// random prefix, which is what a fixed key needs.
fn nonce(nonce_prefix: &[u8; NONCE_PREFIX_LEN], counter: u32) -> Nonce {
  let mut bytes = [0u8; 12];
  bytes[..NONCE_PREFIX_LEN].copy_from_slice(nonce_prefix);
  bytes[NONCE_PREFIX_LEN..].copy_from_slice(&counter.to_be_bytes());
  Nonce::assume_unique_for_key(bytes)
}

/// The chunk's position, its length and whether it ends the file:
/// authenticated but not encrypted, which is what makes reordering,
/// truncation and a rewritten frame length fail.
fn aad(counter: u32, last: bool, len: u32) -> Aad<[u8; 10]> {
  let mut bytes = [0u8; 10];
  bytes[0] = VERSION;
  bytes[1..5].copy_from_slice(&counter.to_be_bytes());
  bytes[5] = u8::from(last);
  bytes[6..10].copy_from_slice(&len.to_be_bytes());
  Aad::from(bytes)
}

/// Owner-only, best effort: a backup and its intermediate copy are the whole
/// store.
fn restrict(file: &File) {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
  }
  #[cfg(not(unix))]
  let _ = file;
}

fn io(e: std::io::Error) -> String {
  e.to_string()
}

#[cfg(test)]
#[path = "backup_crypto_tests.rs"]
mod tests;
