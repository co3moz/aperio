//! Optional policy over where the server may send *outbound* callbacks:
//! webhook deliveries and autoscaling hooks (planned_features #15).
//!
//! Both features take a URL from a lower-trust party (an Operator creating a
//! webhook, a client declaring `scaling.url`) and then have the server call
//! it, which makes the server a potential blind SSRF probe into its own
//! network. Blocking private addresses outright would break the *normal*
//! deployment, where the receiver lives on the same network, so the policy
//! is opt-in and defaults to today's permissive behaviour:
//!
//! - `outbound.allowlist` (`APERIO_OUTBOUND_ALLOWLIST`): host/CIDR patterns
//!   the server may call. When set, it is the policy: a destination either
//!   matches an entry or is refused, and a matching entry is trusted even if
//!   it is private (the operator named it).
//! - `outbound.block_private` (`APERIO_OUTBOUND_BLOCK_PRIVATE`): with no
//!   allowlist, refuse destinations that resolve to internal addresses
//!   (loopback, RFC 1918, link-local, CGNAT/metadata, unique-local).
//!
//! Enforced when the call is made, not only when the URL is stored, so a
//! policy added later also covers webhooks created before it.
//!
//! It also owns *how* those calls leave (#118), because the two questions
//! turn out to be one:
//!
//! - `outbound.proxy` (`APERIO_OUTBOUND_PROXY`): the proxy to send them
//!   through, on a network with no direct route out.
//! - `outbound.no_proxy` (`APERIO_OUTBOUND_NO_PROXY`): the destinations that
//!   skip it, for the auth endpoint or issuer inside your own network.
//!
//! **A proxy changes what the policy can decide, so the policy has to know
//! about it.** Half of the policy reads the URL as text (a literal address, a
//! hostname pattern) and is unaffected. The other half resolves the name here
//! and looks at the addresses, and through a proxy the name is resolved on the
//! proxy's network and connected to from there, so that half would be judging
//! an answer that does not apply. It is therefore not run, the startup log
//! says so, and the destinations on the bypass list keep the whole policy
//! because the server dials those itself.
//!
//! The ambient `HTTP_PROXY` family is not read by anything here. It used to be
//! read by the HTTP client, which is how a deployment could be proxying its
//! callbacks with nothing saying so and this policy unable to tell.

use std::net::IpAddr;

/// One allowlist entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutboundPattern {
  /// Exact hostname (case-insensitive, no trailing dot).
  Host(String),
  /// `*.suffix`: any subdomain of `suffix` (but not `suffix` itself).
  Suffix(String),
  /// An IP or CIDR range, matched against IP-literal hosts *and* against
  /// what a hostname resolves to.
  Cidr(IpAddr, u32),
}

/// Parses a comma-separated allowlist. Each entry is a hostname, a
/// `*.suffix` wildcard, an IP, or a CIDR. An unparseable entry is an error:
/// starting with a partial allowlist would silently refuse legitimate
/// destinations (or admit ones the operator meant to exclude).
pub(crate) fn parse_patterns(raw: &str) -> Result<Vec<OutboundPattern>, String> {
  let mut out = Vec::new();
  for entry in raw.split(',') {
    let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    if entry.is_empty() {
      continue;
    }
    if let Some(suffix) = entry.strip_prefix("*.") {
      if suffix.is_empty() || suffix.contains('*') || suffix.contains('/') {
        return Err(format!("invalid wildcard pattern '{entry}'"));
      }
      out.push(OutboundPattern::Suffix(suffix.to_string()));
      continue;
    }
    if entry.contains('*') {
      return Err(format!(
        "invalid pattern '{entry}' (wildcards only as a leading '*.')"
      ));
    }
    // An IP or CIDR entry parses via the shared trusted-proxies grammar;
    // anything else is a literal hostname.
    if entry.parse::<IpAddr>().is_ok() || entry.contains('/') {
      let mut ranges = crate::routing::parse_trusted_proxies(&entry)?;
      out.push(match ranges.pop() {
        Some((base, bits)) => OutboundPattern::Cidr(base, bits),
        None => return Err(format!("invalid entry '{entry}'")),
      });
      continue;
    }
    out.push(OutboundPattern::Host(entry));
  }
  Ok(out)
}

/// The environment variables an HTTP client reads before it connects.
///
/// Both spellings of each, because that is how they are read: whichever is
/// found first decides, so finding either one is finding a proxy.
const PROXY_ENV_VARS: [&str; 6] = [
  "HTTP_PROXY",
  "http_proxy",
  "HTTPS_PROXY",
  "https_proxy",
  "ALL_PROXY",
  "all_proxy",
];

/// Proxy variables set in this process's environment.
///
/// **Nothing acts on these any more, which is why they have to be reported.**
/// The HTTP client used to read them by default, so a deployment could be
/// sending its callbacks through a proxy with nothing in its configuration
/// saying so, and the policy below could not tell. Now the server is told
/// instead, through `APERIO_OUTBOUND_PROXY`, and this exists only to notice the
/// machine that still has the old variables set and say that they no longer
/// do anything, rather than let a working route disappear in silence.
///
/// `NO_PROXY` is not among them, and not treated as an escape anywhere. It is
/// measured behaviour, not caution: a request to a loopback address with
/// `NO_PROXY=*` set still went to the proxy, so a rule that matters cannot be
/// delegated to a library's reading of that variable. `APERIO_OUTBOUND_NO_PROXY`
/// is ours and is applied by us.
pub(crate) fn proxy_env_vars() -> Vec<&'static str> {
  proxy_vars_from(|name| std::env::var(name).ok())
}

/// The same question asked of any environment.
///
/// Split out so the rule can be tested without writing to this process's own
/// environment: that is global to every test sharing the binary, and a test
/// that sets `HTTP_PROXY` would be deciding what an unrelated one sees.
fn proxy_vars_from(lookup: impl Fn(&str) -> Option<String>) -> Vec<&'static str> {
  PROXY_ENV_VARS
    .into_iter()
    .filter(|name| {
      lookup(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    })
    .collect()
}

/// Where this server's outbound calls go when the network has no direct route
/// out (`planned_features.md` #118).
///
/// Configuration rather than ambient environment, and that is the point. The
/// `HTTP_PROXY` family was being read by the HTTP client all along, so a
/// deployment could be sending its callbacks through a proxy without anything
/// in the deployment saying so, and the policy below could not know. Here the
/// server is *told*, which is what lets the two be reconciled instead of
/// silently disagreeing.
#[derive(Clone, Debug, Default)]
pub(crate) struct Egress {
  pub(crate) proxy: Option<aperio_config::egress::EgressProxy>,
  pub(crate) bypass: aperio_config::egress::EgressBypass,
}

impl Egress {
  /// Whether a call to `host` is made by this server directly.
  ///
  /// True when there is no proxy at all, and for the destinations the bypass
  /// list keeps off it. It decides more than routing: a direct call is one
  /// whose address this server chooses, so it is the only kind the policy's
  /// resolution-based half can judge.
  pub(crate) fn direct_to(&self, host: &str) -> bool {
    self.proxy.is_none() || self.bypass.covers(host)
  }

  /// The builder every outbound HTTP client starts from.
  ///
  /// **The ambient environment is never consulted.** reqwest reads
  /// `HTTP_PROXY` and friends by default, which is how a deployment ended up
  /// proxying its callbacks without saying so and without the policy above
  /// being able to tell. `no_proxy()` turns that off for good, and whatever
  /// route this server takes is the one it was configured to take.
  ///
  /// The proxy is applied per destination rather than wholesale, because the
  /// common server has both kinds at once: a `forward` auth endpoint on the
  /// same host and a webhook receiver on the internet. `Proxy::custom` is
  /// given the bypass rule so one client serves both.
  pub(crate) fn client_builder(&self) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder().no_proxy();
    let Some(ref proxy) = self.proxy else {
      return builder;
    };
    let url = proxy.url();
    let bypass = self.bypass.clone();
    let routed = reqwest::Proxy::custom(move |target| {
      match target.host_str() {
        // A destination on the bypass list, or on this machine, is ours to
        // dial: handing it to a proxy elsewhere could not reach it anyway.
        Some(host) if bypass.covers(host) => None,
        _ => url.parse::<reqwest::Url>().ok(),
      }
    });
    let routed = match proxy.credentials() {
      Some((user, password)) => routed.basic_auth(user, password),
      None => routed,
    };
    builder.proxy(routed)
  }
}

/// The egress every outbound client is built from.
///
/// Process-wide, because it is: the value comes from configuration read once
/// at startup and no call can be made under a different one. A global rather
/// than a parameter threaded through six call chains for a value that never
/// varies, and the same shape the client uses for its own dial settings.
///
/// It is set from the same configuration as `OutboundPolicy::egress`, in one
/// place, so the copy the policy reasons about and the copy the client dials
/// through cannot disagree. The policy keeps its own so that `check` can be
/// tested without a global.
static EGRESS: std::sync::OnceLock<Egress> = std::sync::OnceLock::new();

/// Records the process's egress. Idempotent; a second call is ignored.
pub(crate) fn set_egress(egress: Egress) {
  let _ = EGRESS.set(egress);
}

/// The builder every outbound HTTP client in the server starts from.
///
/// One constructor rather than seven hand-assembled ones, which is also what
/// keeps the four-part rule (no ambient proxy, the configured proxy, no
/// redirects, a bounded read) from being enforced by repetition.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
  match EGRESS.get() {
    Some(egress) => egress.client_builder(),
    // Before startup has run, and in tests: still never the environment.
    None => reqwest::Client::builder().no_proxy(),
  }
}

/// The effective outbound policy, snapshotted into the server config at
/// startup. Default: no restriction (today's behaviour).
#[derive(Clone, Debug, Default)]
pub(crate) struct OutboundPolicy {
  pub(crate) allowlist: Vec<OutboundPattern>,
  pub(crate) block_private: bool,
  /// How the calls this policy judges actually leave.
  ///
  /// Part of the policy rather than beside it, because it changes what the
  /// policy *means*: a destination reached through a proxy is one whose name
  /// the proxy resolves on its own network, so the addresses checked here are
  /// not the addresses connected to. The policy is not weaker everywhere, it
  /// is weaker for exactly those destinations, and it can only say so if it
  /// knows which they are.
  pub(crate) egress: Egress,
}

impl OutboundPolicy {
  /// Whether any restriction is configured at all: the fast path for the
  /// default deployment skips URL parsing and resolution entirely.
  pub(crate) fn restricted(&self) -> bool {
    !self.allowlist.is_empty() || self.block_private
  }

  /// Checks one destination URL against the policy. `Ok(())` when the
  /// policy is empty, when the host matches an allowlist entry, or when
  /// (allowlist-less) every resolved address is public.
  ///
  /// **What a proxy changes.** For a destination this server reaches through
  /// a proxy, the name is resolved by the proxy and the connection is made
  /// from there, so resolving it here decides nothing about where the request
  /// ends up. Those checks are therefore not performed rather than performed
  /// on the wrong answer: a CIDR entry cannot match a hostname, and
  /// `block_private` covers only an address written as one. Everything the
  /// URL says as text (a literal IP, a hostname pattern) is judged exactly as
  /// before, because the name is what the proxy is handed. The startup log
  /// says which half is in force, and the destinations on the bypass list are
  /// dialed by this server and keep the whole policy.
  pub(crate) async fn check(&self, url_str: &str) -> Result<(), String> {
    if !self.restricted() {
      return Ok(());
    }
    let url = url::Url::parse(url_str).map_err(|e| format!("invalid url: {e}"))?;
    let Some(host) = url.host_str() else {
      return Err("no host in the URL".to_string());
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    // Bracketed IPv6 literals come back as `[::1]` from host_str.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let literal_ip = bare.parse::<IpAddr>().ok();
    // A literal address is still judged under a proxy: the proxy is asked to
    // connect to that exact address, so the URL saying `169.254.169.254` is
    // as good a statement of intent as it ever was.
    let resolves_here = self.egress.direct_to(&host);

    if !self.allowlist.is_empty() {
      let port = url.port_or_known_default().unwrap_or(443);
      // A hostname is matched by name first; its resolved addresses are also
      // given to the CIDR entries, so an all-IP allowlist still covers named
      // destinations.
      for pattern in &self.allowlist {
        match pattern {
          OutboundPattern::Host(h) if *h == host => return Ok(()),
          OutboundPattern::Suffix(s) => {
            if host.len() > s.len() + 1 && host.ends_with(s.as_str()) {
              let boundary = host.as_bytes()[host.len() - s.len() - 1];
              if boundary == b'.' {
                return Ok(());
              }
            }
          }
          OutboundPattern::Cidr(base, bits) => {
            if let Some(ip) = literal_ip
              && crate::auth::cidr_contains(*base, *bits, ip)
            {
              return Ok(());
            }
          }
          _ => {}
        }
      }
      let has_cidrs = self
        .allowlist
        .iter()
        .any(|p| matches!(p, OutboundPattern::Cidr(..)));
      if literal_ip.is_none() && has_cidrs && resolves_here {
        let addrs = tokio::net::lookup_host((bare, port))
          .await
          .map_err(|e| format!("cannot resolve {host}: {e}"))?;
        for addr in addrs {
          for pattern in &self.allowlist {
            if let OutboundPattern::Cidr(base, bits) = pattern
              && crate::auth::cidr_contains(*base, *bits, addr.ip())
            {
              return Ok(());
            }
          }
        }
      }
      return Err(format!(
        "destination {host} is not on the outbound allowlist (APERIO_OUTBOUND_ALLOWLIST)"
      ));
    }

    // Allowlist-less mode: only the private-address gate.
    if let Some(ip) = literal_ip {
      if is_internal(ip) {
        return Err(format!(
          "destination {host} is an internal address (refused by APERIO_OUTBOUND_BLOCK_PRIVATE)"
        ));
      }
      return Ok(());
    }
    // Through a proxy this name is resolved elsewhere and connected to from
    // there, so there is nothing here to judge. Allowed rather than refused,
    // because refusing every named destination would take the feature away
    // from the deployment that needs it most, and the startup log has already
    // said that this is what the setting covers on this server.
    if !resolves_here {
      return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    // Resolve and check every address the name maps to: a hostname resolving
    // to 127.0.0.1 or 169.254.169.254 is the classic bypass.
    let addrs = tokio::net::lookup_host((bare, port))
      .await
      .map_err(|e| format!("cannot resolve {host}: {e}"))?;
    let mut any = false;
    for addr in addrs {
      any = true;
      if is_internal(addr.ip()) {
        return Err(format!(
          "{host} resolves to the internal address {} (refused by APERIO_OUTBOUND_BLOCK_PRIVATE)",
          addr.ip()
        ));
      }
    }
    if !any {
      return Err(format!("{host} resolves to no address"));
    }
    Ok(())
  }
}

/// Rejects a destination the server must not be talked into calling, when the
/// URL was named by a *client* rather than by the operator.
///
/// **The SSRF boundary, in one place.** A tunnel-token holder is a lower-trust
/// credential than the operator running the server: an operator naming an
/// internal address is describing their own network, a client naming one is
/// aiming this server at it. `scaling.url` has been fenced this way since it
/// existed; a client-declared `jwt.jwks_url` was not, and reached the same
/// primitive (an arbitrary host and scheme, fetched from the server's
/// network) with nothing but the operator's optional outbound policy in the
/// way, which is off by default.
///
/// Deliberately independent of `OutboundPolicy`: that one is the operator's
/// own preference about their own callbacks and defaults to permissive. This
/// is not a preference, so relaxing it takes a named flag per feature.
pub(crate) async fn client_declared_destination_allowed(
  url: &url::Url,
  allow_insecure: bool,
  allow_private: bool,
  relax_hint: &str,
) -> Result<(), String> {
  match url.scheme() {
    "https" => {}
    "http" if allow_insecure => {}
    scheme => {
      return Err(format!(
        "scheme {scheme} is not allowed (https only unless {relax_hint}_ALLOW_HTTP=1)"
      ));
    }
  }
  let Some(host) = url.host_str() else {
    return Err("no host in the URL".to_string());
  };
  let port = url.port_or_known_default().unwrap_or(443);
  // Resolve and check every address the name maps to: a hostname that resolves
  // to 127.0.0.1 or 169.254.169.254 is the classic bypass.
  let addrs = tokio::net::lookup_host((host, port))
    .await
    .map_err(|e| format!("cannot resolve {host}: {e}"))?;
  let mut any = false;
  for addr in addrs {
    any = true;
    if !allow_private && is_internal(addr.ip()) {
      return Err(format!(
        "{host} resolves to the internal address {} (refused)",
        addr.ip()
      ));
    }
  }
  if !any {
    return Err(format!("{host} resolves to no address"));
  }
  Ok(())
}

/// True for addresses that live inside the deployment rather than on the
/// public internet: loopback, RFC 1918, link-local (including the cloud
/// metadata services), CGNAT, unique-local, unspecified, and IPv4-mapped
/// forms of all of those.
pub(crate) fn is_internal(ip: IpAddr) -> bool {
  match ip {
    IpAddr::V4(v4) => {
      v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_unspecified()
        // 100.64.0.0/10, carrier-grade NAT and the usual cloud metadata mesh.
        || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
    }
    IpAddr::V6(v6) => {
      v6.is_loopback()
        || v6.is_unspecified()
        // Unique local (fc00::/7) and link local (fe80::/10).
        || (v6.segments()[0] & 0xfe00) == 0xfc00
        || (v6.segments()[0] & 0xffc0) == 0xfe80
        // IPv4-mapped addresses must be judged by the address they carry.
        || v6.to_ipv4_mapped().is_some_and(|v4| is_internal(IpAddr::V4(v4)))
    }
  }
}

/// Reads a response body, stopping the moment it exceeds `limit`.
///
/// `None` when it did, or when what came back is not text. **The limit is
/// applied while reading, not after**, and that is the whole point: a length
/// checked on a body already in memory frees what it has just finished
/// allocating, which leaves how much that is to whatever answered. On the
/// visitor-auth path that would be once per request.
///
/// The declared length is the cheap refusal: a body that says how big it is
/// and is too big never costs a chunk, and never costs the wait for a body
/// that is promised and never sent.
///
/// Lives here because every caller is the server calling somebody else, which
/// is what this module is for, and because two copies of a bound like this
/// drift.
pub(crate) async fn read_bounded(mut res: reqwest::Response, limit: usize) -> Option<String> {
  if res.content_length().is_some_and(|n| n as usize > limit) {
    return None;
  }
  let mut buf: Vec<u8> = Vec::new();
  while let Ok(Some(chunk)) = res.chunk().await {
    if buf.len() + chunk.len() > limit {
      return None;
    }
    buf.extend_from_slice(&chunk);
  }
  String::from_utf8(buf).ok()
}

#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;
