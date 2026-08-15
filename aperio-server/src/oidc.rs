use serde::Deserialize;
use tracing::{error, info, warn};

/// Runtime OIDC configuration resolved from the issuer's discovery document.
#[derive(Clone)]
pub struct OidcRuntime {
  pub authorization_endpoint: String,
  pub token_endpoint: String,
  pub userinfo_endpoint: String,
  pub client_id: String,
  pub client_secret: String,
  pub scopes: String,
  /// Allowed email patterns: exact addresses, `*@domain`, or `*`.
  pub allowed_emails: Vec<String>,
  /// Optional fixed redirect URL (otherwise derived from the request Host).
  pub redirect_url_override: Option<String>,
}

#[derive(Deserialize)]
struct DiscoveryDoc {
  authorization_endpoint: String,
  token_endpoint: String,
  userinfo_endpoint: Option<String>,
}

/// Largest discovery document read back. Generous for a document of a dozen
/// URLs, and a limit rather than a guess about the issuer's good behaviour.
const MAX_DISCOVERY_BYTES: usize = 256 * 1024;

/// Fetches JSON from an issuer, through the fence every other outbound call
/// the server makes goes through.
///
/// The issuer URL comes from a configuration file (or from an organization's
/// stored settings) and makes the *server* issue a request, which is the exact
/// shape [`crate::outbound::OutboundPolicy`] exists to refuse. Redirects are
/// not followed for the reason the check exists: a URL that passes the fence
/// must not be able to hand the server a `Location` pointing behind it. The
/// body is bounded while it is read, so an issuer, or anything answering as
/// one, does not decide how much memory this costs.
pub(crate) async fn fetch_json<T: serde::de::DeserializeOwned>(
  policy: &crate::outbound::OutboundPolicy,
  url: &str,
) -> Result<T, FetchFailure> {
  policy
    .check(url)
    .await
    .map_err(|why| FetchFailure::Call(format!("refused by the outbound policy: {why}")))?;
  let http = crate::outbound::client_builder()
    .timeout(std::time::Duration::from_secs(15))
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|e| FetchFailure::Call(e.to_string()))?;
  let res = http
    .get(url)
    .send()
    .await
    .and_then(|r| r.error_for_status())
    .map_err(|e| FetchFailure::Call(e.to_string()))?;
  let body = crate::outbound::read_bounded(res, MAX_DISCOVERY_BYTES)
    .await
    .ok_or_else(|| {
      FetchFailure::Call(format!(
        "unreadable, or larger than {MAX_DISCOVERY_BYTES} bytes"
      ))
    })?;
  serde_json::from_str(&body).map_err(|e| FetchFailure::Parse(e.to_string()))
}

/// Which half of a fetch went wrong, kept apart because the two send an
/// operator to different places: one is the network, the fence or the
/// issuer's availability, the other is the document it served.
pub(crate) enum FetchFailure {
  Call(String),
  Parse(String),
}

/// Builds an OIDC runtime by fetching the issuer's discovery document.
/// Returns an error string on any misconfiguration instead of exiting, so it
/// is safe to call for a per-organization override (a bad tenant config must
/// not take the whole server down). `load_from_env` maps the error to a fatal
/// startup exit; the per-org path surfaces it as a login failure.
pub async fn build_runtime(
  policy: &crate::outbound::OutboundPolicy,
  issuer: &str,
  client_id: &str,
  client_secret: &str,
  allowed_emails: Vec<String>,
  scopes: String,
  redirect_url_override: Option<String>,
) -> Result<OidcRuntime, String> {
  let issuer = issuer.trim().trim_end_matches('/');
  if issuer.is_empty() {
    return Err("OIDC issuer is empty".into());
  }
  if client_id.trim().is_empty() || client_secret.trim().is_empty() {
    return Err("OIDC client id / client secret are missing".into());
  }
  if allowed_emails.is_empty() {
    return Err(
      "OIDC allowed emails must be set (comma separated; supports user@x.com, *@x.com, *)".into(),
    );
  }
  let discovery_url = format!("{issuer}/.well-known/openid-configuration");
  info!("Fetching OIDC discovery document from {}", discovery_url);
  let doc: DiscoveryDoc = fetch_json(policy, &discovery_url)
    .await
    .map_err(|e| match e {
      FetchFailure::Call(why) => format!("failed to fetch OIDC discovery document: {why}"),
      FetchFailure::Parse(why) => format!("failed to parse OIDC discovery document: {why}"),
    })?;
  let userinfo_endpoint = doc
    .userinfo_endpoint
    .ok_or_else(|| "OIDC issuer does not advertise a userinfo_endpoint".to_string())?;

  info!(
    "OIDC runtime built (issuer: {}, allowed: {:?})",
    issuer, allowed_emails
  );
  Ok(OidcRuntime {
    authorization_endpoint: doc.authorization_endpoint,
    token_endpoint: doc.token_endpoint,
    userinfo_endpoint,
    client_id: client_id.to_string(),
    client_secret: client_secret.to_string(),
    scopes,
    allowed_emails,
    redirect_url_override,
  })
}

/// Loads OIDC configuration from `APERIO_OIDC_*` environment variables. Returns
/// `None` when the feature is not configured; exits the process on
/// misconfiguration so a broken SSO setup never silently exposes the app.
pub async fn load_from_env(policy: &crate::outbound::OutboundPolicy) -> Option<OidcRuntime> {
  let issuer = std::env::var("APERIO_OIDC_ISSUER").ok()?;
  if issuer.trim().is_empty() {
    return None;
  }
  let allowed_emails: Vec<String> = std::env::var("APERIO_OIDC_ALLOWED_EMAILS")
    .unwrap_or_default()
    .split(',')
    .map(|s| s.trim().to_ascii_lowercase())
    .filter(|s| !s.is_empty())
    .collect();
  let scopes =
    std::env::var("APERIO_OIDC_SCOPES").unwrap_or_else(|_| "openid email profile".to_string());
  let redirect_url_override = std::env::var("APERIO_OIDC_REDIRECT_URL")
    .ok()
    .filter(|s| !s.trim().is_empty());
  if redirect_url_override.is_none() {
    // Not fatal: deriving works, and requiring the key would break every
    // deployment that never set it. But it is worth saying out loud, because
    // the failure it guards against is silent, the `Host` of the request that
    // starts a login decides where the provider returns the code.
    warn!(
      "OIDC: APERIO_OIDC_REDIRECT_URL is not set, so the callback URL is derived from each \
       request's Host header. Set it to the one URL registered with your provider."
    );
  }
  match build_runtime(
    policy,
    &issuer,
    &std::env::var("APERIO_OIDC_CLIENT_ID").unwrap_or_default(),
    &std::env::var("APERIO_OIDC_CLIENT_SECRET").unwrap_or_default(),
    allowed_emails,
    scopes,
    redirect_url_override,
  )
  .await
  {
    Ok(rt) => Some(rt),
    Err(e) => {
      error!("OIDC configuration error: {e}");
      std::process::exit(1);
    }
  }
}

/// Checks an authenticated email against the allowed patterns
/// (`user@x.com` exact, `*@x.com` domain, `*` any).
pub fn email_allowed(email: &str, patterns: &[String]) -> bool {
  let email = email.trim().to_ascii_lowercase();
  if email.is_empty() {
    return false;
  }
  patterns.iter().any(|p| {
    if p == "*" {
      return true;
    }
    if let Some(domain) = p.strip_prefix("*@") {
      return email.rsplit_once('@').is_some_and(|(_, d)| d == domain);
    }
    p == &email
  })
}

#[cfg(test)]
#[path = "oidc_tests.rs"]
mod tests;
