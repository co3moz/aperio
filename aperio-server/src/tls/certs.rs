//! The certificates the HTTPS listener can present: files an operator
//! manages, and the ones an ACME authority issued, looked up by the name a
//! visitor asked for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use aperio_config::server_settings::CertificateFiles;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tracing::{error, info, warn};

/// One certificate, ready to present, with what is in it.
#[derive(Clone, Debug)]
pub(crate) struct Loaded {
  pub(crate) key: Arc<CertifiedKey>,
  /// The DNS names it serves, lower case; a wildcard as `*.example.com`.
  pub(crate) names: Vec<String>,
  /// When it stops being valid, unix seconds.
  pub(crate) not_after: i64,
}

/// Parses a PEM chain, leaf first, and its key into something to present.
/// The two may be the same text, which is how an issued one is kept.
pub(crate) fn load_pem(chain_pem: &[u8], key_pem: &[u8]) -> Result<Loaded, String> {
  let chain = CertificateDer::pem_slice_iter(chain_pem)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| format!("the certificate is not PEM: {e}"))?;
  let leaf = chain.first().ok_or("there is no certificate in it")?;
  let (names, not_after) = names_and_expiry(leaf)?;
  let key =
    PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("the key is not PEM: {e}"))?;
  let signing = rustls::crypto::ring::sign::any_supported_type(&key)
    .map_err(|e| format!("the key cannot sign: {e}"))?;
  Ok(Loaded {
    key: Arc::new(CertifiedKey::new(chain, signing)),
    names,
    not_after,
  })
}

pub(crate) fn names_and_expiry(leaf: &CertificateDer<'_>) -> Result<(Vec<String>, i64), String> {
  let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref())
    .map_err(|e| format!("the certificate cannot be read: {e}"))?;
  let mut names: Vec<String> = Vec::new();
  if let Ok(Some(san)) = cert.subject_alternative_name() {
    for name in &san.value.general_names {
      if let x509_parser::extensions::GeneralName::DNSName(dns) = name {
        names.push(dns.to_ascii_lowercase());
      }
    }
  }
  // A certificate without a SAN names its host in the subject, which is what
  // an old self-made one does.
  if names.is_empty()
    && let Some(cn) = cert
      .subject()
      .iter_common_name()
      .next()
      .and_then(|cn| cn.as_str().ok())
  {
    names.push(cn.to_ascii_lowercase());
  }
  Ok((names, cert.validity().not_after.timestamp()))
}

/// Whether a certificate's `name` covers the `host` a visitor asked for:
/// exactly, or as a wildcard standing for one label.
pub(crate) fn covers(name: &str, host: &str) -> bool {
  match name.strip_prefix("*.") {
    Some(suffix) => host
      .split_once('.')
      .is_some_and(|(label, rest)| !label.is_empty() && rest == suffix),
    None => name == host,
  }
}

/// A certificate an operator keeps as files, and when they were last read.
struct FileCert {
  files: CertificateFiles,
  stamp: Option<(SystemTime, SystemTime)>,
  loaded: Option<Loaded>,
}

fn modified(path: &str) -> Option<SystemTime> {
  std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Every certificate the listener may present.
pub(crate) struct CertStore {
  files: RwLock<Vec<FileCert>>,
  issued: RwLock<HashMap<String, Loaded>>,
  /// Where issued certificates are kept, one `<hostname>.pem` each.
  issued_dir: Option<PathBuf>,
}

impl std::fmt::Debug for CertStore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CertStore").finish_non_exhaustive()
  }
}

impl CertStore {
  pub(crate) fn new(files: &[CertificateFiles], issued_dir: Option<PathBuf>) -> Self {
    let store = CertStore {
      files: RwLock::new(
        files
          .iter()
          .map(|f| FileCert {
            files: f.clone(),
            stamp: None,
            loaded: None,
          })
          .collect(),
      ),
      issued: RwLock::new(HashMap::new()),
      issued_dir,
    };
    store.refresh_files();
    store.load_issued();
    store
  }

  /// Reads again every file pair whose modification time changed. One that
  /// cannot be read keeps the certificate it had, and says why, so a renewal
  /// caught half-written does not take a working certificate away.
  pub(crate) fn refresh_files(&self) {
    let Ok(mut files) = self.files.write() else {
      return;
    };
    for entry in files.iter_mut() {
      let stamp = modified(&entry.files.cert).zip(modified(&entry.files.key));
      if stamp.is_some() && stamp == entry.stamp {
        continue;
      }
      let read = std::fs::read(&entry.files.cert)
        .and_then(|cert| Ok((cert, std::fs::read(&entry.files.key)?)))
        .map_err(|e| e.to_string())
        .and_then(|(cert, key)| load_pem(&cert, &key));
      match read {
        Ok(loaded) => {
          info!(
            "TLS certificate {} serves {}",
            entry.files.cert,
            loaded.names.join(", ")
          );
          entry.loaded = Some(loaded);
          entry.stamp = stamp;
        }
        Err(e) => error!(
          "TLS certificate {} / {}: {e}{}",
          entry.files.cert,
          entry.files.key,
          if entry.loaded.is_some() {
            "; keeping the one read before"
          } else {
            ""
          }
        ),
      }
    }
  }

  /// Loads the certificates an authority issued before this start.
  fn load_issued(&self) {
    let Some(dir) = &self.issued_dir else {
      return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    let mut issued = HashMap::new();
    for entry in entries.flatten() {
      let path = entry.path();
      let Some(host) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".pem"))
      else {
        continue;
      };
      match std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|pem| load_pem(&pem, &pem))
      {
        Ok(loaded) => {
          issued.insert(host.to_string(), loaded);
        }
        Err(e) => warn!(
          "TLS certificate {}: {e}; it will be issued again",
          path.display()
        ),
      }
    }
    if let Ok(mut map) = self.issued.write() {
      *map = issued;
    }
  }

  /// The certificate for `sni`: one issued for exactly that name, then a
  /// file's that names it, then a file's wildcard. A handshake with no name
  /// gets the first file's, as a default server would.
  pub(crate) fn lookup(&self, sni: Option<&str>) -> Option<Arc<CertifiedKey>> {
    let files = self.files.read().ok()?;
    let Some(host) = sni.map(str::to_ascii_lowercase) else {
      return files
        .iter()
        .find_map(|f| f.loaded.as_ref().map(|l| l.key.clone()));
    };
    if let Some(issued) = self.issued.read().ok()?.get(&host) {
      return Some(issued.key.clone());
    }
    let named = |exact: bool| {
      files.iter().find_map(|f| {
        let loaded = f.loaded.as_ref()?;
        loaded
          .names
          .iter()
          .any(|n| n.starts_with("*.") != exact && covers(n, &host))
          .then(|| loaded.key.clone())
      })
    };
    named(true).or_else(|| named(false))
  }

  /// The certificate issued for `host`, if one is held.
  pub(crate) fn issued(&self, host: &str) -> Option<Loaded> {
    self.issued.read().ok()?.get(host).cloned()
  }

  /// Keeps a certificate an authority just issued, on disk and in memory.
  pub(crate) fn store_issued(
    &self,
    host: &str,
    chain_pem: &str,
    key_pem: &str,
  ) -> Result<Loaded, String> {
    let loaded = load_pem(chain_pem.as_bytes(), key_pem.as_bytes())?;
    if let Some(dir) = &self.issued_dir {
      let mut pem = String::with_capacity(chain_pem.len() + key_pem.len() + 1);
      pem.push_str(chain_pem.trim_end());
      pem.push('\n');
      pem.push_str(key_pem);
      write_private(&dir.join(format!("{host}.pem")), pem.as_bytes())
        .map_err(|e| format!("could not keep it in {}: {e}", dir.display()))?;
    }
    if let Ok(mut map) = self.issued.write() {
      map.insert(host.to_string(), loaded.clone());
    }
    Ok(loaded)
  }

  /// The issued certificates that stop being valid within `within` seconds
  /// of `now`.
  pub(crate) fn due(&self, now: i64, within: i64) -> Vec<String> {
    let Ok(map) = self.issued.read() else {
      return Vec::new();
    };
    let mut due: Vec<String> = map
      .iter()
      .filter(|(_, l)| l.not_after - now < within)
      .map(|(h, _)| h.clone())
      .collect();
    due.sort();
    due
  }
}

/// Writes a file only its owner may read, since it holds a private key.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let tmp = path.with_extension("tmp");
  {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt;
      options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
  }
  // Replaced whole, so a reader never sees half a certificate.
  std::fs::rename(&tmp, path)
}

/// Presents what the store holds for the name in the handshake.
#[derive(Debug)]
pub(crate) struct StoreResolver(pub(crate) Arc<CertStore>);

impl ResolvesServerCert for StoreResolver {
  fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
    self.0.lookup(hello.server_name())
  }
}

#[cfg(test)]
#[path = "certs_tests.rs"]
mod tests;
