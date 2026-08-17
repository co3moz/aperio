//! Building the HTTP client a service forwards through, and the answer a
//! visitor gets when the backend could not be reached at all.
//!
//! The TLS floor is validated here rather than where it is used, so an
//! unusable value is a configuration error at startup instead of a service
//! that comes up and cannot dial.

use super::*;

/// Formats a generic masked error response, avoiding leaking raw socket error details.
pub(crate) fn make_error_response(id: String, status: u16) -> TunnelMessage {
  let headers = vec![("content-type".to_string(), "text/plain".to_string())];

  let user_error = match status {
    502 => "502 Bad Gateway - Target server connection failed.",
    400 => "400 Bad Request - Invalid request payload data.",
    _ => "500 Internal Server Error - Tunnel client failed to process request.",
  };

  let body = BASE64_STANDARD.encode(user_error.as_bytes());

  TunnelMessage::Response {
    id,
    status,
    headers,
    body: Some(body),
    trailers: None,
    timings: None,
  }
}

/// Parses `min_tls_version:` into what reqwest wants.
///
/// An unrecognized value is an error rather than a silent fallback: this
/// setting exists to raise a floor, and a typo that quietly leaves the floor
/// where it was is what makes a security setting worse than none at all.
pub(crate) fn tls_floor(raw: Option<&str>) -> Result<Option<reqwest::tls::Version>, String> {
  let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
    return Ok(None);
  };
  match raw
    .trim_start_matches(['T', 't'])
    .trim_start_matches(['L', 'l'])
    .trim_start_matches(['S', 's'])
    .trim_start_matches(['V', 'v'])
    .trim()
  {
    "1.2" | "12" => Ok(Some(reqwest::tls::Version::TLS_1_2)),
    "1.3" | "13" => Ok(Some(reqwest::tls::Version::TLS_1_3)),
    _ => Err(format!(
      "min_tls_version '{raw}' is not a TLS version (write 1.2 or 1.3)"
    )),
  }
}

/// The builder every request *to the backend* starts from.
///
/// **Never proxied, and that is the whole reason this exists.** reqwest reads
/// `HTTP_PROXY` / `HTTPS_PROXY` from the environment by default, and a machine
/// inside a company network usually has them set, because everything else on
/// it needs them to reach the internet. The backend is the one destination on
/// this client that is *not* the internet: it is `127.0.0.1:3000`, a container
/// beside us, a socket on this host. Sending those requests to a corporate
/// proxy asks it to reach an address only this machine can see, and it answers
/// with a refusal that reaches the visitor as a failure of their own site.
///
/// `NO_PROXY` covers the loopback case on a well-configured machine, which is
/// what kept this from being noticed: the bug appears only where that variable
/// is missing or does not list the backend's address, and then it appears for
/// every request. A default that depends on a second variable being right is
/// not a default. The direction of travel decides this, not the environment,
/// so it is decided here rather than at each call site.
///
/// The requests that *do* belong to the internet, the API and discovery calls
/// to the tunnel server, keep reading the environment and are built directly.
pub(crate) fn backend_client_builder() -> reqwest::ClientBuilder {
  reqwest::Client::builder().no_proxy()
}

/// The backend client used when a configured build fails.
///
/// Same rule with none of the options: a build only fails when the TLS stack
/// itself will not start, so this is a last resort that keeps the process
/// serving rather than a path worth tuning. It goes through the same helper so
/// the no-proxy rule cannot be lost in the fallback, which is exactly where a
/// rule applied by hand gets forgotten.
pub(crate) fn backend_client_fallback() -> reqwest::Client {
  backend_client_builder()
    .build()
    .unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
