//! Everything one service needs to turn a dispatched request into a backend
//! request: where it goes, what headers it carries, and which redirects are
//! followed rather than passed back to the visitor.

use super::*;

/// True when two hosts belong to the same site for redirect purposes:
/// identical hosts, or hostnames sharing at least the last two DNS labels
/// (`example.com` ↔ `test.example.com`, `a.example.com` ↔ `b.example.com`).
/// IP addresses and single-label hosts (`localhost`) only match exactly.
pub(crate) fn same_site(a: &str, b: &str) -> bool {
  let a = a.trim_end_matches('.').to_ascii_lowercase();
  let b = b.trim_end_matches('.').to_ascii_lowercase();
  if a == b {
    return true;
  }
  // IP literals never match a different host.
  if a.parse::<std::net::IpAddr>().is_ok() || b.parse::<std::net::IpAddr>().is_ok() {
    return false;
  }
  let shared = a
    .rsplit('.')
    .zip(b.rsplit('.'))
    .take_while(|(x, y)| x == y)
    .count();
  shared >= 2
}

/// Redirect policy for backend requests: follows same-host scheme upgrades
/// (`http://x` → `https://x`) and redirects within the same root domain, up
/// to `max_hops` jumps and never downgrading https to http. Anything else,
/// including the hop after the limit, is passed through to the visitor as a
/// regular redirect response, exactly like `Policy::none`.
pub(crate) fn redirect_policy(max_hops: usize) -> reqwest::redirect::Policy {
  if max_hops == 0 {
    return reqwest::redirect::Policy::none();
  }
  reqwest::redirect::Policy::custom(move |attempt| {
    if attempt.previous().len() > max_hops {
      return attempt.stop();
    }
    let orig = match attempt.previous().first() {
      Some(u) => u.clone(),
      None => return attempt.stop(),
    };
    let next = attempt.url();
    // https → http downgrades and non-http schemes are never followed.
    let scheme_ok = matches!(
      (orig.scheme(), next.scheme()),
      ("http", "http") | ("http", "https") | ("https", "https")
    );
    let host_ok = match (orig.host_str(), next.host_str()) {
      (Some(a), Some(b)) => same_site(a, b),
      _ => false,
    };
    if scheme_ok && host_ok {
      attempt.follow()
    } else {
      attempt.stop()
    }
  })
}

impl HeaderTransform {
  /// Compiles the config directives for one direction (None = no edits).
  pub(crate) fn compile(directives: Option<&aperio_config::HeaderDirectives>) -> Self {
    let Some(d) = directives else {
      return HeaderTransform::default();
    };
    let mut remove: std::collections::HashSet<String> =
      d.remove.iter().map(|n| n.to_ascii_lowercase()).collect();
    let add: Vec<(String, String)> = d.add.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (name, _) in &add {
      remove.insert(name.to_ascii_lowercase());
    }
    HeaderTransform { add, remove }
  }

  /// True when there is nothing to do (the fast path for most services).
  fn is_empty(&self) -> bool {
    self.add.is_empty() && self.remove.is_empty()
  }

  /// Applies the rules to a header list: strips removals (and old values of
  /// re-added names), then appends the additions.
  pub(crate) fn apply(&self, mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
    if self.is_empty() {
      return headers;
    }
    headers.retain(|(k, _)| !self.remove.contains(&k.to_ascii_lowercase()));
    headers.extend(self.add.iter().cloned());
    headers
  }
}

/// Per-connection constants for request forwarding, so per-request calls
/// only carry the request itself.
pub(crate) struct ForwardContext {
  /// HTTP client used for all backend calls on this connection.
  pub(crate) client: reqwest::Client,
  /// Local backend base URL.
  pub(crate) target: String,
  /// The same URL, parsed once. It does not change for the life of the
  /// connection, and parsing it per request showed up in a profile as its own
  /// line: `url::Url::parse` is not cheap, and this one had the same answer
  /// every time. `None` for a target that is not a URL at all, which is a
  /// configuration error and answers 502, the same as when the parse lived in
  /// the request path.
  pub(crate) target_url: Option<url::Url>,
  /// Forward the original `Host` header instead of the target's.
  pub(crate) pass_hostname: bool,
  /// Path bind of this client, stripped from incoming paths when `trim_bind`.
  pub(crate) path_bind: Option<String>,
  pub(crate) trim_bind: bool,
  /// Cap on response bodies read from the backend.
  pub(crate) max_response_body_size: usize,
  /// Write half of the tunnel, used for streamed responses.
  pub(crate) tunnel_tx: mpsc::Sender<Message>,
  /// Header edits applied to forwarded requests (config `headers.request`).
  pub(crate) request_headers: HeaderTransform,
  /// Header edits applied to backend responses (config `headers.response`).
  pub(crate) response_headers: HeaderTransform,
  /// HTTP/2 client for `h2c://` / `h2://` targets (None = plain HTTP target).
  pub(crate) h2_client: Option<std::sync::Arc<crate::proxy::h2::H2Client>>,
  /// Filesystem path of a `unix://` target's socket (None = TCP target).
  pub(crate) unix_socket: Option<String>,
  /// Seconds to wait for the backend's response head on the HTTP/2 path
  /// (the reqwest path carries its timeout inside `client`).
  pub(crate) timeout_secs: u64,
  /// Pause switches for the streams this connection produces (server flow
  /// control, protocol v3); streamed response bodies register here.
  pub(crate) stream_pauses: crate::flow::PauseRegistry,
  /// Retry policy and circuit breaker for this service's backend.
  pub(crate) resilience: BackendResilience,
}

/// Builds the backend URL for an incoming proxied request: maps the path
/// (optionally stripping the path bind), copies the query, and verifies the
/// result still points at the configured target (SSRF defence-in-depth).
/// Errors are the HTTP status to answer with.
pub(crate) fn build_dest_url(
  ctx: &ForwardContext,
  id: &str,
  uri_str: &str,
) -> Result<url::Url, u16> {
  let Some(target_parsed) = ctx.target_url.as_ref() else {
    error!("Failed to parse local target URL: {:?}", ctx.target);
    return Err(502);
  };
  // The path and the query. Origin-form (`/a/b?c`) is every HTTP/1.1 visitor
  // and splits without allocating; parsing `http://localhost{uri}` for it ran
  // a full URL parse per request to reach two `&str`s, and `set_path` below
  // normalizes what that parse normalized, which
  // `splitting_the_uri_agrees_with_parsing_it_as_a_url` holds to.
  //
  // Absolute-form (`http://host/a`) arrives from HTTP/2 visitors, where the
  // URI is rebuilt from `:scheme` and `:authority`. It is parsed, once, for
  // the shape that needs it. That also fixes what the old expression did with
  // it: prefixing `http://localhost` made the whole URI the *path*, so the
  // backend received `/127.0.0.1:8080/echo` instead of `/echo`.
  let uri_str = if uri_str.is_empty() { "/" } else { uri_str };
  let absolute;
  let (incoming_path_raw, incoming_query) = if uri_str.starts_with('/') {
    match uri_str.split_once('?') {
      Some((path, query)) => (path, Some(query)),
      None => (uri_str, None),
    }
  } else {
    absolute = match url::Url::parse(uri_str) {
      Ok(url) => url,
      Err(e) => {
        error!("Failed to parse incoming proxy URI {:?}: {:?}", uri_str, e);
        return Err(400);
      }
    };
    (absolute.path(), absolute.query())
  };

  let mut dest_url = target_parsed.clone();
  let target_path = target_parsed.path().trim_end_matches('/');
  let mut incoming_path = incoming_path_raw.trim_start_matches('/').to_string();
  if ctx.trim_bind
    && let Some(ref bind) = ctx.path_bind
  {
    let bind_trimmed = bind.trim_matches('/');
    // Match only at a path-segment boundary: bind `/api` trims `/api` and
    // `/api/x` but must NOT match `/apiv2/x` (which is a different route).
    let matches_bind = match incoming_path.strip_prefix(bind_trimmed) {
      Some(rest) => rest.is_empty() || rest.starts_with('/'),
      None => false,
    };
    if matches_bind {
      incoming_path = incoming_path[bind_trimmed.len()..]
        .trim_start_matches('/')
        .to_string();
    }
  }
  let combined_path = if target_path.is_empty() {
    format!("/{}", incoming_path)
  } else {
    format!("{}/{}", target_path, incoming_path)
  };
  dest_url.set_path(&combined_path);
  dest_url.set_query(incoming_query);

  if dest_url.scheme() != target_parsed.scheme()
    || dest_url.host_str() != target_parsed.host_str()
    || dest_url.port_or_known_default() != target_parsed.port_or_known_default()
  {
    error!(
      "SSRF protection triggered: constructed URL diverges from target for request ID {}",
      id
    );
    return Err(400);
  }
  Ok(dest_url)
}

/// One proxied request as received from the tunnel.
pub(crate) struct ForwardRequest {
  pub(crate) id: String,
  pub(crate) method: String,
  pub(crate) uri: String,
  pub(crate) headers: Vec<(String, String)>,
  /// Base64-encoded buffered body (None when absent, streamed, or carried as
  /// bytes in a v6 frame).
  pub(crate) body: Option<String>,
  /// The buffered body as bytes, from a v6 full-request frame. Takes
  /// precedence over `body`: only one of the two is ever set, and this one
  /// costs no decode.
  pub(crate) raw_body: Option<Vec<u8>>,
}

/// How an attempt at the backend failed, for the two paths that dial with
/// hyper directly and so have to tell a timeout from a transport error
/// themselves.
///
/// Both are failures before any response arrived, which is what
/// `retry.attempts` covers, so both go round the retry loop; they part
/// company only at the end, where a stalled backend is a `504` and an
/// unreachable one a `502`.
pub(crate) enum Failure<E> {
  Timeout,
  Backend(E),
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
