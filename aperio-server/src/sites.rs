//! What the server does for a hostname, or a path under one, by itself (the
//! `sites` setting).
//!
//! A site carries an answer given without a client (`redirect`, `respond`),
//! the policy on proxied traffic (`timeout`, `headers`, `rate_limit`,
//! `canary`, `waf`), and what its hostname says when nothing serves it
//! (`fallback`, `pages`, `maintenance`). One matcher decides which sites a
//! request falls under, where seven sections used to each have their own.
//!
//! A request takes each setting from the most specific matching site that
//! writes it: an exact hostname before a pattern before `*`, then the longest
//! `path`, then the order they were written in. Three settings add up instead,
//! because for them "most specific wins" would be the wrong answer:
//!
//! - `headers` layer broadest first, so `*` sets what every hostname gets and
//!   a narrower site has the last word.
//! - `waf` rules of every matching site apply. A hostname that adds one rule
//!   of its own must not escape the scanner blocks `*` put up for everybody.
//! - `maintenance` windows of every matching site apply, for the same reason:
//!   the weekly window on `*` is still everybody's window.
//!
//! The list is compiled once, when the settings are written, and every
//! mistake is refused there naming the site, so a site that cannot work is
//! never stored.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use crate::headers::HeaderTransforms;
use crate::routing::{normalize_path_bind, path_matches_bind};

mod canary;
mod maintenance;
mod waf;

pub(crate) use canary::{CanaryRule, Side};
pub(crate) use maintenance::Window;
use waf::WafRule;

/// The largest page a site may carry, in bytes.
const MAX_PAGE_BYTES: usize = 512 * 1024;

/// A redirect: a site's answer, or its fallback.
#[derive(Clone, Debug)]
pub(crate) struct Redirect {
  pub(crate) url: String,
  pub(crate) permanent: bool,
  pub(crate) preserve_path: bool,
}

impl Redirect {
  fn compile(raw: &aperio_config::Redirect, at: &str) -> Result<Redirect, String> {
    let url = raw.url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
      return Err(format!(
        "{at}.url: `{url}` is not an absolute http(s) URL; a browser would read it as a path \
         on the same hostname"
      ));
    }
    Ok(Redirect {
      url,
      permanent: raw.permanent,
      preserve_path: raw.preserve_path,
    })
  }

  /// `301` when permanent, otherwise `302`.
  pub(crate) fn status(&self) -> StatusCode {
    if self.permanent {
      StatusCode::MOVED_PERMANENTLY
    } else {
      StatusCode::FOUND
    }
  }

  /// Where the visitor is sent, with their path and query appended when
  /// `preserve_path` asks for it.
  pub(crate) fn location(&self, path: &str, query: Option<&str>) -> String {
    if !self.preserve_path {
      return self.url.clone();
    }
    let mut location = format!("{}{}", self.url.trim_end_matches('/'), path);
    if let Some(q) = query {
      location.push('?');
      location.push_str(q);
    }
    location
  }

  fn respond(&self, path: &str, query: Option<&str>) -> Response {
    Response::builder()
      .status(self.status())
      .header("location", self.location(path, query))
      .body(Body::empty())
      .unwrap_or_default()
  }
}

/// A fixed response.
#[derive(Clone, Debug)]
pub(crate) struct Respond {
  pub(crate) status: u16,
  pub(crate) content_type: String,
  pub(crate) body: String,
}

/// What a site that answers by itself answers with.
#[derive(Clone, Debug)]
pub(crate) enum Answer {
  Redirect(Redirect),
  Respond(Respond),
}

impl Answer {
  /// The status the answer carries.
  pub(crate) fn status(&self) -> u16 {
    match self {
      Answer::Redirect(r) => r.status().as_u16(),
      Answer::Respond(r) => r.status,
    }
  }

  fn respond(&self, path: &str, query: Option<&str>) -> Response {
    match self {
      Answer::Redirect(r) => r.respond(path, query),
      Answer::Respond(r) => Response::builder()
        .status(StatusCode::from_u16(r.status).unwrap_or(StatusCode::OK))
        .header("content-type", r.content_type.clone())
        .body(Body::from(r.body.clone()))
        .unwrap_or_default(),
    }
  }
}

/// A site's `rate_limit`: one token bucket every visitor shares.
#[derive(Clone, Debug)]
pub(crate) struct RateLimit {
  pub(crate) rps: f64,
  pub(crate) burst: f64,
  /// Uppercased methods the limit counts; None = every method.
  pub(crate) methods: Option<Vec<String>>,
  /// The bucket's name in the rate map: the site's position and match, so two
  /// sites never share one and an unchanged site keeps its bucket on a change
  /// elsewhere.
  pub(crate) key: String,
}

impl RateLimit {
  /// True when the limit counts this verb. An absent filter counts every
  /// verb, and an absent verb (the explainer, which reasons about a route
  /// rather than a request) is not filtered out.
  pub(crate) fn counts(&self, method: Option<&str>) -> bool {
    match (&self.methods, method) {
      (None, _) | (Some(_), None) => true,
      (Some(list), Some(m)) => list.iter().any(|allowed| allowed.eq_ignore_ascii_case(m)),
    }
  }
}

/// One compiled site.
#[derive(Clone, Debug, Default)]
pub(crate) struct Site {
  /// Normalized hostname pattern: exact, `*.suffix`, a partial label, or `*`.
  pub(crate) hostname: String,
  /// Normalized path prefix, or None for the whole hostname.
  pub(crate) path: Option<String>,
  pub(crate) answer: Option<Answer>,
  /// Seconds to wait for the serving client's answer.
  pub(crate) timeout: Option<u64>,
  headers: Option<HeaderTransforms>,
  pub(crate) rate_limit: Option<RateLimit>,
  pub(crate) canary: Option<CanaryRule>,
  waf: Vec<WafRule>,
  pub(crate) fallback: Option<Redirect>,
  page_504: Option<String>,
  page_503: Option<String>,
  maintenance: Vec<Window>,
  /// How specific the match is, higher first: the hostname's kind, then the
  /// path's length.
  rank: (u32, usize),
}

impl Site {
  fn host_matches(&self, host: Option<&str>) -> bool {
    self.hostname == "*"
      || host.is_some_and(|h| crate::store::orgs::pattern_matches_host(&self.hostname, h))
  }

  fn matches(&self, host: Option<&str>, path: &str) -> bool {
    self.host_matches(host)
      && self
        .path
        .as_deref()
        .is_none_or(|p| path_matches_bind(path, p))
  }

  /// `hostname` or `hostname/path`, for a message.
  #[cfg(test)]
  pub(crate) fn label(&self) -> String {
    format!("{}{}", self.hostname, self.path.as_deref().unwrap_or(""))
  }
}

/// How specific a hostname pattern is: `*` matches everything and ranks last,
/// a subdomain wildcard next, a partial label (`*-pi.example.com`, one level)
/// above it, an exact name first. A longer suffix is the narrower pattern.
fn host_rank(pattern: &str) -> u32 {
  let len = pattern.len() as u32;
  if pattern == "*" {
    0
  } else if pattern.starts_with("*.") {
    1_000 + len
  } else if pattern.contains('*') {
    2_000 + len
  } else {
    3_000 + len
  }
}

/// The compiled sites, most specific first.
#[derive(Clone, Default)]
pub(crate) struct Sites {
  sites: Arc<Vec<Site>>,
}

impl Sites {
  /// Compiles the `sites` setting. Refuses the first site that cannot work,
  /// naming it, so it is never stored.
  pub(crate) fn compile(raw: &[aperio_config::Site]) -> Result<Sites, String> {
    let mut sites = Vec::with_capacity(raw.len());
    for (i, site) in raw.iter().enumerate() {
      sites.push(compile_site(i, site)?);
    }
    // Stable, so two sites of equal rank keep the order they were written in.
    sites.sort_by_key(|s| std::cmp::Reverse(s.rank));
    Ok(Sites {
      sites: Arc::new(sites),
    })
  }

  /// True when there are no sites (the fast path).
  pub(crate) fn is_empty(&self) -> bool {
    self.sites.is_empty()
  }

  /// Every site, most specific first.
  pub(crate) fn all(&self) -> &[Site] {
    &self.sites
  }

  /// The sites a request falls under, most specific first.
  fn matching<'a, 'q>(
    &'a self,
    host: Option<&'q str>,
    path: &'q str,
  ) -> impl Iterator<Item = &'a Site> + use<'a, 'q> {
    self.sites.iter().filter(move |s| s.matches(host, path))
  }

  /// The sites that cover a whole hostname (no `path`) and match it, most
  /// specific first: where a fallback, pages and maintenance come from.
  fn hostwide<'a, 'q>(
    &'a self,
    host: Option<&'q str>,
  ) -> impl Iterator<Item = &'a Site> + use<'a, 'q> {
    self
      .sites
      .iter()
      .filter(move |s| s.path.is_none() && s.host_matches(host))
  }

  /// The site that answers this request by itself, if one does.
  pub(crate) fn answering(&self, host: Option<&str>, path: &str) -> Option<&Site> {
    self.matching(host, path).find(|s| s.answer.is_some())
  }

  /// The answer for this request, if a site gives one without a client.
  pub(crate) fn answer(
    &self,
    host: Option<&str>,
    path: &str,
    query: Option<&str>,
  ) -> Option<Response> {
    let site = self.answering(host, path)?;
    site.answer.as_ref().map(|a| a.respond(path, query))
  }

  /// Seconds to wait for the serving client's answer on this request.
  pub(crate) fn timeout_for(&self, host: Option<&str>, path: &str) -> Option<u64> {
    self.matching(host, path).find_map(|s| s.timeout)
  }

  /// The canary split for this request.
  pub(crate) fn canary_for(&self, host: Option<&str>, path: &str) -> Option<&CanaryRule> {
    self.matching(host, path).find_map(|s| s.canary.as_ref())
  }

  /// The rate limit that counts this request, if any. `method: None` asks
  /// about the route rather than one request.
  pub(crate) fn rate_limit_for(
    &self,
    host: Option<&str>,
    path: &str,
    method: Option<&str>,
  ) -> Option<&RateLimit> {
    self
      .matching(host, path)
      .filter_map(|s| s.rate_limit.as_ref())
      .find(|rl| rl.counts(method))
  }

  /// Applies every matching site's request header edits, broadest first.
  pub(crate) fn request_headers(
    &self,
    host: Option<&str>,
    path: &str,
    headers: Vec<(String, String)>,
  ) -> Vec<(String, String)> {
    self.layered(host, path, headers, |t| &t.request)
  }

  /// Applies every matching site's response header edits, broadest first.
  pub(crate) fn response_headers(
    &self,
    host: Option<&str>,
    path: &str,
    headers: Vec<(String, String)>,
  ) -> Vec<(String, String)> {
    self.layered(host, path, headers, |t| &t.response)
  }

  fn layered(
    &self,
    host: Option<&str>,
    path: &str,
    mut headers: Vec<(String, String)>,
    direction: fn(&HeaderTransforms) -> &crate::headers::HeaderTransform,
  ) -> Vec<(String, String)> {
    if self.sites.is_empty() {
      return headers;
    }
    let edits: Vec<&HeaderTransforms> = self
      .matching(host, path)
      .filter_map(|s| s.headers.as_ref())
      .collect();
    for t in edits.into_iter().rev() {
      headers = direction(t).apply(headers);
    }
    headers
  }

  /// True when some site has a firewall rule (the fast path).
  pub(crate) fn has_waf(&self) -> bool {
    self.sites.iter().any(|s| !s.waf.is_empty())
  }

  /// The first deny rule of any matching site this request trips, by
  /// description. Answered `403`.
  pub(crate) fn waf_deny(
    &self,
    host: Option<&str>,
    method: &str,
    path: &str,
    headers: &HeaderMap,
  ) -> Option<&str> {
    self
      .matching(host, path)
      .flat_map(|s| s.waf.iter())
      .find(|r| r.denies(method, path, headers))
      .map(|r| r.desc.as_str())
  }

  /// The first size rule of any matching site a body of `body_len` exceeds,
  /// by description. Answered `413`.
  pub(crate) fn waf_body(
    &self,
    host: Option<&str>,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body_len: usize,
  ) -> Option<&str> {
    self
      .matching(host, path)
      .flat_map(|s| s.waf.iter())
      .find(|r| r.refuses_body(method, path, headers, body_len))
      .map(|r| r.desc.as_str())
  }

  /// Where visitors of this hostname go when nothing serves it.
  pub(crate) fn fallback_for(&self, host: Option<&str>) -> Option<&Redirect> {
    self.hostwide(host).find_map(|s| s.fallback.as_ref())
  }

  /// This hostname's `504` page, or the broadest one set.
  pub(crate) fn page_504(&self, host: Option<&str>) -> Option<&str> {
    self.hostwide(host).find_map(|s| s.page_504.as_deref())
  }

  /// This hostname's `503` page, or the broadest one set.
  pub(crate) fn page_503(&self, host: Option<&str>) -> Option<&str> {
    self.hostwide(host).find_map(|s| s.page_503.as_deref())
  }

  /// A maintenance window running now for this hostname, with the unix
  /// second it ends: the most specific site's first, then the broader ones.
  pub(crate) fn maintenance_for(
    &self,
    host: Option<&str>,
    now_secs: u64,
  ) -> Option<(&Window, u64)> {
    self
      .hostwide(host)
      .flat_map(|s| s.maintenance.iter())
      .find_map(|w| w.active_until(now_secs).map(|until| (w, until)))
  }

  /// True when some site has a maintenance window (the fast path).
  pub(crate) fn has_maintenance(&self) -> bool {
    self.sites.iter().any(|s| !s.maintenance.is_empty())
  }
}

fn compile_site(i: usize, raw: &aperio_config::Site) -> Result<Site, String> {
  let shown = format!(
    "{}{}",
    raw.hostname.trim(),
    raw.path.as_deref().map(str::trim).unwrap_or("")
  );
  let err = |what: String| format!("sites[{i}] ({shown}): {what}");

  let hostname =
    crate::store::orgs::normalize_org_hostname_pattern(&raw.hostname).ok_or_else(|| {
      err(format!(
        "`hostname: {}` is not a hostname, a pattern like `*.example.com`, or `*`",
        raw.hostname
      ))
    })?;
  let path = match raw.path.as_deref().map(str::trim) {
    None | Some("") | Some("/") => None,
    Some(p) => Some(normalize_path_bind(p).ok_or_else(|| {
      err(format!(
        "`path: {p}` is not a path prefix (letters, digits, `-`, `_`, `.`, `~` between slashes)"
      ))
    })?),
  };

  if raw.redirect.is_some() && raw.respond.is_some() {
    return Err(err(
      "`redirect` and `respond` are two answers; a site gives one".to_string(),
    ));
  }
  let answer = match (&raw.redirect, &raw.respond) {
    (Some(r), _) => Some(Answer::Redirect(
      Redirect::compile(r, "redirect").map_err(err)?,
    )),
    (None, Some(r)) => Some(Answer::Respond(Respond {
      status: r.status.unwrap_or(200),
      content_type: r
        .content_type
        .clone()
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "text/html; charset=utf-8".to_string()),
      body: r.body.clone().unwrap_or_default(),
    })),
    (None, None) => None,
  };
  if let Some(Answer::Respond(r)) = &answer
    && StatusCode::from_u16(r.status).is_err()
  {
    return Err(err(format!(
      "`respond.status: {}` is not an HTTP status",
      r.status
    )));
  }

  // A site that answers by itself never reaches a client, so policy on the
  // traffic it would have proxied can mean nothing. Saying so beats storing
  // a setting that silently does not apply.
  if answer.is_some() {
    let proxied: Vec<&str> = [
      ("timeout", raw.timeout.is_some()),
      ("headers", raw.headers.is_some()),
      ("rate_limit", raw.rate_limit.is_some()),
      ("canary", raw.canary.is_some()),
      ("waf", !raw.waf.is_empty()),
      ("fallback", raw.fallback.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    if !proxied.is_empty() {
      return Err(err(format!(
        "`{}` apply to traffic a client serves, and this site answers by itself with `{}`; \
         put them on a second site with the same hostname and path",
        proxied.join("`, `"),
        if raw.redirect.is_some() {
          "redirect"
        } else {
          "respond"
        }
      )));
    }
  }

  // A fallback, the pages and maintenance are about the hostname: what it
  // says when nothing serves it, or while it is down on purpose. A path
  // under it has no answer of its own to give in either case.
  if path.is_some() {
    let hostwide: Vec<&str> = [
      ("fallback", raw.fallback.is_some()),
      ("pages", raw.pages.is_some()),
      ("maintenance", !raw.maintenance.is_empty()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    if !hostwide.is_empty() {
      return Err(err(format!(
        "`{}` are about the whole hostname; put them on a site without `path`",
        hostwide.join("`, `")
      )));
    }
  }

  let timeout = match raw.timeout {
    None => None,
    Some(t) if t.as_secs() == 0 => {
      return Err(err(
        "`timeout` must be at least 1s; leave it out to use `gateway.response_timeout`".to_string(),
      ));
    }
    Some(t) => Some(t.as_secs()),
  };

  let headers = raw.headers.as_ref().map(HeaderTransforms::compile);

  let rate_limit = match &raw.rate_limit {
    None => None,
    Some(rl) => {
      if !(rl.rps.is_finite() && rl.rps > 0.0) {
        return Err(err(
          "`rate_limit.rps` must be a positive number".to_string(),
        ));
      }
      if let Some(b) = rl.burst
        && !(b.is_finite() && b > 0.0)
      {
        return Err(err(
          "`rate_limit.burst` must be a positive number; leave it out for one second's worth"
            .to_string(),
        ));
      }
      let methods = match &rl.methods {
        None => None,
        Some(list) if list.is_empty() => {
          return Err(err(
            "`rate_limit.methods` is empty, which would count nothing; leave it out to count \
             every method"
              .to_string(),
          ));
        }
        Some(list) => Some(
          list
            .iter()
            .map(|m| m.trim().to_ascii_uppercase())
            .collect::<Vec<_>>(),
        ),
      };
      Some(RateLimit {
        rps: rl.rps,
        // Floored at one token: a burst below one can never pay for a
        // request, and the site would answer 429 to everything.
        burst: rl.burst.unwrap_or(rl.rps).max(1.0),
        key: format!(
          "site#{i}:{hostname}:{}:{}",
          path.as_deref().unwrap_or("*"),
          methods
            .as_ref()
            .map(|m| m.join(","))
            .unwrap_or_else(|| "*".to_string())
        ),
        methods,
      })
    }
  };

  let canary = match &raw.canary {
    None => None,
    Some(c) => {
      if c.service.trim().is_empty() {
        return Err(err(
          "`canary.service` names the client service running the new version; empty, nothing \
           matches it and the split silently does nothing"
            .to_string(),
        ));
      }
      let weight = c.weight.unwrap_or(0);
      if weight > 100 {
        return Err(err(format!(
          "`canary.weight` is a percentage, so {weight} is out of range. A weight past 100 \
           would quietly mean 'everybody', which is not what somebody who typed it meant"
        )));
      }
      Some(CanaryRule {
        service: c.service.trim().to_string(),
        weight,
        header: c.header.clone().filter(|h| !h.trim().is_empty()),
        value: c.value.clone(),
      })
    }
  };

  let waf = raw
    .waf
    .iter()
    .enumerate()
    .map(|(j, rule)| {
      let mut compiled = WafRule::compile(rule, &format!("waf[{j}]")).map_err(err)?;
      // The reason a refused visitor's request is logged under, so it says
      // which site's rule fired.
      compiled.desc = format!("{shown} {}", compiled.desc);
      Ok(compiled)
    })
    .collect::<Result<Vec<_>, String>>()?;

  let fallback = raw
    .fallback
    .as_ref()
    .map(|f| Redirect::compile(f, "fallback").map_err(err))
    .transpose()?;

  let page = |html: &Option<String>, name: &str| -> Result<Option<String>, String> {
    match html {
      Some(h) if h.len() > MAX_PAGE_BYTES => Err(err(format!("pages.{name}: exceeds 512KB"))),
      Some(h) if !h.trim().is_empty() => Ok(Some(h.clone())),
      _ => Ok(None),
    }
  };
  let (page_504, page_503) = match &raw.pages {
    None => (None, None),
    Some(p) => (
      page(&p.unavailable, "unavailable")?,
      page(&p.maintenance, "maintenance")?,
    ),
  };

  let maintenance = raw
    .maintenance
    .iter()
    .enumerate()
    .map(|(j, w)| Window::compile(w, &format!("maintenance[{j}]")).map_err(err))
    .collect::<Result<Vec<_>, _>>()?;

  let site = Site {
    rank: (host_rank(&hostname), path.as_deref().map_or(0, str::len)),
    hostname,
    path,
    answer,
    timeout,
    headers,
    rate_limit,
    canary,
    waf,
    fallback,
    page_504,
    page_503,
    maintenance,
  };
  let sets_something = site.answer.is_some()
    || site.timeout.is_some()
    || site.headers.is_some()
    || site.rate_limit.is_some()
    || site.canary.is_some()
    || !site.waf.is_empty()
    || site.fallback.is_some()
    || site.page_504.is_some()
    || site.page_503.is_some()
    || !site.maintenance.is_empty();
  if !sets_something {
    return Err(err(
      "sets nothing; a site needs an answer (`redirect`, `respond`), policy (`timeout`, \
       `headers`, `rate_limit`, `canary`, `waf`), or what its hostname says (`fallback`, \
       `pages`, `maintenance`)"
        .to_string(),
    ));
  }
  Ok(site)
}

#[cfg(test)]
#[path = "sites_tests.rs"]
mod tests;
