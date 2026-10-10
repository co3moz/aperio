//! Answering the server's ask about a visitor (`forward` with `via: client`,
//! `planned_features.md` #157): the endpoint that decides lives on this
//! client's network, so the client calls it and sends the verdict back.
//!
//! The question arrives composed: the request line as `X-Forwarded-*` and
//! the allow-listed request headers, exactly as the server would have sent
//! them to an endpoint of its own. This side adds nothing to it. What goes
//! back is a status and headers, never a body: the endpoint was asked a
//! question, not asked to serve.

use std::time::Duration;

/// The endpoint's answer, as it travels back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
  pub(crate) status: u16,
  pub(crate) headers: Vec<(String, String)>,
  /// Why the endpoint could not be asked, when it could not.
  pub(crate) error: Option<String>,
}

/// The headers sent back from any answer, whatever the allowlist says: what
/// a browser needs to act on a refusal.
const ALWAYS_BACK: &[&str] = &["location", "content-type", "www-authenticate", "set-cookie"];

/// Whether this client may call `url` for the server.
///
/// A `forward` endpoint one of this client's own services declares is its
/// operator's choice, and is always answered. Any other URL is the server's
/// choice, a `forward` written in the server's own `auth:`, and calling it
/// means making a request from this client's network to wherever the server
/// says, the cloud metadata address and an internal admin port included.
/// That is answered only when this client said so with
/// `allow_server_forward`. Compared exactly: the server sends back the URL it
/// was told, byte for byte, so a near match is somebody else's URL.
pub(crate) fn may_call<'a>(
  url: &str,
  declared: impl IntoIterator<Item = Option<&'a aperio_config::AuthSetting>>,
  allow_server_forward: bool,
) -> bool {
  allow_server_forward
    || declared.into_iter().flatten().any(|policy| {
      policy
        .methods()
        .iter()
        .any(|m| aperio_config::forward_via_client(m) && m.url.as_deref() == Some(url))
    })
}

/// The answer to an ask [`may_call`] refused, saying what would allow it.
pub(crate) fn refused(url: &str) -> Answer {
  // Once per process at `warn`: the server's visitors decide how often this
  // fires, and the line says everything there is to say the first time.
  static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
  if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
    tracing::warn!(
      "The server asked this client to call {url} for a forward check that this client's own \
       config does not declare; refused. Set allow_server_forward: true \
       (APERIO_ALLOW_SERVER_FORWARD=1) to answer checks the server writes"
    );
  } else {
    tracing::debug!("Refused a server-written forward check of {url}");
  }
  Answer {
    status: 0,
    headers: Vec::new(),
    error: Some(format!(
      "the client calls only the forward endpoints its own config declares; {url} is the \
       server's, which takes allow_server_forward: true (APERIO_ALLOW_SERVER_FORWARD=1) on the \
       client"
    )),
  }
}

/// Asks `url` about the request the headers describe, within `timeout`.
pub(crate) async fn answer(
  url: &str,
  request_headers: &[(String, String)],
  response_headers: &[String],
  timeout: Duration,
) -> Answer {
  let Some(http) = client() else {
    return Answer {
      status: 0,
      headers: Vec::new(),
      error: Some("the HTTP client could not be built".to_string()),
    };
  };
  let mut req = http
    .get(url)
    .timeout(timeout.max(Duration::from_millis(100)));
  for (name, value) in request_headers {
    req = req.header(name.as_str(), value.as_str());
  }
  let res = match req.send().await {
    Ok(res) => res,
    Err(e) => {
      return Answer {
        status: 0,
        headers: Vec::new(),
        error: Some(e.to_string()),
      };
    }
  };
  let status = res.status().as_u16();
  let mut headers = Vec::new();
  for (name, value) in res.headers() {
    let name = name.as_str();
    let wanted = ALWAYS_BACK.iter().any(|h| h.eq_ignore_ascii_case(name))
      || response_headers
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name));
    if wanted && let Ok(value) = value.to_str() {
      headers.push((name.to_string(), value.to_string()));
    }
  }
  Answer {
    status,
    headers,
    error: None,
  }
}

/// One HTTP client for the life of the process, like the server's own
/// forward-auth client: a `reqwest::Client` owns a connection pool, and a
/// fresh one per ask would be a TCP and TLS handshake per visitor request.
/// Redirects are not followed: a `302` is the endpoint's own refusal, sent
/// back for the visitor, not a place for this client to go.
fn client() -> Option<&'static reqwest::Client> {
  static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
  CLIENT
    .get_or_init(|| {
      reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()
    })
    .as_ref()
}

#[cfg(test)]
#[path = "forward_ask_tests.rs"]
mod tests;
