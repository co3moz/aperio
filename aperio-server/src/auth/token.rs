//! Tokens on the way in: pulling one out of a request, comparing it without
//! leaking its length, and deciding whether a tunnel may connect with it.

use axum::http::HeaderMap;
use std::net::IpAddr;

use super::*;
use crate::state::AppState;

/// Extracts a Bearer token or `x-auth-token` value from request headers.
pub(crate) fn extract_token(headers: &HeaderMap) -> Option<String> {
  if let Some(auth_header) = headers.get("authorization")
    && let Ok(auth_str) = auth_header.to_str()
    && let Some(stripped) = auth_str.strip_prefix("Bearer ")
  {
    return Some(stripped.to_string());
  }
  if let Some(x_token) = headers.get("x-auth-token")
    && let Ok(x_token_str) = x_token.to_str()
  {
    return Some(x_token_str.to_string());
  }
  None
}

/// Helper function to extract Bearer token or `x-auth-token` from header values
/// and verify if it matches the configured server security token.
#[cfg(test)]
pub(crate) fn extract_and_verify_token(headers: &HeaderMap, server_token: &str) -> bool {
  match extract_token(headers) {
    Some(tok) => constant_time_eq_str(&tok, server_token),
    None => false,
  }
}

/// Resolves the permissions for a presented tunnel token: the master token
/// grants unrestricted access; otherwise the dynamic token store is consulted
/// (rejecting unknown and expired tokens).
pub(crate) async fn authorize_tunnel_token(
  state: &AppState,
  headers: &HeaderMap,
  client_ip: IpAddr,
) -> Option<ClientPerms> {
  let presented = extract_token(headers)?;
  if constant_time_eq_str(&presented, &state.config().token) {
    return Some(ClientPerms::master());
  }
  // Verify against the store, then release the lock before emitting events so
  // we never hold the token store across an await on other state locks.
  let (perms, canary, org_id) = {
    let store = state.token_store.lock().await;
    let token = store.verify(&presented)?;
    // Dynamic tokens can be restricted to source IPs/CIDRs.
    if !ip_allowed(client_ip, &token.allowed_ips) {
      warn!(
        "Token '{}' rejected: source IP {} not in allowed list {:?}",
        token.name, client_ip, token.allowed_ips
      );
      return None;
    }
    (
      ClientPerms {
        master: false,
        hostnames: token.hostnames.clone(),
        paths: token.paths.clone(),
        token_name: Some(token.name.clone()),
        token_id: Some(token.id.clone()),
        allow_public: token.allow_public,
        allow_bind: token.allow_bind,
        allow_otel: token.allow_otel,
        topics: token.topics.clone(),
        org_id: token.org_id.clone(),
        max_connections: token.max_connections,
        // Filled in below: the org store must not be locked while the token
        // store lock is held.
        org_hostnames: Vec::new(),
      },
      token.canary,
      token.org_id.clone(),
    )
  };
  // Resolve the organization's hostname allowlist once, so every later bind
  // check on this connection is a pure in-memory comparison.
  let mut perms = perms;
  perms.org_hostnames = state.org_store.lock().await.hostnames_of(org_id.as_deref());
  let perms = perms;

  let token_id = perms.token_id.clone().unwrap_or_default();
  let token_name = perms.token_name.clone().unwrap_or_default();

  // A canary/decoy token is never meant to be used: any authentication with it
  // is a breach signal, so it always trips an alert.
  if canary {
    warn!(
      "CANARY TRIPPED: token '{}' authenticated from {}",
      token_name, client_ip
    );
    state
      .audit_in(
        "canary_tripped",
        &token_name,
        &client_ip.to_string(),
        org_id.clone(),
        &format!("token={} id={} ip={}", token_name, token_id, client_ip),
      )
      .await;
    state
      .emit_event_in(
        "canary_tripped",
        serde_json::json!({"token": token_name, "token_id": token_id, "ip": client_ip.to_string()}),
        org_id.clone(),
      )
      .await;
  }

  // Alert when a token connects from a source IP not seen before this run. The
  // very first address a token is seen from establishes the baseline silently.
  // Cap the tracked source-IP set per token: once a token has connected from
  // this many distinct addresses the new-IP signal is meaningless, and the set
  // must not grow without bound (spoofed XFF, NAT/mobile churn).
  const TOKEN_SEEN_IPS_CAP: usize = 256;
  let is_new_ip = {
    let mut seen = state.token_seen_ips.lock().await;
    let ips = seen.entry(token_id.clone()).or_default();
    if ips.is_empty() {
      ips.insert(client_ip);
      false
    } else if ips.len() >= TOKEN_SEEN_IPS_CAP {
      // At the cap: stop tracking and stop alerting.
      false
    } else {
      ips.insert(client_ip)
    }
  };
  if is_new_ip {
    warn!(
      "Token '{}' connected from a new source IP {}",
      token_name, client_ip
    );
    state
      .audit_in(
        "token_new_ip",
        &token_name,
        &client_ip.to_string(),
        org_id.clone(),
        &format!("token={} id={} ip={}", token_name, token_id, client_ip),
      )
      .await;
    state
      .emit_event_in(
        "token_new_ip",
        serde_json::json!({"token": token_name, "token_id": token_id, "ip": client_ip.to_string()}),
        org_id,
      )
      .await;
  }

  Some(perms)
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod tests;
