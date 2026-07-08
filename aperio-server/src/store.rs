//! Persistence layer: file-backed stores for audit events, traffic stats,
//! dynamic tokens, and webhook definitions.

use std::io;
use std::path::{Path, PathBuf};

pub(crate) mod audit;
pub(crate) mod stats;
pub(crate) mod tokens;
pub(crate) mod webhooks;

/// Writes `contents` to `path` atomically: writes a sibling temp file, flushes
/// it to disk, then renames it over the target. A crash mid-write can then only
/// ever leave the intact previous file or the intact new one — never a
/// truncated/corrupt store. `std::fs::rename` replaces the destination
/// atomically on both Unix and Windows.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
  use std::io::Write;
  let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
  let tmp = path.with_file_name(format!("{name}.tmp"));
  {
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(contents)?;
    f.sync_all()?;
  }
  std::fs::rename(&tmp, path)
}

/// Renames a file that failed to parse aside as `<name>.corrupt.<epoch>` so the
/// bad data is preserved for recovery instead of being silently overwritten by
/// an empty store on the next write. Returns the backup path on success.
pub(crate) fn backup_corrupt(path: &Path) -> Option<PathBuf> {
  let secs = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
  let backup = path.with_file_name(format!("{name}.corrupt.{secs}"));
  std::fs::rename(path, &backup).ok().map(|_| backup)
}
