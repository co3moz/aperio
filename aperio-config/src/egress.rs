//! Where an outbound connection goes on a network that has no direct route
//! out: the proxy in front of it, and the destinations that skip it.
//!
//! Shared by both binaries because both need the same value and neither owns
//! it. The client dials the tunnel server through a proxy
//! (`planned_features.md` #117) and the server makes its callbacks through one
//! (#118), so the parsing, the redaction and the bypass rule live here once
//! and each side adds only the shape it needs: the client turns this into a
//! `CONNECT` request, the server turns it into a reqwest proxy.
//!
//! **Nothing here prints a credential.** A proxy value may carry
//! `user:password@`, so [`EgressProxy`] keeps host and port in a separate
//! redacted string, writes its own `Debug`, and hides the credential even in
//! the error returned for a value that failed to parse, since that error is
//! printed too.

use std::fmt;

/// A proxy an operator pointed a binary at.
#[derive(Clone, PartialEq, Eq)]
pub struct EgressProxy {
  host: String,
  port: u16,
  credentials: Option<(String, String)>,
  redacted: String,
}

impl fmt::Debug for EgressProxy {
  /// Hand-written: a derived one would put the password in any future `{:?}`.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "EgressProxy({})", self.redacted)
  }
}

impl EgressProxy {
  /// Parses `[scheme://][user:password@]host[:port]`.
  ///
  /// An `https://` proxy, meaning TLS to the proxy itself, is refused rather
  /// than quietly dialed in the clear: treating it as plaintext would hand the
  /// proxy credential to the network the scheme was chosen to hide it from.
  pub fn parse(raw: &str) -> Result<EgressProxy, String> {
    let raw = raw.trim();
    if raw.is_empty() {
      return Err("egress proxy is empty".to_string());
    }
    let (scheme, rest) = match raw.split_once("://") {
      Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
      None => ("http".to_string(), raw),
    };
    match scheme.as_str() {
      "http" => {}
      "https" => {
        return Err(
          "the egress proxy is an https:// proxy, which is TLS to the proxy itself and \
           not supported. Use the http:// form; what travels inside it is still TLS \
           end to end."
            .to_string(),
        );
      }
      other => {
        return Err(format!(
          "egress proxy scheme '{other}' is not supported (write http:// or host:port)"
        ));
      }
    }
    // A path is meaningless for a CONNECT proxy, and dropping one silently
    // would hide a typo such as a whole URL pasted in.
    let rest = rest.trim_end_matches('/');
    if rest.contains('/') {
      return Err(format!(
        "egress proxy '{}' has a path; a proxy is a host and a port",
        redact_raw(raw)
      ));
    }
    // The last `@`, so a password containing one stays with the credential.
    let (credential, hostport) = match rest.rsplit_once('@') {
      Some((credential, hostport)) => (Some(credential), hostport),
      None => (None, rest),
    };
    if hostport.is_empty() {
      return Err("egress proxy names no host".to_string());
    }
    let (host, port) = split_host_port(hostport)?;
    // A `@` with nothing in front of it is a typo, not a credential. Left as
    // one it would be worse than ignored: `has_credentials` would say yes, the
    // startup line would report the proxy as authenticated, and an empty
    // `Basic` would go on the wire, so a proxy's `407` would be reported as a
    // rejected password the operator never wrote.
    if credential.is_some_and(|c| c.is_empty()) {
      return Err(format!(
        "egress proxy '{}' has a '@' with no credential in front of it",
        redact_raw(raw)
      ));
    }
    // The *first* colon splits them: a password may contain one, a user name
    // may not, which is what basic auth's own grammar says.
    let credentials = credential.map(|c| match c.split_once(':') {
      Some((user, password)) => (user.to_string(), password.to_string()),
      None => (c.to_string(), String::new()),
    });
    Ok(EgressProxy {
      redacted: format!("{host}:{port}"),
      host,
      port,
      credentials,
    })
  }

  /// The proxy's host, as written.
  pub fn host(&self) -> &str {
    &self.host
  }

  /// The proxy's port.
  pub fn port(&self) -> u16 {
    self.port
  }

  /// User and password, for the caller that has to present them.
  pub fn credentials(&self) -> Option<(&str, &str)> {
    self
      .credentials
      .as_ref()
      .map(|(u, p)| (u.as_str(), p.as_str()))
  }

  /// Whether a credential was configured, without revealing it.
  pub fn has_credentials(&self) -> bool {
    self.credentials.is_some()
  }

  /// Host and port, never the credential. Everything user-facing uses this.
  pub fn redacted(&self) -> &str {
    &self.redacted
  }

  /// The proxy as a URL an HTTP client will accept.
  ///
  /// **Here rather than at the call sites, because of the bracket.** An IPv6
  /// literal is stored unbracketed, and `http://2001:db8::1:3128` is not a
  /// URL: it fails to parse with `InvalidPort`. Both callers formatted the
  /// authority by hand and both got it wrong, and on the server the parse
  /// failure turned into `None`, which reqwest reads as "do not proxy this
  /// request", so a configured proxy was skipped with nothing said. One
  /// method, so there is one place that knows about the bracket.
  ///
  /// The credential is deliberately *not* in the URL: it goes in a header the
  /// caller adds, so it cannot be logged as part of a destination.
  pub fn url(&self) -> String {
    if self.host.contains(':') {
      // Unbracketed colons mean an IPv6 literal; a hostname cannot contain one.
      format!("http://[{}]:{}", self.host, self.port)
    } else {
      format!("http://{}:{}", self.host, self.port)
    }
  }
}

/// Splits `host:port`, including the `[v6]:port` form.
fn split_host_port(raw: &str) -> Result<(String, u16), String> {
  if let Some(rest) = raw.strip_prefix('[') {
    let (addr, tail) = rest
      .split_once(']')
      .ok_or_else(|| format!("egress proxy '{raw}' opens a bracket it never closes"))?;
    let port = tail
      .strip_prefix(':')
      .ok_or_else(|| format!("egress proxy '{raw}' needs a port after the address"))?;
    return Ok((addr.to_string(), parse_port(port, raw)?));
  }
  match raw.rsplit_once(':') {
    Some((host, port)) if !host.is_empty() => Ok((host.to_string(), parse_port(port, raw)?)),
    // A colon with nothing before it names a port and forgets the host. It
    // used to fall through to the branch below and become a *hostname* of
    // ":3128", which parses, starts, and then fails somewhere else entirely.
    // Startup is the one place a typo in this setting is still cheap.
    Some(("", _)) => Err(format!("egress proxy '{raw}' names a port but no host")),
    // No port at all is the scheme default, which is what every other tool
    // does with the same string. Named in the docs, since a proxy on 80 is
    // unusual and a missing port is more often an omission than a choice.
    _ => Ok((raw.to_string(), 80)),
  }
}

fn parse_port(raw: &str, whole: &str) -> Result<u16, String> {
  raw
    .parse::<u16>()
    .map_err(|_| format!("egress proxy '{whole}' has no usable port"))
}

/// Hides a credential in a value that failed to parse, since that failure is
/// printed and the value is exactly the kind most likely to be a paste of
/// something private.
fn redact_raw(raw: &str) -> String {
  match raw.rsplit_once('@') {
    Some((_, tail)) => format!("***@{tail}"),
    None => raw.to_string(),
  }
}

/// The destinations that skip the proxy and are dialed directly.
///
/// **Ours rather than the HTTP client's.** `NO_PROXY` was measured not to
/// behave as a bypass-everything switch even when set to `*`, so a rule that
/// matters (an internal auth endpoint that must not be handed to a company
/// proxy, and on the server a destination whose policy check only means
/// something when we connect ourselves) cannot be delegated to a library's
/// own parsing of an ambient variable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressBypass {
  entries: Vec<BypassEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BypassEntry {
  /// An exact hostname, case-insensitive.
  Host(String),
  /// `.suffix` or `*.suffix`: that name and anything under it.
  Suffix(String),
}

impl EgressBypass {
  /// Parses a comma-separated list. Empty entries are skipped rather than
  /// refused, since a trailing comma is not a mistake worth a failed start.
  pub fn parse(raw: &str) -> EgressBypass {
    let mut entries = Vec::new();
    for entry in raw.split(',') {
      let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
      if entry.is_empty() {
        continue;
      }
      // `.corp` and `*.corp` are the two spellings people write for the same
      // thing, and both cover `corp` itself: somebody listing a domain means
      // the domain.
      match entry.strip_prefix("*.").or_else(|| entry.strip_prefix('.')) {
        Some(suffix) if !suffix.is_empty() => entries.push(BypassEntry::Suffix(suffix.to_string())),
        _ => entries.push(BypassEntry::Host(entry)),
      }
    }
    EgressBypass { entries }
  }

  /// Whether this list is empty.
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  /// How many entries it has, for a startup line that says what is in force.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// Whether `host` is dialed directly rather than through the proxy.
  ///
  /// Loopback is always direct, listed or not: a destination on this machine
  /// cannot be reached by asking a proxy elsewhere to fetch it, and making an
  /// operator write that down would be a rule whose only correct value is the
  /// one we could have chosen.
  pub fn covers(&self, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]" {
      return true;
    }
    self.entries.iter().any(|entry| match entry {
      BypassEntry::Host(name) => *name == host,
      BypassEntry::Suffix(suffix) => host == *suffix || host.ends_with(&format!(".{suffix}")),
    })
  }
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
