//! Making one admin API call and printing its answer: the request shape, the
//! credential scope it is sent under, and the small readers for an argument
//! that may arrive as a file or on stdin.

use super::*;

/// A prepared call: method, path under the server root, query pairs, body.
pub(crate) struct Call {
  pub(crate) method: reqwest::Method,
  pub(crate) path: String,
  pub(crate) query: Vec<(String, String)>,
  pub(crate) body: Option<Value>,
  /// Bearer credential for this call when it differs from the configured
  /// admin key (token self-refresh presents the token secret itself).
  pub(crate) auth: Option<String>,
}

impl Call {
  pub(crate) fn new(method: reqwest::Method, path: impl Into<String>) -> Self {
    Self {
      method,
      path: path.into(),
      query: Vec::new(),
      body: None,
      auth: None,
    }
  }
  pub(crate) fn get(path: impl Into<String>) -> Self {
    Self::new(reqwest::Method::GET, path)
  }
  pub(crate) fn post(path: impl Into<String>, body: Value) -> Self {
    Self::new(reqwest::Method::POST, path).with_body(body)
  }
  pub(crate) fn put(path: impl Into<String>, body: Value) -> Self {
    Self::new(reqwest::Method::PUT, path).with_body(body)
  }
  pub(crate) fn delete(path: impl Into<String>) -> Self {
    Self::new(reqwest::Method::DELETE, path)
  }
  pub(crate) fn with_body(mut self, body: Value) -> Self {
    self.body = Some(body);
    self
  }
  pub(crate) fn with_auth(mut self, secret: impl Into<String>) -> Self {
    self.auth = Some(secret.into());
    self
  }
  pub(crate) fn query(mut self, key: &str, value: Option<impl ToString>) -> Self {
    if let Some(v) = value {
      self.query.push((key.to_string(), v.to_string()));
    }
    self
  }
}

/// Inserts `key` into a JSON object only when the value is present, so the
/// server's `Option`/`#[serde(default)]` fields keep their "absent" meaning.
pub(crate) fn put_opt(map: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
  if let Some(v) = value {
    map.insert(key.to_string(), v.into());
  }
}

/// Resolves an `--expire` flag into a `ttl_seconds` value. `never` yields
/// `Some(0)` for endpoints where 0 means "no expiry"; when `never_omits` is
/// true (token/admin-key creation, where an absent field means "never"), it
/// yields `None` instead.
pub(crate) fn ttl_field(expire: &Option<String>, never_omits: bool) -> Result<Option<u64>, String> {
  match expire {
    None => Ok(None),
    Some(raw) => {
      let secs = parse_duration(raw)?;
      if secs == 0 && never_omits {
        Ok(None)
      } else {
        Ok(Some(secs))
      }
    }
  }
}

/// Reads a value that may be `-` (meaning: read it from stdin), used for
/// passwords and JSON documents so secrets need not appear in shell history.
pub(crate) fn read_maybe_stdin(value: &str) -> Result<String, String> {
  if value != "-" {
    return Ok(value.to_string());
  }
  use std::io::Read;
  let mut buf = String::new();
  std::io::stdin()
    .read_to_string(&mut buf)
    .map_err(|e| format!("failed to read stdin: {}", e))?;
  Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Loads a JSON document from a file path (or stdin for `-`).
pub(crate) fn read_json_file(path: &str) -> Result<Value, String> {
  let raw = if path == "-" {
    read_maybe_stdin("-")?
  } else {
    std::fs::read_to_string(path).map_err(|e| format!("failed to read {}: {}", path, e))?
  };
  serde_json::from_str(&raw).map_err(|e| format!("{} is not valid JSON: {}", path, e))
}

/// Performs one admin API call and returns the decoded response. A JSON body
/// decodes into a `Value`; anything else (CSV, plain text) comes back as a
/// JSON string so the caller can print it verbatim.
pub(crate) async fn send(
  http: &reqwest::Client,
  server: &str,
  credential: Option<&str>,
  call: Call,
) -> Result<Value, String> {
  let url = build_http_url(server, &call.path)?;
  let mut parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
  if !call.query.is_empty() {
    let mut pairs = parsed.query_pairs_mut();
    for (k, v) in &call.query {
      pairs.append_pair(k, v);
    }
    drop(pairs);
  }

  let mut req = http.request(call.method.clone(), parsed.as_str());
  if let Some(secret) = call.auth.as_deref().or(credential) {
    req = req.bearer_auth(secret);
  }
  // A `null` body means "no body": the bodyless POST endpoints (replay,
  // refire, redeliver) have no JSON extractor to satisfy.
  if let Some(body) = call.body.as_ref().filter(|b| !b.is_null()) {
    req = req.json(body);
  }
  let response = req
    .send()
    .await
    .map_err(|e| format!("request to {} failed: {}", parsed, e))?;

  let status = response.status();
  // The dashboard router answers an unauthenticated API call with a redirect
  // to the login page. Following it would yield a 200 with HTML, so surface
  // it as the authentication error it actually is.
  if status.is_redirection() {
    return Err(
      "authentication required: pass an admin key with --api-key (or APERIO_API_KEY / yaml server.api_key)"
        .to_string(),
    );
  }
  let text = response.text().await.unwrap_or_default();
  if !status.is_success() {
    let detail = text.trim();
    return Err(if detail.is_empty() {
      format!("server returned {}", status)
    } else {
      format!("server returned {}: {}", status, detail)
    });
  }
  if text.trim().is_empty() {
    return Ok(Value::Null);
  }
  Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/// The host/path scope of an api command. It comes from the client's own
/// global `--hostname` / `--path` flags, which mean the same thing here as
/// they do for a tunnel: comma-separated lists are accepted wherever the
/// endpoint takes several.
pub(crate) struct Scope {
  pub(crate) hostnames: Vec<String>,
  pub(crate) paths: Vec<String>,
}

impl Scope {
  pub(crate) fn from_opts(opts: &CommonOpts) -> Self {
    let split = |raw: &Option<String>| -> Vec<String> {
      raw
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
    };
    Self {
      hostnames: split(&opts.hostname),
      paths: split(&opts.path),
    }
  }
  /// The single hostname the command acts on, if one was given.
  pub(crate) fn hostname(&self) -> Option<String> {
    self.hostnames.first().cloned()
  }
  /// The single path the command acts on, if one was given.
  pub(crate) fn path(&self) -> Option<String> {
    self.paths.first().cloned()
  }
  /// The hostname of a command that cannot work without one.
  pub(crate) fn require_hostname(&self) -> Result<String, String> {
    self
      .hostname()
      .ok_or_else(|| "a hostname is required (--hostname app.example.com)".to_string())
  }
}
