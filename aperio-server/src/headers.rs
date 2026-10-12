//! Header edits on proxied traffic, as a site's `headers` writes them.
//!
//! Mirrors the client's per-service `headers:`: `request` edits what tunnel
//! clients (and thus backends) receive, `response` edits what visitors
//! receive. Hop-by-hop and tunnel-critical headers stay managed by Aperio
//! regardless (they are stripped after these edits run). WebSocket upgrades
//! pass through untouched.

use aperio_config::{HeaderDirectives, HeaderRules};
use std::collections::HashSet;

/// Compiled form of one direction's rules: removals match case-insensitively,
/// additions replace any existing header of the same name.
#[derive(Default, Clone, Debug)]
pub(crate) struct HeaderTransform {
  /// Headers to set (original-case name, value).
  add: Vec<(String, String)>,
  /// Lowercased names to strip (includes the names being re-added).
  remove: HashSet<String>,
}

impl HeaderTransform {
  /// Compiles the directives for one direction (None = no edits).
  pub(crate) fn compile(directives: Option<&HeaderDirectives>) -> Self {
    let Some(d) = directives else {
      return HeaderTransform::default();
    };
    let mut remove: HashSet<String> = d.remove.iter().map(|n| n.to_ascii_lowercase()).collect();
    let add: Vec<(String, String)> = d.add.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (name, _) in &add {
      remove.insert(name.to_ascii_lowercase());
    }
    HeaderTransform { add, remove }
  }

  /// True when there is nothing to do (the fast path when unconfigured).
  pub(crate) fn is_empty(&self) -> bool {
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

/// The compiled pair carried in the server configuration.
#[derive(Default, Clone, Debug)]
pub(crate) struct HeaderTransforms {
  /// Applied to forwarded requests before they enter the tunnel.
  pub(crate) request: HeaderTransform,
  /// Applied to responses before they return to the visitor (and before
  /// they are cached or captured for the inspector, so all views agree).
  pub(crate) response: HeaderTransform,
}

impl HeaderTransforms {
  /// Compiles a site's `headers`.
  pub(crate) fn compile(rules: &HeaderRules) -> Self {
    HeaderTransforms {
      request: HeaderTransform::compile(rules.request.as_ref()),
      response: HeaderTransform::compile(rules.response.as_ref()),
    }
  }
}

#[cfg(test)]
#[path = "headers_tests.rs"]
mod tests;
