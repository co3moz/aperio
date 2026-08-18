//! `aperio-client api ...`: command-line access to the Aperio server's admin
//! API: the same operations the dashboard performs (share links, dynamic
//! tokens, ephemeral tunnels, maintenance mode, cache/webhooks/users/orgs,
//! …), so they can be scripted from CI without a browser session.
//!
//! Authentication uses a programmatic admin key (`--api-key`, yaml
//! `server.api_key`, env `APERIO_API_KEY`) sent as `Authorization: Bearer`.
//! The tunnel token (`--server-token`) is accepted as a fallback credential;
//! the server only honours it on the endpoints that take the master token
//! (`api tunnel create|delete`), so everything else needs an admin key.
//!
//! Every command prints the server's JSON response (pretty-printed) on
//! stdout and exits 0; a transport error or a non-2xx status prints to
//! stderr and exits 1, so shell scripts can branch on the exit code.

use serde_json::{Map, Value};
use std::time::Duration;

// The command line, the call it becomes, and the transport that makes it.
pub(crate) mod build;
pub(crate) mod call;
pub(crate) mod commands;

pub(crate) use build::*;
pub(crate) use call::*;
pub(crate) use commands::*;

use crate::config::{ClientSettings, CommonOpts, build_http_url};

/// Parses a human duration into seconds: `45s`, `30m`, `2h`, `1d`, `2w`, a
/// bare number (seconds), or `never` / `none` / `0` for "no expiry" (0).
pub(crate) fn parse_duration(raw: &str) -> Result<u64, String> {
  let value = raw.trim().to_ascii_lowercase();
  if value.is_empty() {
    return Err("empty duration".to_string());
  }
  if matches!(value.as_str(), "never" | "none" | "infinite" | "0") {
    return Ok(0);
  }
  let (number, multiplier) = match value.chars().last() {
    Some('s') => (&value[..value.len() - 1], 1u64),
    Some('m') => (&value[..value.len() - 1], 60),
    Some('h') => (&value[..value.len() - 1], 3_600),
    Some('d') => (&value[..value.len() - 1], 86_400),
    Some('w') => (&value[..value.len() - 1], 604_800),
    _ => (value.as_str(), 1),
  };
  let parsed: u64 = number
    .parse()
    .map_err(|_| format!("invalid duration '{}' (use 30m, 2h, 1d, 1w, or never)", raw))?;
  parsed
    .checked_mul(multiplier)
    .ok_or_else(|| format!("duration '{}' is too large", raw))
}

// --- CLI surface -----------------------------------------------------------

// --- HTTP plumbing ---------------------------------------------------------

// --- Command → call mapping -----------------------------------------------

/// The credential sent as `Authorization: Bearer`: the admin API key when one
/// is configured, otherwise the tunnel token (accepted by the endpoints that
/// take the master token, such as `api tunnel create`).
fn credential(settings: &ClientSettings) -> Option<String> {
  settings
    .api_key
    .clone()
    .or_else(|| settings.token.clone())
    .filter(|s| !s.trim().is_empty())
}

/// Runs one `aperio-client api ...` command and exits the process: the JSON
/// response goes to stdout with exit code 0, any failure to stderr with 1.
pub(crate) async fn run_api(
  settings: &ClientSettings,
  opts: &CommonOpts,
  command: &ApiCommand,
) -> ! {
  let Some(server) = settings.server.clone() else {
    eprintln!(
      "error: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)"
    );
    std::process::exit(1);
  };

  let call = match build_call(command, settings, opts) {
    Ok(call) => call,
    Err(e) => {
      eprintln!("error: {}", e);
      std::process::exit(1);
    }
  };

  crate::ensure_crypto_provider();
  let http = match reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    // Never follow the login redirect the dashboard router answers with; it
    // would turn a 401-in-spirit into a 200 page of HTML.
    .redirect(reqwest::redirect::Policy::none())
    .build()
  {
    Ok(client) => client,
    Err(e) => {
      eprintln!("error: failed to build the HTTP client: {}", e);
      std::process::exit(1);
    }
  };

  match send(&http, &server, credential(settings).as_deref(), call).await {
    Ok(Value::Null) => std::process::exit(0),
    // Non-JSON responses (the CSV export, plain-text acknowledgements) print
    // verbatim rather than as a quoted JSON string.
    Ok(Value::String(text)) => {
      println!("{}", text);
      std::process::exit(0);
    }
    Ok(value) => {
      println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
      );
      std::process::exit(0);
    }
    Err(e) => {
      eprintln!("error: {}", e);
      std::process::exit(1);
    }
  }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
