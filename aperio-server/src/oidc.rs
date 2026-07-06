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

/// Loads OIDC configuration from `APERIO_OIDC_*` environment variables and
/// the issuer's `/.well-known/openid-configuration`. Returns `None` when the
/// feature is not configured; exits the process on misconfiguration so a
/// broken SSO setup never silently exposes the proxied app.
pub async fn load_from_env() -> Option<OidcRuntime> {
  let issuer = std::env::var("APERIO_OIDC_ISSUER").ok()?;
  let issuer = issuer.trim().trim_end_matches('/').to_string();
  if issuer.is_empty() {
    return None;
  }
  let client_id = std::env::var("APERIO_OIDC_CLIENT_ID").unwrap_or_default();
  let client_secret = std::env::var("APERIO_OIDC_CLIENT_SECRET").unwrap_or_default();
  if client_id.trim().is_empty() || client_secret.trim().is_empty() {
    error!(
      "APERIO_OIDC_ISSUER is set but APERIO_OIDC_CLIENT_ID / APERIO_OIDC_CLIENT_SECRET are missing!"
    );
    std::process::exit(1);
  }
  let allowed_emails: Vec<String> = std::env::var("APERIO_OIDC_ALLOWED_EMAILS")
    .unwrap_or_default()
    .split(',')
    .map(|s| s.trim().to_ascii_lowercase())
    .filter(|s| !s.is_empty())
    .collect();
  if allowed_emails.is_empty() {
    error!(
      "APERIO_OIDC_ALLOWED_EMAILS must be set (comma separated; supports user@x.com, *@x.com, *)"
    );
    std::process::exit(1);
  }
  let scopes =
    std::env::var("APERIO_OIDC_SCOPES").unwrap_or_else(|_| "openid email profile".to_string());
  let redirect_url_override = std::env::var("APERIO_OIDC_REDIRECT_URL")
    .ok()
    .filter(|s| !s.trim().is_empty());

  let discovery_url = format!("{}/.well-known/openid-configuration", issuer);
  info!("Fetching OIDC discovery document from {}", discovery_url);
  let doc: DiscoveryDoc = match reqwest::Client::new()
    .get(&discovery_url)
    .timeout(std::time::Duration::from_secs(15))
    .send()
    .await
    .and_then(|r| r.error_for_status())
  {
    Ok(res) => match res.json().await {
      Ok(doc) => doc,
      Err(e) => {
        error!("Failed to parse OIDC discovery document: {}", e);
        std::process::exit(1);
      }
    },
    Err(e) => {
      error!("Failed to fetch OIDC discovery document: {}", e);
      std::process::exit(1);
    }
  };
  let userinfo_endpoint = match doc.userinfo_endpoint {
    Some(u) => u,
    None => {
      error!("OIDC issuer does not advertise a userinfo_endpoint; cannot proceed");
      std::process::exit(1);
    }
  };

  info!(
    "OIDC SSO enabled (issuer: {}, allowed: {:?})",
    issuer, allowed_emails
  );
  Some(OidcRuntime {
    authorization_endpoint: doc.authorization_endpoint,
    token_endpoint: doc.token_endpoint,
    userinfo_endpoint,
    client_id,
    client_secret,
    scopes,
    allowed_emails,
    redirect_url_override,
  })
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
