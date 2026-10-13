//! Certificates from an ACME authority: the account, an order proven with
//! TLS-ALPN-01 on the HTTPS listener itself, renewal, and the limits that
//! keep a busy server inside the authority's own.
//!
//! TLS-ALPN-01 is the proof that needs nothing but the port the listener is
//! already on: the authority connects to 443 asking for the `acme-tls/1`
//! protocol, and the listener answers that one handshake with a certificate
//! carrying the proof. No port 80, no DNS provider.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use instant_acme::{
  Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
  NewOrder, OrderStatus, RetryPolicy,
};
use rustls::pki_types::PrivateKeyDer;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tracing::{info, warn};

use super::certs::{CertStore, Loaded, write_private};
use super::http::ReqwestAcme;
use super::plan::AcmePlan;
use crate::state::AppState;

/// After a failed order, how long before the same hostname is tried again.
/// An authority counts failed validations per name, and Let's Encrypt allows
/// five an hour; one attempt an hour stays well inside it.
pub(crate) const FAILURE_BACKOFF: Duration = Duration::from_secs(3600);
/// New certificates obtained on demand in any one hour. A tenant who points
/// many names at the server spends this rather than the authority's limit
/// for the whole registered domain.
pub(crate) const ON_DEMAND_PER_HOUR: usize = 20;
/// A certificate is renewed once it has this little left. Let's Encrypt's
/// last for ninety days.
pub(crate) const RENEW_WITHIN: i64 = 30 * 86_400;
/// How often the renewal pass looks.
const RENEWAL_EVERY: Duration = Duration::from_secs(12 * 3600);

/// Why a certificate is being asked for, which decides the limits it meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
  /// Named in `tls.acme.hostnames`, the dashboard's or a panel's.
  Configured,
  /// The first handshake for a hostname a client serves.
  OnDemand,
  /// One held already, close to its end.
  Renewal,
}

impl Why {
  fn as_str(self) -> &'static str {
    match self {
      Why::Configured => "configured",
      Why::OnDemand => "on-demand",
      Why::Renewal => "renewal",
    }
  }
}

/// The TLS-ALPN-01 proofs being served right now, by hostname.
#[derive(Debug, Default)]
pub(crate) struct Challenges(RwLock<HashMap<String, Arc<CertifiedKey>>>);

impl Challenges {
  fn set(&self, host: &str, key: Arc<CertifiedKey>) {
    if let Ok(mut map) = self.0.write() {
      map.insert(host.to_string(), key);
    }
  }

  fn clear(&self, host: &str) {
    if let Ok(mut map) = self.0.write() {
      map.remove(host);
    }
  }

  pub(crate) fn get(&self, host: &str) -> Option<Arc<CertifiedKey>> {
    self.0.read().ok()?.get(host).cloned()
  }
}

/// Presents a proof, and only to a handshake asking for `acme-tls/1`.
#[derive(Debug)]
pub(crate) struct ChallengeResolver(pub(crate) Arc<Challenges>);

impl ResolvesServerCert for ChallengeResolver {
  fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
    self.0.get(&hello.server_name()?.to_ascii_lowercase())
  }
}

/// The self-signed certificate that proves control of `host` to an
/// authority: the hostname, and the digest of the key authorization in the
/// `acmeIdentifier` extension (RFC 8737).
pub(crate) fn proof_certificate(host: &str, digest: &[u8]) -> Result<Arc<CertifiedKey>, String> {
  let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
  let mut params =
    rcgen::CertificateParams::new(vec![host.to_string()]).map_err(|e| e.to_string())?;
  params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
  let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
  let der = PrivateKeyDer::Pkcs8(key.serialize_der().into());
  let signing = rustls::crypto::ring::sign::any_supported_type(&der).map_err(|e| e.to_string())?;
  Ok(Arc::new(CertifiedKey::new(
    vec![cert.der().clone()],
    signing,
  )))
}

/// Obtains and renews certificates from one authority.
pub(crate) struct Issuer {
  plan: AcmePlan,
  /// Where the account is kept, beside the issued certificates.
  dir: PathBuf,
  certs: Arc<CertStore>,
  challenges: Arc<Challenges>,
  state: Option<Arc<AppState>>,
  account: tokio::sync::Mutex<Option<Account>>,
  /// One order per hostname at a time: the handshakes waiting on a name
  /// share the one order rather than each placing their own.
  orders: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
  failed: std::sync::Mutex<HashMap<String, Instant>>,
  recent: std::sync::Mutex<VecDeque<Instant>>,
  /// How long an order may take to become ready and to be issued.
  patience: Duration,
}

impl Issuer {
  pub(crate) fn new(
    plan: AcmePlan,
    dir: PathBuf,
    certs: Arc<CertStore>,
    challenges: Arc<Challenges>,
    state: Option<Arc<AppState>>,
  ) -> Self {
    Issuer {
      plan,
      dir,
      certs,
      challenges,
      state,
      account: tokio::sync::Mutex::new(None),
      orders: std::sync::Mutex::new(HashMap::new()),
      failed: std::sync::Mutex::new(HashMap::new()),
      recent: std::sync::Mutex::new(VecDeque::new()),
      patience: Duration::from_secs(90),
    }
  }

  pub(crate) fn plan(&self) -> &AcmePlan {
    &self.plan
  }

  /// Why a certificate for `host` is wanted: [`Why::Configured`] for a name
  /// in `tls.acme.hostnames`, the dashboard's or an organization's panel,
  /// which are names the operator wrote and this server has to answer for,
  /// and [`Why::OnDemand`] for a name a client happens to serve. The
  /// difference is the hourly allowance: a panel used to be obtained as an
  /// on-demand name and spent one of a tenant's twenty slots, whether or not
  /// `on_demand` was even on.
  pub(crate) async fn reason_for(&self, host: &str) -> Why {
    if self.plan.hostnames.iter().any(|h| h == host) {
      return Why::Configured;
    }
    match &self.state {
      Some(state) if state.panel_org(Some(host)).await.is_some() => Why::Configured,
      _ => Why::OnDemand,
    }
  }

  /// Makes sure a certificate for `host` is held, obtaining one if it is not
  /// (or renewing it, for [`Why::Renewal`]).
  pub(crate) async fn ensure(&self, host: &str, why: Why) -> Result<(), String> {
    if why != Why::Renewal && self.certs.issued(host).is_some() {
      return Ok(());
    }
    if let Some(until) = self.failed.lock().ok().and_then(|f| f.get(host).copied())
      && until > Instant::now()
    {
      return Err(format!(
        "the last attempt for {host} failed; the next is in {} minute(s)",
        (until - Instant::now()).as_secs() / 60 + 1
      ));
    }
    let lock = {
      let mut orders = self.orders.lock().map_err(|_| "poisoned")?;
      orders.entry(host.to_string()).or_default().clone()
    };
    let _held = lock.lock().await;
    // Another handshake's order may have finished while this one waited.
    let held_now = self.certs.issued(host);
    match why {
      Why::Renewal => {
        if held_now.is_some_and(|l| l.not_after - chrono::Utc::now().timestamp() >= RENEW_WITHIN) {
          return Ok(());
        }
      }
      _ if held_now.is_some() => return Ok(()),
      Why::OnDemand => self.take_on_demand_slot()?,
      Why::Configured => {}
    }
    match self.issue(host).await {
      Ok(loaded) => {
        if let Ok(mut failed) = self.failed.lock() {
          failed.remove(host);
        }
        let expires = chrono::DateTime::from_timestamp(loaded.not_after, 0)
          .map(|t| t.format("%Y-%m-%d").to_string())
          .unwrap_or_default();
        info!(
          "TLS certificate for {host} issued ({}), valid until {expires}",
          why.as_str()
        );
        self
          .audit(
            "tls_certificate_issued",
            &format!("hostname={host} reason={} expires={expires}", why.as_str()),
          )
          .await;
        Ok(())
      }
      Err(e) => {
        if let Ok(mut failed) = self.failed.lock() {
          failed.insert(host.to_string(), Instant::now() + FAILURE_BACKOFF);
        }
        warn!(
          "TLS certificate for {host} ({}) could not be issued: {e}",
          why.as_str()
        );
        self
          .audit(
            "tls_certificate_failed",
            &format!("hostname={host} reason={} error={e}", why.as_str()),
          )
          .await;
        Err(e)
      }
    }
  }

  /// Counts an on-demand order against the hourly allowance, or refuses it.
  fn take_on_demand_slot(&self) -> Result<(), String> {
    let mut recent = self.recent.lock().map_err(|_| "poisoned")?;
    let hour_ago = Instant::now().checked_sub(Duration::from_secs(3600));
    while recent
      .front()
      .is_some_and(|t| hour_ago.is_some_and(|h| *t < h))
    {
      recent.pop_front();
    }
    if recent.len() >= ON_DEMAND_PER_HOUR {
      return Err(format!(
        "{ON_DEMAND_PER_HOUR} certificates were obtained on demand in the last hour; the next waits"
      ));
    }
    recent.push_back(Instant::now());
    Ok(())
  }

  async fn audit(&self, event: &str, details: &str) {
    if let Some(state) = &self.state {
      state.audit(event, "system", "-", details).await;
    }
  }

  /// The account file, one per directory, so moving from an authority's
  /// staging directory to production makes a new account rather than
  /// presenting staging credentials to production.
  fn account_path(&self) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(self.plan.directory.as_bytes());
    let short: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    self.dir.join(format!("account-{short}.json"))
  }

  async fn account(&self) -> Result<Account, String> {
    let mut held = self.account.lock().await;
    if let Some(account) = held.as_ref() {
      return Ok(account.clone());
    }
    let path = self.account_path();
    let account = match std::fs::read(&path) {
      Ok(raw) => {
        let credentials: AccountCredentials = serde_json::from_slice(&raw)
          .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
        Account::builder_with_http(Box::new(ReqwestAcme::outbound()?))
          .from_credentials(credentials)
          .await
          .map_err(|e| format!("the account in {} was refused: {e}", path.display()))?
      }
      Err(_) => {
        let contact = self.plan.email.as_ref().map(|e| format!("mailto:{e}"));
        let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, credentials) = Account::builder_with_http(Box::new(ReqwestAcme::outbound()?))
          .create(
            &NewAccount {
              contact: &contacts,
              terms_of_service_agreed: true,
              only_return_existing: false,
            },
            self.plan.directory.clone(),
            None,
          )
          .await
          .map_err(|e| format!("the authority refused a new account: {e}"))?;
        let raw = serde_json::to_vec(&credentials).map_err(|e| e.to_string())?;
        write_private(&path, &raw)
          .map_err(|e| format!("could not keep the account in {}: {e}", path.display()))?;
        info!("ACME account created at {}", self.plan.directory);
        account
      }
    };
    *held = Some(account.clone());
    Ok(account)
  }

  /// One order for `host`, proven over TLS-ALPN-01, through to the issued
  /// certificate, which is then kept.
  async fn issue(&self, host: &str) -> Result<Loaded, String> {
    let account = self.account().await?;
    let identifiers = [Identifier::Dns(host.to_string())];
    let mut order = account
      .new_order(&NewOrder::new(&identifiers))
      .await
      .map_err(|e| format!("the order was refused: {e}"))?;
    let proved = self.prove(&mut order, host).await;
    let finished = match proved {
      Ok(()) => self.finish(&mut order, host).await,
      Err(e) => Err(e),
    };
    self.challenges.clear(host);
    finished
  }

  async fn prove(&self, order: &mut instant_acme::Order, host: &str) -> Result<(), String> {
    let mut authorizations = order.authorizations();
    while let Some(authorization) = authorizations.next().await {
      let mut authorization =
        authorization.map_err(|e| format!("the authorization could not be read: {e}"))?;
      match authorization.status {
        AuthorizationStatus::Valid => continue,
        AuthorizationStatus::Pending => {}
        other => return Err(format!("the authorization is {other:?}")),
      }
      let mut challenge = authorization
        .challenge(ChallengeType::TlsAlpn01)
        .ok_or("the authority offered no tls-alpn-01 challenge")?;
      let digest = challenge.key_authorization().digest();
      self
        .challenges
        .set(host, proof_certificate(host, digest.as_ref())?);
      challenge
        .set_ready()
        .await
        .map_err(|e| format!("the challenge could not be answered: {e}"))?;
    }
    Ok(())
  }

  async fn finish(&self, order: &mut instant_acme::Order, host: &str) -> Result<Loaded, String> {
    let retry = RetryPolicy::new().timeout(self.patience);
    match order.poll_ready(&retry).await {
      Ok(OrderStatus::Ready) => {}
      Ok(status) => {
        return Err(format!(
          "the authority could not verify {host} ({status:?}); is port 443 of this server reachable for it, at that name?"
        ));
      }
      Err(e) => return Err(format!("the proof was not accepted: {e}")),
    }
    let key_pem = order
      .finalize()
      .await
      .map_err(|e| format!("the order could not be finalized: {e}"))?;
    let chain_pem = order
      .poll_certificate(&retry)
      .await
      .map_err(|e| format!("the certificate was not issued: {e}"))?;
    self.certs.store_issued(host, &chain_pem, &key_pem)
  }

  /// Obtains what is configured, then renews what is due, for as long as
  /// the server runs.
  pub(crate) async fn run(self: Arc<Self>) {
    let mut wanted = self.plan.hostnames.clone();
    // The dashboard's hostname and every organization's panel: names the
    // operator wrote, obtained at start like the configured ones rather than
    // at the first visitor's handshake. A panel set later is obtained then,
    // as a configured name.
    if let Some(state) = &self.state {
      let mut panels: Vec<String> = state.org_store.lock().await.panel_hostnames();
      panels.extend(state.config().dashboard_hostname.clone());
      for host in panels {
        if super::plan::certifiable(&host).is_ok() && !wanted.contains(&host) {
          wanted.push(host);
        }
      }
    }
    for host in wanted {
      let _ = self.ensure(&host, Why::Configured).await;
    }
    loop {
      for host in self.certs.due(chrono::Utc::now().timestamp(), RENEW_WITHIN) {
        let _ = self.ensure(&host, Why::Renewal).await;
      }
      tokio::time::sleep(RENEWAL_EVERY).await;
    }
  }

  /// Whether a handshake for `host` may wait for a certificate to be
  /// obtained: a configured name, the dashboard's or a panel's, or, with
  /// `on_demand`, one a connected client serves that is not a random
  /// subdomain.
  pub(crate) async fn wants(&self, host: &str) -> bool {
    if super::plan::certifiable(host).is_err() {
      return false;
    }
    if self.plan.hostnames.iter().any(|h| h == host) {
      return true;
    }
    let Some(state) = &self.state else {
      return false;
    };
    if state.panel_org(Some(host)).await.is_some() {
      return true;
    }
    if !self.plan.on_demand {
      return false;
    }
    let config = state.config();
    if config
      .random_subdomain_suffix
      .as_deref()
      .is_some_and(|pattern| crate::routing::host_matches_random_pattern(host, pattern))
    {
      return false;
    }
    let clients = state.clients.read().await;
    clients
      .values()
      .any(|c| c.effective_hostnames().into_iter().any(|h| h == host))
  }
}

#[cfg(test)]
#[path = "acme_tests.rs"]
mod tests;
