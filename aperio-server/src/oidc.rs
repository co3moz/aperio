use serde::Deserialize;
use tracing::{error, info};

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

/// Builds an OIDC runtime by fetching the issuer's discovery document.
/// Returns an error string on any misconfiguration instead of exiting, so it
/// is safe to call for a per-organization override (a bad tenant config must
/// not take the whole server down). `load_from_env` maps the error to a fatal
/// startup exit; the per-org path surfaces it as a login failure.
pub async fn build_runtime(
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
  let doc: DiscoveryDoc = reqwest::Client::new()
    .get(&discovery_url)
    .timeout(std::time::Duration::from_secs(15))
    .send()
    .await
    .and_then(|r| r.error_for_status())
    .map_err(|e| format!("failed to fetch OIDC discovery document: {e}"))?
    .json()
    .await
    .map_err(|e| format!("failed to parse OIDC discovery document: {e}"))?;
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
pub async fn load_from_env() -> Option<OidcRuntime> {
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
  match build_runtime(
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
mod tests {
  use super::*;

  #[test]
  fn test_email_allowed() {
    let patterns = vec!["ceo@corp.com".to_string(), "*@team.example.com".to_string()];
    assert!(email_allowed("ceo@corp.com", &patterns));
    assert!(email_allowed("CEO@Corp.com", &patterns));
    assert!(email_allowed("dev@team.example.com", &patterns));
    assert!(!email_allowed("dev@corp.com", &patterns));
    assert!(!email_allowed(
      "dev@evil-team.example.com.attacker.io",
      &patterns
    ));
    assert!(!email_allowed("", &patterns));

    // Wildcard-all
    assert!(email_allowed("anyone@anywhere.io", &["*".to_string()]));
    // Suffix trickery must not match the domain pattern
    assert!(!email_allowed(
      "x@nteam.example.com",
      &["*@team.example.com".to_string()]
    ));
  }
}
