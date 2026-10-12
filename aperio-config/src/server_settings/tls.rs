//! The `tls` section: serving HTTPS without a proxy in front, from
//! certificate files, from an ACME authority, or both.
//!
//! A section of its own rather than a struct in `sections.rs` because it is a
//! subject of its own: the listener, where certificates come from, and the
//! authority that issues them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Let's Encrypt's production directory, the authority used unless another
/// is named.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Tls {
  /// Serve HTTPS on `port`, beside the plain listener on the boot `PORT`.
  /// Needs certificates: `certificates`, `acme`, or both. Default `false`.
  pub enabled: bool,
  /// Port of the HTTPS listener, on the boot `HOST`. An ACME authority
  /// proves a hostname by connecting to it on 443, so with `acme` this is
  /// 443, or whatever forwards 443 to it. Default `443`.
  #[schemars(extend("examples" = [8443]))]
  pub port: u16,
  /// Answer plain HTTP with a redirect to HTTPS, except the health probes.
  /// Tunnel clients then have to dial `wss://`. Default `false`.
  pub redirect_http: bool,
  /// Certificates you manage yourself, as PEM files. Each serves the names
  /// written in it, a wildcard included, and is read again when its files
  /// change, so a renewal by another tool needs no restart.
  #[schemars(extend("examples" = [[{"cert": "/etc/aperio/tls/example.com.pem", "key": "/etc/aperio/tls/example.com.key"}]]))]
  pub certificates: Vec<CertificateFiles>,
  /// Certificates obtained and renewed from an ACME authority, Let's
  /// Encrypt unless another is named, proven with TLS-ALPN-01 on the HTTPS
  /// port.
  pub acme: Acme,
}

impl Default for Tls {
  fn default() -> Self {
    Tls {
      enabled: false,
      port: 443,
      redirect_http: false,
      certificates: Vec::new(),
      acme: Acme::default(),
    }
  }
}

/// One certificate and its key, as files on the server.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CertificateFiles {
  /// The certificate chain, PEM, leaf first.
  #[schemars(extend("examples" = ["/etc/aperio/tls/example.com.pem"]))]
  pub cert: String,
  /// Its private key, PEM.
  #[schemars(extend("examples" = ["/etc/aperio/tls/example.com.key"]))]
  pub key: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Acme {
  /// Obtain and renew certificates automatically. Turning it on agrees to
  /// the authority's subscriber agreement. Default `false`.
  pub enabled: bool,
  /// Where the authority sends notices about the certificates, such as one
  /// about to expire that could not be renewed.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["ops@example.com"]))]
  pub email: Option<String>,
  /// The authority's ACME directory. Default Let's Encrypt; its staging
  /// directory, `https://acme-staging-v02.api.letsencrypt.org/directory`,
  /// is the one to try a setup against without spending its rate limits.
  #[schemars(extend("examples" = ["https://acme-staging-v02.api.letsencrypt.org/directory"]))]
  pub directory: String,
  /// Hostnames to hold a certificate for from the start, such as the
  /// dashboard's own. A wildcard needs a DNS-01 proof, which this server
  /// does not make.
  #[schemars(extend("examples" = [["tunnel.example.com"]]))]
  pub hostnames: Vec<String>,
  /// Also obtain a certificate the first time a hostname a client serves is
  /// reached over HTTPS, holding that handshake until it is issued. Random
  /// subdomains are left out: each would be a certificate of its own, and an
  /// authority limits how many one domain may have. Default `true`.
  pub on_demand: bool,
}

impl Default for Acme {
  fn default() -> Self {
    Acme {
      enabled: false,
      email: None,
      directory: LETS_ENCRYPT.to_string(),
      hostnames: Vec::new(),
      on_demand: true,
    }
  }
}
