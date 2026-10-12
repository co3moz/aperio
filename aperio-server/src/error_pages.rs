//! Per-hostname custom error pages (the `error_pages:` section of
//! `aperio-server.yaml`).
//!
//! The global `pages.unavailable` / `pages.maintenance` pages apply to every
//! service; this section overrides them per hostname, so each exposed site
//! can carry its own branding on gateway-timeout and maintenance responses:
//!
//! ```yaml
//! error_pages:
//!   - hostname: app.example.com
//!     504_page: ./pages/app-504.html
//!     503_page: ./pages/app-503.html
//! ```
//!
//! Pages are read from disk when the section is (re)loaded, at startup and
//! on config hot-reload. An unreadable file or malformed section logs an
//! error and keeps the global pages, so a bad edit never breaks proxying.

use serde::Deserialize;

/// One `error_pages:` entry as written in the file.
#[derive(Deserialize)]
struct ErrorPageRule {
  /// Hostname to match exactly (case-insensitive).
  hostname: String,
  /// Path of the HTML served on 504 gateway-timeout responses.
  #[serde(rename = "504_page")]
  page_504: Option<String>,
  /// Path of the HTML served on 503 maintenance responses.
  #[serde(rename = "503_page")]
  page_503: Option<String>,
}

/// One compiled rule: the hostname plus the loaded page contents.
#[derive(Clone)]
struct CompiledRule {
  hostname: String,
  html_504: Option<String>,
  html_503: Option<String>,
}

/// Compiled `error_pages:` rules, carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct ErrorPages {
  rules: Vec<CompiledRule>,
}

impl ErrorPages {
  /// Custom 504 page for a request hostname, if one is configured.
  pub(crate) fn page_504(&self, host: Option<&str>) -> Option<&str> {
    self.find(host).and_then(|r| r.html_504.as_deref())
  }

  /// Custom 503 maintenance page for a request hostname, if configured.
  pub(crate) fn page_503(&self, host: Option<&str>) -> Option<&str> {
    self.find(host).and_then(|r| r.html_503.as_deref())
  }

  fn find(&self, host: Option<&str>) -> Option<&CompiledRule> {
    let host = host?.to_ascii_lowercase();
    self.rules.iter().find(|r| r.hostname == host)
  }
}

/// Compiles the `error_pages:` section of the settings. A page file that
/// cannot be read keeps the global page for that hostname, with a log line.
pub(crate) fn from_section(section: Option<serde_yaml::Value>) -> Result<ErrorPages, String> {
  let Some(section) = section else {
    return Ok(ErrorPages::default());
  };
  let rules: Vec<ErrorPageRule> =
    serde_yaml::from_value(section).map_err(|e| format!("error_pages: {e}"))?;
  let mut compiled = Vec::with_capacity(rules.len());
  for rule in rules {
    let hostname = rule.hostname.trim().to_ascii_lowercase();
    if hostname.is_empty() {
      tracing::error!("`error_pages:` entry without a hostname is ignored");
      continue;
    }
    let load = |path: Option<&String>, which: &str| -> Option<String> {
      let path = path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())?;
      match std::fs::read_to_string(&path) {
        Ok(html) => {
          tracing::info!("Custom {which} page for {hostname} loaded from {path}");
          Some(html)
        }
        Err(e) => {
          tracing::error!(
            "Failed to read the {which} page for {hostname} from {path}: {e}, using the global page"
          );
          None
        }
      }
    };
    let html_504 = load(rule.page_504.as_ref(), "504");
    let html_503 = load(rule.page_503.as_ref(), "503");
    if html_504.is_none() && html_503.is_none() {
      continue;
    }
    compiled.push(CompiledRule {
      hostname,
      html_504,
      html_503,
    });
  }
  Ok(ErrorPages { rules: compiled })
}

#[cfg(test)]
#[path = "error_pages_tests.rs"]
mod tests;
