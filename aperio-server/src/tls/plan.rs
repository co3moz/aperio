//! The `tls` section, checked: what the HTTPS listener will serve, settled
//! when the settings are saved so a document that cannot work is refused with
//! the path that is wrong, rather than discovered when the server restarts.

use aperio_config::server_settings::{CertificateFiles, Tls};

/// What the HTTPS listener serves, from a `tls` section that passed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TlsPlan {
  pub(crate) port: u16,
  pub(crate) redirect_http: bool,
  pub(crate) certificates: Vec<CertificateFiles>,
  pub(crate) acme: Option<AcmePlan>,
}

/// The ACME half, when it is on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AcmePlan {
  pub(crate) directory: String,
  pub(crate) email: Option<String>,
  /// Normalized: lower case, no trailing dot, no duplicates.
  pub(crate) hostnames: Vec<String>,
  pub(crate) on_demand: bool,
}

/// `None` when HTTPS is off. `Err` names the setting that cannot work.
pub(crate) fn plan(tls: &Tls) -> Result<Option<TlsPlan>, String> {
  if !tls.enabled {
    if tls.acme.enabled {
      return Err(
        "tls.acme.enabled: needs tls.enabled, since the proof is made on the HTTPS listener"
          .to_string(),
      );
    }
    if tls.redirect_http {
      return Err(
        "tls.redirect_http: needs tls.enabled, or there is nothing to redirect to".to_string(),
      );
    }
    return Ok(None);
  }
  if tls.port == 0 {
    return Err("tls.port: 0 is not a port".to_string());
  }
  for (i, files) in tls.certificates.iter().enumerate() {
    if files.cert.trim().is_empty() || files.key.trim().is_empty() {
      return Err(format!(
        "tls.certificates[{i}]: both `cert` and `key` are file paths"
      ));
    }
  }
  if tls.certificates.is_empty() && !tls.acme.enabled {
    return Err(
      "tls.enabled: there is nothing to serve; add tls.certificates, or turn tls.acme.enabled on"
        .to_string(),
    );
  }
  let acme = if tls.acme.enabled {
    Some(acme_plan(&tls.acme)?)
  } else {
    None
  };
  Ok(Some(TlsPlan {
    port: tls.port,
    redirect_http: tls.redirect_http,
    certificates: tls.certificates.clone(),
    acme,
  }))
}

fn acme_plan(acme: &aperio_config::server_settings::Acme) -> Result<AcmePlan, String> {
  let directory = acme.directory.trim();
  let parsed = url::Url::parse(directory)
    .map_err(|e| format!("tls.acme.directory: `{directory}` is not a URL: {e}"))?;
  if parsed.scheme() != "https" {
    return Err(format!(
      "tls.acme.directory: `{directory}` is not https://, and an ACME directory always is"
    ));
  }
  let email = match acme.email.as_deref().map(str::trim) {
    None | Some("") => None,
    Some(email) => {
      let valid = email.split_once('@').is_some_and(|(user, domain)| {
        !user.is_empty() && domain.contains('.') && !email.contains(char::is_whitespace)
      });
      if !valid {
        return Err(format!("tls.acme.email: `{email}` is not an email address"));
      }
      Some(email.to_string())
    }
  };
  let mut hostnames: Vec<String> = Vec::new();
  for raw in &acme.hostnames {
    let host = certifiable(raw).map_err(|why| format!("tls.acme.hostnames: `{raw}` {why}"))?;
    if !hostnames.contains(&host) {
      hostnames.push(host);
    }
  }
  Ok(AcmePlan {
    directory: directory.to_string(),
    email,
    hostnames,
    on_demand: acme.on_demand,
  })
}

/// A hostname an authority can certify over TLS-ALPN-01, normalized, or
/// why it is not one.
pub(crate) fn certifiable(raw: &str) -> Result<String, &'static str> {
  if raw.contains('*') {
    return Err("is a wildcard, which needs a DNS-01 proof this server does not make");
  }
  let host = crate::routing::normalize_hostname_bind(raw).ok_or("is not a hostname")?;
  if host.parse::<std::net::IpAddr>().is_ok()
    || host
      .split('.')
      .all(|l| l.chars().all(|c| c.is_ascii_digit()))
  {
    return Err("is an address; an authority certifies names");
  }
  if !host.contains('.') {
    return Err("has a single label, which no public authority certifies");
  }
  Ok(host)
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
