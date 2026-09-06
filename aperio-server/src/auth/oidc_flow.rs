//! The OIDC flow, both legs: where a provider is told to come back to, and
//! what the callback is allowed to turn into a session.

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Derives the OIDC redirect URI for this deployment: the explicit override
/// wins, otherwise it is built from the request Host header (and
/// X-Forwarded-Proto when running behind a trusted proxy).
///
/// Deriving it is a convenience with a sharp edge, which is why the override
/// is recommended and warned about at startup when it is missing: the `Host`
/// of the request that *starts* the login decides where the provider sends
/// the authorization code back to. A visitor lured to a hostname that resolves
/// to this server would have their code sent to that hostname. Redeeming it
/// still needs the client secret, and the provider's own registered-callback
/// list is the other gate, but neither of those is ours.
///
/// Only the start of the flow can decide it. The callback reads the URL back
/// out of the login's state entry instead of deriving it again.
fn oidc_redirect_uri(state: &AppState, headers: &HeaderMap) -> Option<String> {
  let rt = state.oidc.as_ref()?;
  if let Some(ref fixed) = rt.redirect_url_override {
    return Some(fixed.clone());
  }
  let host = headers.get("host").and_then(|v| v.to_str().ok())?;
  let proto = if state.config().trust_proxy {
    headers
      .get("x-forwarded-proto")
      .and_then(|v| v.to_str().ok())
      .unwrap_or("http")
  } else {
    "http"
  };
  Some(format!("{}://{}/aperio/oidc/callback", proto, host))
}

/// Starts the OIDC authorization code flow: stores a CSRF state token and
/// redirects the browser to the identity provider.
/// Resolves a per-organization OIDC runtime, building and caching it from the
/// org's stored config on first use. Returns None when the org has no OIDC
/// override or its config fails to build (logged, never fatal).
pub(crate) async fn resolve_org_oidc(
  state: &AppState,
  org_id: &str,
) -> Option<crate::oidc::OidcRuntime> {
  if let Some(rt) = state.org_oidc.lock().await.get(org_id) {
    return Some(rt.clone());
  }
  let cfg = state.org_store.lock().await.find(org_id)?.oidc.clone()?;
  // The tenant's directory hands out roles inside the tenant and nothing
  // beyond: every grant here names this organization, whatever the table
  // says, and `*` cannot be written into it.
  let here = crate::store::grants::GrantOrg::Child(org_id.to_string());
  let group_grants = cfg
    .group_grants
    .iter()
    .filter_map(|entry| {
      let (group, role) = entry.split_once('=')?;
      let role = Role::parse(role)?;
      Some((
        group.trim().to_string(),
        crate::store::grants::Grant::new(here.clone(), role),
      ))
    })
    .collect();
  let grants = crate::oidc::OidcGrantPolicy {
    default_grants: cfg
      .default_role
      .map(|role| vec![crate::store::grants::Grant::new(here.clone(), role)])
      .unwrap_or_default(),
    groups_claim: String::new(),
    group_grants,
  };
  match crate::oidc::build_runtime(
    &state.config().outbound_policy,
    &cfg.issuer,
    &cfg.client_id,
    &cfg.client_secret,
    cfg.allowed_emails.clone(),
    "openid email profile".to_string(),
    None,
    grants,
  )
  .await
  {
    Ok(rt) => {
      state
        .org_oidc
        .lock()
        .await
        .insert(org_id.to_string(), rt.clone());
      Some(rt)
    }
    Err(e) => {
      warn!("Per-org OIDC for {} failed to build: {}", org_id, e);
      None
    }
  }
}

pub(crate) async fn oidc_login_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  // A per-org login (`?org=<id>`) authenticates against that org's OIDC and
  // binds the session to it; otherwise the global OIDC runtime is used.
  let org = query
    .get("org")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty() && s != "master");
  let (rt, bound_org) = match &org {
    Some(id) => match resolve_org_oidc(&state, id).await {
      Some(rt) => (rt, Some(id.clone())),
      None => {
        return (
          StatusCode::NOT_FOUND,
          "OIDC is not configured for this organization",
        )
          .into_response();
      }
    },
    None => match state.oidc.clone() {
      Some(rt) => (rt, None),
      None => return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response(),
    },
  };
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  // Priced as a credential attempt: this begins a login.
  if !state
    .check_rate_limit_cost(caller_ip, crate::state::RateCost::Guessable)
    .await
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let redirect_after = query
    .get("redirect")
    .map(|r| safe_redirect_path(r).to_string())
    .unwrap_or_else(|| "/".to_string());
  let Some(redirect_uri) = oidc_redirect_uri(&state, &headers) else {
    return (StatusCode::BAD_REQUEST, "Missing Host header").into_response();
  };

  // Register the CSRF state (10 min TTL, opportunistic GC).
  let state_token = uuid::Uuid::new_v4().to_string();
  {
    let mut states = state.oidc_states.lock().await;
    let now = Instant::now();
    states.retain(|_, (_, _, _, exp)| *exp > now);
    states.insert(
      state_token.clone(),
      (
        redirect_after,
        bound_org,
        redirect_uri.clone(),
        now + Duration::from_secs(600),
      ),
    );
  }

  let authorize = url::Url::parse_with_params(
    &rt.authorization_endpoint,
    &[
      ("response_type", "code"),
      ("client_id", rt.client_id.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("scope", rt.scopes.as_str()),
      ("state", state_token.as_str()),
    ],
  );
  match authorize {
    Ok(u) => Response::builder()
      .status(StatusCode::FOUND)
      .header("Location", u.to_string())
      .body(Body::empty())
      .unwrap(),
    Err(e) => {
      error!("Failed to build OIDC authorize URL: {}", e);
      (
        StatusCode::INTERNAL_SERVER_ERROR,
        "OIDC configuration error",
      )
        .into_response()
    }
  }
}

/// OIDC callback: validates the CSRF state, exchanges the code for tokens,
/// fetches the userinfo email, checks it against the allowlist, and creates
/// a session identical to the password login.
pub(crate) async fn oidc_callback_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  // Priced as a credential attempt: the callback carries the code that becomes a session.
  if !state
    .check_rate_limit_cost(caller_ip, crate::state::RateCost::Guessable)
    .await
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let (Some(code), Some(state_param)) = (query.get("code"), query.get("state")) else {
    return (StatusCode::BAD_REQUEST, "Missing code/state parameter").into_response();
  };

  // Validate and consume the CSRF state, recovering the bound org (if any).
  let (redirect_after, bound_org, redirect_uri) = {
    let mut states = state.oidc_states.lock().await;
    match states.remove(state_param) {
      Some((redirect, org, callback, exp)) if exp > Instant::now() => (redirect, org, callback),
      _ => {
        return (StatusCode::BAD_REQUEST, "Invalid or expired OIDC state").into_response();
      }
    }
  };
  // Use the same runtime the login flow selected: the org's, or the global one.
  let rt = match &bound_org {
    Some(id) => match resolve_org_oidc(&state, id).await {
      Some(rt) => rt,
      None => return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response(),
    },
    None => match state.oidc.clone() {
      Some(rt) => rt,
      None => return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response(),
    },
  };
  // `redirect_uri` comes from the state entry, not from this request: it must
  // match the one the authorization request carried, and this request's `Host`
  // is not something to settle that with.

  // Both endpoints come out of the issuer's own discovery document, which is
  // to say they are chosen by something outside this deployment, so they go
  // through the outbound fence before the server calls them. Checked here
  // rather than once at build time because a runtime is cached and reused,
  // and a fence tightened afterwards should cover it.
  for endpoint in [&rt.token_endpoint, &rt.userinfo_endpoint] {
    if let Err(why) = state.config().outbound_policy.check(endpoint).await {
      error!("Refusing to call the OIDC endpoint {endpoint}: {why}");
      return (StatusCode::BAD_GATEWAY, "OIDC endpoint refused").into_response();
    }
  }
  // Exchange the authorization code for an access token.
  let http = match crate::outbound::client_builder()
    .timeout(Duration::from_secs(15))
    // Not followed, for the reason the check above exists: an endpoint that
    // passes the fence must not be able to answer with a `Location` pointing
    // behind it, and this is the request that carries the client secret.
    .redirect(reqwest::redirect::Policy::none())
    .build()
  {
    Ok(c) => c,
    Err(e) => {
      // Falling back to a default client would drop the 15s timeout, letting
      // the token/userinfo calls hang indefinitely, fail cleanly instead.
      error!("Failed to build the OIDC HTTP client: {}", e);
      return (StatusCode::INTERNAL_SERVER_ERROR, "OIDC client init failed").into_response();
    }
  };
  let token_res = http
    .post(&rt.token_endpoint)
    .form(&[
      ("grant_type", "authorization_code"),
      ("code", code.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("client_id", rt.client_id.as_str()),
      ("client_secret", rt.client_secret.as_str()),
    ])
    .send()
    .await;
  #[derive(Deserialize)]
  struct TokenResponse {
    access_token: String,
    /// The ID token, when the provider sends one: read for the groups claim
    /// when userinfo does not carry it. It came from the token endpoint over
    /// TLS, the same trust the userinfo answer gets, so its payload is read
    /// without a signature check, and nothing in it decides who this is.
    #[serde(default)]
    id_token: Option<String>,
  }
  let (access_token, id_token) = match token_res {
    // Bounded while it is read, like every other answer the server takes from
    // somewhere it does not run: an endpoint that decides how much memory a
    // login costs is an endpoint that can end the process.
    Ok(res) if res.status().is_success() => {
      match crate::outbound::read_bounded(res, OIDC_MAX_ANSWER_BYTES)
        .await
        .and_then(|body| serde_json::from_str::<TokenResponse>(&body).ok())
      {
        Some(t) => (t.access_token, t.id_token),
        None => {
          error!("OIDC token response was unreadable, oversized, or not a token response");
          return (StatusCode::BAD_GATEWAY, "OIDC token exchange failed").into_response();
        }
      }
    }
    Ok(res) => {
      warn!("OIDC token endpoint returned {}", res.status());
      return (StatusCode::UNAUTHORIZED, "OIDC token exchange rejected").into_response();
    }
    Err(e) => {
      error!("OIDC token exchange failed: {}", e);
      return (StatusCode::BAD_GATEWAY, "OIDC token exchange failed").into_response();
    }
  };

  // Fetch the verified identity from the issuer (trusted via TLS). Read as
  // a document rather than a fixed shape, because the groups claim is named
  // by configuration.
  let userinfo = http
    .get(&rt.userinfo_endpoint)
    .bearer_auth(&access_token)
    .send()
    .await;
  let info: serde_json::Value = match userinfo {
    Ok(res) if res.status().is_success() => {
      match crate::outbound::read_bounded(res, OIDC_MAX_ANSWER_BYTES)
        .await
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .filter(|v| v.is_object())
      {
        Some(v) => v,
        None => {
          error!("OIDC userinfo was unreadable, oversized, or not a userinfo document");
          return (StatusCode::BAD_GATEWAY, "OIDC userinfo failed").into_response();
        }
      }
    }
    _ => {
      return (StatusCode::BAD_GATEWAY, "OIDC userinfo failed").into_response();
    }
  };
  let email = info
    .get("email")
    .and_then(|v| v.as_str())
    .unwrap_or_default()
    .to_string();
  let email_verified = info.get("email_verified").and_then(|v| v.as_bool());
  // The directory's groups: from userinfo, else from the ID token's payload.
  let groups = claim_values(&info, rt.grants.claim()).unwrap_or_else(|| {
    id_token
      .as_deref()
      .and_then(jwt_payload)
      .and_then(|payload| claim_values(&payload, rt.grants.claim()))
      .unwrap_or_default()
  });

  // Refuse an identity the IdP explicitly reports as unverified: the allowlist
  // is keyed on email, so without this an attacker who can set an arbitrary
  // *unverified* address at a multi-tenant IdP would be handed a full admin
  // session. An IdP that omits the claim is trusted as before, but we log it so
  // the operator can tighten the IdP if needed.
  if email_verified == Some(false) {
    warn!(
      "OIDC login denied for {} (email not verified by the IdP)",
      email
    );
    state
      .audit(
        "oidc_login_denied",
        &email,
        &caller_ip.to_string(),
        "email_verified=false",
      )
      .await;
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - Your email address is not verified",
    )
      .into_response();
  }
  if email_verified.is_none() {
    warn!(
      "OIDC userinfo for {} omitted email_verified; accepting on trust",
      email
    );
  }

  if !oidc::email_allowed(&email, &rt.allowed_emails) {
    warn!("OIDC login denied for {} (not in allowlist)", email);
    state
      .audit(
        "oidc_login_denied",
        &email,
        &caller_ip.to_string(),
        &format!("email={}", email),
      )
      .await;
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - Your account is not allowed to access this service",
    )
      .into_response();
  }

  // The identity provider has said who this is. What they may do is the
  // record's to say (`planned_features.md` #154): matched by email, created
  // at the first login with the policy's default grants, and kept in step
  // with the groups claim through the map. A record nothing reaches gets no
  // session: the login is refused with a message that says an admin has to
  // grant something, rather than a dashboard that shows nothing.
  let record = match settle_record(
    &state,
    &rt,
    &email,
    bound_org.as_deref(),
    &groups,
    &caller_ip.to_string(),
  )
  .await
  {
    Ok(user) => user,
    Err(resp) => return resp,
  };
  let role_at_home = record
    .role_in(record.org_id.as_deref())
    .unwrap_or(Role::Viewer);
  // On an organization's panel the account has to reach that organization,
  // whatever else its record holds.
  let panel_host = crate::server::panel::request_host(&headers);
  if !state
    .login_admits(panel_host.as_deref(), &record.grants)
    .await
  {
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - This account does not reach the organization this panel belongs to",
    )
      .into_response();
  }

  info!("OIDC login success for {}", email);
  state
    .audit_in(
      "oidc_login_success",
      &email,
      &caller_ip.to_string(),
      bound_org.clone(),
      &format!("email={} groups={}", email, groups.join("|")),
    )
    .await;

  // Create a global session identical to the password login flow. The role
  // recorded here is informational: what the dashboard enforces is read
  // from the record on every request.
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &session_token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 86400,
      created_at: crate::store::sessions::now_secs(),
      ip: Some(caller_ip.to_string()),
      user_agent: crate::store::sessions::session_user_agent(&headers),
      // An allowlisted identity signing in to administer Aperio.
      plane: crate::store::sessions::Plane::Admin,
      scope_host: None,
      username: Some(email.clone()),
      role: role_at_home,
      selected_org: None,
      // A per-org login is fixed to that org, whatever its record holds
      // elsewhere; a global OIDC login is what its record says.
      bound_org: bound_org.clone(),
      login_host: panel_host.clone(),
    },
  );
  let cookie = session_set_cookie(state.config().secure_cookies, &session_token);
  Response::builder()
    .status(StatusCode::FOUND)
    .header("Set-Cookie", cookie)
    .header("Location", redirect_after)
    .body(Body::empty())
    .unwrap()
}

/// The values of claim `name` in a claims document: an array of strings, or
/// one string. `None` when the claim is absent, so a caller can look
/// somewhere else.
fn claim_values(doc: &serde_json::Value, name: &str) -> Option<Vec<String>> {
  match doc.get(name)? {
    serde_json::Value::Array(items) => Some(
      items
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect(),
    ),
    serde_json::Value::String(s) => Some(
      s.split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect(),
    ),
    _ => None,
  }
}

/// The payload of a JWT, decoded and parsed, signature not checked: see the
/// note on `id_token` above for why that is acceptable here and for what it
/// is used for, which is never the identity.
fn jwt_payload(token: &str) -> Option<serde_json::Value> {
  use base64::prelude::*;
  let payload = token.split('.').nth(1)?;
  let bytes = BASE64_URL_SAFE_NO_PAD.decode(payload).ok()?;
  serde_json::from_slice(&bytes).ok()
}

/// Matches the login to its dashboard user record, creating one at the first
/// login, applies the group map, and answers with the record as it now is.
/// `Err` is the response to send instead of a session: a record nothing
/// reaches.
///
/// A per-organization login lives in that organization: its record has that
/// home, its default grant names it, and its map cannot name anything else.
#[allow(clippy::result_large_err)] // see api/tokens.rs
async fn settle_record(
  state: &AppState,
  rt: &crate::oidc::OidcRuntime,
  email: &str,
  bound_org: Option<&str>,
  groups: &[String],
  ip: &str,
) -> Result<crate::store::users::User, Response> {
  use crate::store::grants::{self, Grant, GrantOrg};
  // A grant in a config file may name an organization by handle; the store
  // knows them by id. Resolved against a snapshot, so no two stores are
  // locked at once.
  let orgs: Vec<(String, String)> = state
    .org_store
    .lock()
    .await
    .list()
    .iter()
    .map(|o| (o.id.clone(), o.name.clone()))
    .collect();
  let resolve = |g: Grant| -> Grant {
    let GrantOrg::Child(name) = &g.org else {
      return g;
    };
    match orgs
      .iter()
      .find(|(id, handle)| id == name || handle.eq_ignore_ascii_case(name))
    {
      Some((id, _)) => Grant {
        org: GrantOrg::Child(id.clone()),
        ..g
      },
      None => g,
    }
  };
  let mapped: Vec<Grant> = rt.grants.mapped(groups).into_iter().map(resolve).collect();
  let defaults: Vec<Grant> = rt
    .grants
    .default_grants
    .iter()
    .cloned()
    .map(resolve)
    .collect();

  let mut users = state.users.lock().await;
  let (id, before, created) = match users.find_by_username(email) {
    Some(user) => (user.id.clone(), user.grants.clone(), false),
    None => {
      // A disabled record is a refusal, not a new account under the same
      // name: `find_by_username` skips disabled rows on purpose.
      if users.is_disabled_username(email) {
        return Err(
          (
            StatusCode::FORBIDDEN,
            "403 Forbidden - This account is disabled",
          )
            .into_response(),
        );
      }
      let user = users
        .create_sso(email, bound_org.map(str::to_string), defaults)
        .map_err(crate::api::users::user_error)?;
      (user.id.clone(), user.grants.clone(), true)
    }
  };
  // The map owns what it produces; with no map, the record is what an admin
  // wrote and stays so. At a first login the defaults count as added too,
  // so the audit line says where every grant came from.
  let (next, mut added, removed) = if rt.grants.group_grants.is_empty() && !created {
    (before.clone(), Vec::new(), Vec::new())
  } else {
    grants::apply_group_map(&before, mapped)
  };
  if created {
    for g in &next {
      if !added.contains(g) {
        added.push(g.clone());
      }
    }
  }
  let user = if next != before {
    users
      .set_grants_from_login(&id, next)
      .map_err(crate::api::users::user_error)?
  } else {
    users.get(&id).cloned().expect("looked up above")
  };
  drop(users);

  let via = |g: &Grant, fallback: &str| match &g.source {
    Some(group) => format!("group:{group}"),
    None => fallback.to_string(),
  };
  for g in &added {
    state
      .audit_in(
        "user_grant_added",
        email,
        ip,
        bound_org.map(str::to_string),
        &format!(
          "username={} org={} role={} via={}",
          email,
          g.org.as_str(),
          g.role.as_str(),
          via(g, "oidc_default")
        ),
      )
      .await;
  }
  for g in &removed {
    state
      .audit_in(
        "user_grant_removed",
        email,
        ip,
        bound_org.map(str::to_string),
        &format!(
          "username={} org={} role={} via={}",
          email,
          g.org.as_str(),
          g.role.as_str(),
          via(g, "oidc_map")
        ),
      )
      .await;
  }

  // What the session may act in: the whole record for a global login, this
  // organization alone for a per-organization one.
  let reaches = match bound_org {
    Some(org) => user.role_in(Some(org)).is_some(),
    None => !user.grants.is_empty(),
  };
  if !reaches {
    warn!(
      "OIDC login for {} refused: the record reaches nothing{}",
      email,
      if created {
        " (created at this login)"
      } else {
        ""
      }
    );
    state
      .audit_in(
        "oidc_login_denied",
        email,
        ip,
        bound_org.map(str::to_string),
        &format!("email={} no_grants=true created={}", email, created),
      )
      .await;
    return Err(
      (
        StatusCode::FORBIDDEN,
        "403 Forbidden - Your identity was verified, but no organization has been granted to this \
         account yet. Ask an Aperio administrator to grant one on the Users page.",
      )
        .into_response(),
    );
  }
  Ok(user)
}

/// Validates a redirect path to prevent open redirect attacks.
/// Only allows same-origin relative paths (starting with `/`) and rejects
/// protocol-relative URLs (`//evil.com`) and backslash-based bypasses (`/\`).
pub(crate) fn safe_redirect_path(uri: &str) -> &str {
  if uri.starts_with('/') && !uri.starts_with("//") && !uri.starts_with("/\\") {
    uri
  } else {
    "/"
  }
}

#[cfg(test)]
#[path = "oidc_flow_tests.rs"]
mod tests;
