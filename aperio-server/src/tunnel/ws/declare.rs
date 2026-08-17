//! Applying one service declaration from a heartbeat.
//!
//! One pass of what the Ping loop does per entry: take a `ServiceDecl`, decide
//! field by field what this token actually permits, and write the result onto
//! that service. It is long because a declaration is wide, not because it does
//! several things.
//!
//! Extracted from `on_ping` on the measurement the file above records: the
//! loop body re-binds every field from its own `ServiceDecl`, so nothing of
//! the Ping's forty locals crosses into it. What does cross is named in the
//! signature, and the three `&mut Option<_>` are the decisions that cannot be
//! acted on under the `clients` write lock and are carried out to the caller.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{info, warn};

use super::*;
use crate::protocol::{PROTOCOL_VERSION, ServiceDecl};
use crate::routing::{normalize_hostname_bind, normalize_path_bind};
use crate::state::ClientHandle;
use tokio::sync::Semaphore;

/// What a declaration decides that cannot be acted on where it is decided.
///
/// Both are settled under the `clients` write lock and need it released before
/// they can be carried out: pinning writes to the token store, and arming a
/// scaling record calls out to another one. Carried together rather than as
/// two loose out-parameters, which is also what says they are one kind of
/// thing.
#[derive(Default)]
pub(super) struct Deferred {
  /// The token this connection authenticated with, for trust-on-first-use.
  pub(super) pin: Option<(String, String, Option<String>)>,
  /// The autoscaling block a service declared, with the binds it was for.
  pub(super) scaling: Option<ScalingBindCtx>,
}

impl ConnCtx {
  /// Applies one declaration to `handle`'s service at `service_index`.
  ///
  /// `protocol` is the one Ping field that is the *connection's* rather than a
  /// service's and is still needed here, for the version-skew warning.
  pub(super) fn apply_declaration(
    &self,
    handle: &mut ClientHandle,
    declaration: &ServiceDecl,
    service_index: usize,
    deferred: &mut Deferred,
    protocol: Option<u32>,
  ) {
    let state = &self.state;
    let client_id = &self.client_id;
    let d0 = declaration.clone();
    let (
      service,
      service_custom_name,
      path_bind,
      hostname_bind,
      hostname_binds,
      max_concurrent,
      tcp,
      backend_healthy,
      backend_probed,
      priority,
      bandwidth_bps,
      public,
      visitor_auth,
      visitor_auth_methods,
      allowed_ips,
      tunnels,
      cache,
      resilience,
      no_capture,
      max_request_body,
      response_timeout,
      webhook_inbox,
      denied,
      scaling,
      connections,
      connections_min,
      connections_max,
      config_notes,
      metrics_labels,
    ) = (
      d0.service,
      d0.service_custom_name,
      d0.path_bind,
      d0.hostname_bind,
      d0.hostname_binds,
      d0.max_concurrent,
      d0.tcp,
      d0.backend_healthy,
      d0.backend_probed,
      d0.priority,
      d0.bandwidth_bps,
      d0.public,
      d0.visitor_auth,
      d0.visitor_auth_methods,
      d0.allowed_ips,
      d0.tunnels,
      d0.cache,
      d0.resilience,
      d0.no_capture,
      d0.max_request_body,
      d0.response_timeout,
      d0.webhook_inbox,
      d0.denied,
      d0.scaling,
      d0.connections,
      d0.connections_min,
      d0.connections_max,
      d0.config_notes,
      d0.metrics_labels,
    );
    let normalized_path = path_bind.and_then(|b| normalize_path_bind(&b));
    let normalized_host = hostname_bind.and_then(|h| normalize_hostname_bind(&h));

    // Declared binds must be permitted by the token used to connect.
    if let Some(p) = normalized_path {
      if handle.perms.path_allowed(&p) {
        handle.service_at_mut(service_index).declared_path = Some(p);
      } else {
        warn!(
          "Client {} declared path bind {} not permitted by its token; ignored",
          client_id, p
        );
      }
    }
    if let Some(h) = normalized_host {
      if handle.perms.hostname_allowed(&h) {
        handle.service_at_mut(service_index).declared_hostname = Some(h);
      } else {
        warn!(
          "Client {} declared hostname bind {} not permitted by its token; ignored",
          client_id, h
        );
      }
    }
    // Additional multi-hostname binds: normalize and admit each
    // that the token permits (others are dropped with a warning).
    if !hostname_binds.is_empty() {
      let mut admitted = Vec::new();
      for raw in &hostname_binds {
        let Some(h) = normalize_hostname_bind(raw) else {
          continue;
        };
        if handle.perms.hostname_allowed(&h) {
          if !admitted.contains(&h) {
            admitted.push(h);
          }
        } else {
          warn!(
            "Client {} declared hostname bind {} not permitted by its token; ignored",
            client_id, h
          );
        }
      }
      handle.service_at_mut(service_index).declared_hostnames = admitted;
    }
    // The concurrency limit, which moves: `adaptive_concurrency` (#65)
    // lowers the announced number when the client's backend falls behind
    // and climbs back when it recovers, and the whole point of
    // announcing it is that the server acts on it.
    if let Some(n) = max_concurrent
      && n > 0
    {
      let service = handle.service_at_mut(service_index);
      match (
        service.inflight_limiter.clone(),
        service.max_concurrent_ceiling,
      ) {
        // First announcement on this connection: it is both the limit
        // and the ceiling the client may climb back to.
        (None, _) => {
          // Clamp to the semaphore's permit ceiling: a client
          // announcing an absurd limit must not panic Semaphore::new
          // (its max is below u32::MAX on 32-bit targets).
          let permits = (n as usize).min(Semaphore::MAX_PERMITS);
          service.max_concurrent = Some(permits as u32);
          service.max_concurrent_ceiling = Some(permits as u32);
          service.inflight_limiter = Some(Arc::new(Semaphore::new(permits)));
          info!(
            "Client {} announced concurrency limit: {}, excess requests will be queued",
            client_id, n
          );
        }
        (Some(limiter), Some(ceiling)) => {
          let enforced = service.max_concurrent.unwrap_or(ceiling);
          // Never above what this connection first asked for. The client
          // lowers a ceiling under pressure and climbs back towards it;
          // it does not raise one, and a peer that says otherwise is not
          // running the feature this is here to serve.
          let want = n.min(ceiling);
          let now = match want.cmp(&enforced) {
            std::cmp::Ordering::Equal => enforced,
            std::cmp::Ordering::Less => {
              // Forgetting takes at most what is free, so a shrink under
              // load takes fewer than it asked for. What it actually
              // took is the new limit: claiming the target would put a
              // number on screen the semaphore is not enforcing, and the
              // rest is taken next heartbeat, when requests have
              // finished and there are permits to take.
              let taken = limiter.forget_permits((enforced - want) as usize) as u32;
              enforced - taken
            }
            std::cmp::Ordering::Greater => {
              // Only ever handing back what was forgotten, which is what
              // keeps the limiter from ending up above the ceiling.
              limiter.add_permits((want - enforced) as usize);
              want
            }
          };
          if now != enforced {
            service.max_concurrent = Some(now);
            info!(
              "Client {} moved its concurrency limit from {} to {} (adaptive concurrency); \
                 dispatch follows it",
              client_id, enforced, now
            );
          }
        }
        // A limiter with no ceiling beside it is not a state anything
        // builds: both are written together above. Left alone rather
        // than guessed at, since inventing a ceiling here would be
        // inventing a limit the client never announced.
        (Some(_), None) => {}
      }
    }
    handle.service_at_mut(service_index).tcp_enabled = tcp;
    if handle.service_at(service_index).cache != cache {
      handle.service_at_mut(service_index).cache = cache;
      if cache {
        info!(
          "Client {} opted into the server-side response cache",
          client_id
        );
      }
    }
    // The service asked to be cached but the server's cache is
    // off, so the opt-in silently does nothing, warn once so the
    // operator can enable APERIO_CACHE (or the owner can drop the
    // flag). Surfaced in the dashboard as a badge too.
    if cache
      && !state.config().cache_enabled
      && !handle.service_at(service_index).cache_ignored_warned
    {
      handle.service_at_mut(service_index).cache_ignored_warned = true;
      warn!(
        "Client {} requested response caching (cache: true) but the server cache is disabled (APERIO_CACHE off); the opt-in is ignored",
        client_id
      );
    }
    if handle.service_at(service_index).max_request_body != max_request_body {
      handle.service_at_mut(service_index).max_request_body = max_request_body;
      if let Some(limit) = max_request_body {
        info!(
          "Client {} declared a request body cap of {} bytes; bigger uploads are rejected with 413 before dispatch",
          client_id, limit
        );
      }
    }
    if handle.service_at(service_index).response_timeout != response_timeout {
      handle.service_at_mut(service_index).response_timeout = response_timeout;
      if let Some(secs) = response_timeout {
        info!(
          "Client {} declared a per-service response timeout of {}s (overrides the global gateway response timeout)",
          client_id, secs
        );
      }
    }
    // Denied-redirect declaration: only well-formed absolute
    // http(s) URLs are honored; anything else stays stealth.
    let denied = denied
      .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
      .filter(|u| url::Url::parse(u).is_ok());
    if handle.service_at(service_index).denied != denied {
      if let Some(url) = &denied {
        info!(
          "Client {} declares a denied-visitor redirect: {}",
          client_id, url
        );
      }
      handle.service_at_mut(service_index).denied = denied;
    }
    // Parallel-connection count and the client's own record of
    // what it resolved differently: display-only, for the
    // dashboard's per-connection config view.
    handle.service_at_mut(service_index).connections = connections;
    handle.service_at_mut(service_index).connections_min = connections_min;
    handle.service_at_mut(service_index).connections_max = connections_max;
    handle.service_at_mut(service_index).capture = !no_capture;
    if handle.service_at(service_index).config_notes != config_notes {
      handle.service_at_mut(service_index).config_notes = config_notes;
    }
    // Sanitized on arrival rather than on the way out: a series, once
    // scraped, is in the metrics backend whatever the server does later.
    let metrics_labels = crate::metrics_labels::sanitize(&metrics_labels);
    if handle.service_at(service_index).metrics_labels != metrics_labels {
      handle.service_at_mut(service_index).metrics_labels = metrics_labels;
    }
    if handle.service_at(service_index).webhook_inbox != webhook_inbox {
      handle.service_at_mut(service_index).webhook_inbox = webhook_inbox;
      if webhook_inbox {
        info!(
          "Client {} opted into the webhook inbox: inbound POSTs are persisted for re-firing",
          client_id
        );
      }
    }
    if handle.service_at(service_index).resilience != resilience {
      handle.service_at_mut(service_index).resilience = resilience;
      if resilience {
        info!(
          "Client {} asked for serve-stale resilience: cached responses outlive its disconnects",
          client_id
        );
      }
    }
    if handle.service_at(service_index).tunnels != tunnels {
      info!(
        "Client {} declares {} bindable tunnel(s)",
        client_id,
        tunnels.len()
      );
      handle.service_at_mut(service_index).tunnels = tunnels;
    }
    // Log backend health transitions reported by the client's
    // own probe; the eligibility filter honours the flag.
    handle.service_at_mut(service_index).backend_probed = backend_probed;
    if handle.service_at(service_index).backend_healthy != backend_healthy {
      handle.service_at_mut(service_index).backend_healthy = backend_healthy;
      if backend_healthy {
        info!(
          "Client {} reports its backend is healthy again; back in routing",
          client_id
        );
      } else {
        warn!(
          "Client {} reports its backend as unhealthy; excluded from routing (tunnel stays connected)",
          client_id
        );
      }
    }
    if handle.service_at(service_index).priority != priority {
      info!(
        "Client {} announced load-balancing priority {}",
        client_id, priority
      );
      handle.service_at_mut(service_index).priority = priority;
    }
    // Announced link capacity feeds the writer task's shaper.
    let announced_bw = bandwidth_bps.unwrap_or(0);
    if handle
      .service_at(service_index)
      .bandwidth_bps
      .swap(announced_bw, Ordering::Relaxed)
      != announced_bw
      && announced_bw > 0
    {
      info!(
        "Client {} announced a bandwidth limit of {} bytes/s; pacing outgoing frames",
        client_id, announced_bw
      );
    }
    if service.is_some() {
      handle.service_at_mut(service_index).service_name = service;
      handle.service_at_mut(service_index).service_custom_name = service_custom_name;
    }
    // Public declaration: honored only when the token permits
    // publishing public services.
    let effective_public = public && handle.perms.allow_public;
    if public
      && !handle.perms.allow_public
      && !handle.service_at(service_index).public_denied_warned
    {
      handle.service_at_mut(service_index).public_denied_warned = true;
      warn!(
        "Client {} declared itself public but its token does not permit publishing public services; keeping the visitor auth gate",
        client_id
      );
    }
    if handle.service_at(service_index).public != effective_public {
      handle.service_at_mut(service_index).public = effective_public;
      if effective_public {
        info!(
          "Client {} serves public traffic: the visitor auth gate is skipped for its routes",
          client_id
        );
      }
    }
    // Client-declared visitor password override: honored only
    // when the token may control the visitor gate (same
    // permission as `public`) and the value is a well-formed
    // "user:password". None/empty clears any previous override.
    let requested_auth = visitor_auth
      .as_deref()
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .map(str::to_string);
    let effective_auth = match requested_auth {
      Some(_) if !handle.perms.allow_public => {
        if !handle.service_at(service_index).visitor_auth_denied_warned {
          handle
            .service_at_mut(service_index)
            .visitor_auth_denied_warned = true;
          warn!(
            "Client {} declared a visitor password but its token does not permit controlling the visitor gate; ignoring it",
            client_id
          );
        }
        None
      }
      Some(ref creds) if !crate::routing::valid_visitor_creds(creds) => {
        if !handle.service_at(service_index).visitor_auth_denied_warned {
          handle
            .service_at_mut(service_index)
            .visitor_auth_denied_warned = true;
          warn!(
            "Client {} declared an invalid visitor password (expected user:password); ignoring it",
            client_id
          );
        }
        None
      }
      other => other,
    };
    // The full policy, when the client sent one. Same permission as the
    // scalar and as `public`, and the same silent-drop rule: a method
    // this build does not know, or one a client has no business
    // declaring, is dropped rather than guessed at, and the methods
    // beside it stay in force.
    let declared_policy = visitor_auth_methods.as_ref().and_then(|specs| {
      if !handle.perms.allow_public {
        // Said out loud, like the scalar case beside it. The handshake
        // already refused this connection the right to declare a gate (it
        // announced an empty list), so a client that is up to date is not
        // serving this route at all; a client that sent one anyway is
        // reading an announcement it should have refused on, and the
        // reason belongs in the operator's log either way.
        if !handle.service_at(service_index).visitor_auth_denied_warned {
          handle.service_at_mut(service_index).visitor_auth_denied_warned = true;
          warn!(
            "Client {} declared a visitor-auth policy but its token does not permit controlling the visitor gate; ignoring it",
            client_id
          );
        }
        return None;
      }
      let usable: Vec<aperio_config::AuthMethodSpec> = specs
        .iter()
        .filter(|spec| {
          CLIENT_DECLARABLE_METHODS.contains(&spec.method.trim().to_ascii_lowercase().as_str())
        })
        .cloned()
        .collect();
      if usable.len() != specs.len() && !handle.service_at(service_index).visitor_auth_denied_warned {
        handle.service_at_mut(service_index).visitor_auth_denied_warned = true;
        warn!(
          "Client {} declared a visitor-auth method this server does not accept from a client; ignoring it",
          client_id
        );
      }
      (!usable.is_empty())
        .then(|| {
          // `true`: these specs arrived over the tunnel, so a `jwks_url`
          // among them is a destination a client chose for this server to
          // fetch, and is fenced as one.
          crate::visitor_auth::Policy::compile_from(
            &aperio_config::AuthSetting::Any(usable),
            true,
          )
        })
        .filter(|p| p.gates() || p.admits_everyone())
    });
    if handle.service_at(service_index).visitor_auth_policy != declared_policy {
      if let Some(ref p) = declared_policy {
        info!(
          "Client {} gates its service with method(s): {}",
          client_id,
          p.method_names().join(", ")
        );
      }
      handle.service_at_mut(service_index).visitor_auth_policy = declared_policy;
    }
    if handle.service_at(service_index).visitor_auth != effective_auth {
      let now_set = effective_auth.is_some();
      handle.service_at_mut(service_index).visitor_auth = effective_auth;
      if now_set {
        info!(
          "Client {} gates its service behind a client-set visitor login",
          client_id
        );
      }
    }
    // The service nothing gates. Said once per connection, and only
    // while the server is still open by default, because that is the
    // configuration where an ungated service is reachable by anyone and
    // nothing in the file says so: it is open because nothing closed it.
    // Under `default_access: deny` this is not a warning but the stated
    // policy, and the route is simply refused (planned_features #108).
    if !handle.service_at(service_index).ungated_warned
      && handle.service_at(service_index).visitor_auth.is_none()
      && handle
        .service_at(service_index)
        .visitor_auth_policy
        .is_none()
      && !handle.service_at(service_index).public
      && !state.config().visitor_auth.gates()
      && state.oidc.is_none()
    {
      handle.service_at_mut(service_index).ungated_warned = true;
      if state.config().default_access == crate::settings::DefaultAccess::Deny {
        // The one message that turns "the site went dark after we
        // upgraded" from an afternoon into a minute. It fires where the
        // cause is, on the connection whose service is being refused,
        // and it names the line to write rather than the state it found.
        warn!(
          "Client {} declares no gate and is not declared open, so its traffic is refused (`default_access` is `deny`). Write `public: true` on the service if it is meant to be reachable by anyone, or give it an `auth:`.",
          client_id
        );
      } else {
        warn!(
          "Client {} serves traffic that nothing gates: this server has no `auth:` and the service declares neither a gate nor `public: true`, so it is reachable by anyone. Declare it (`auth:` / `public: true`), or set `default_access: deny` to refuse what nothing declares.",
          client_id
        );
      }
    }
    // Client-declared visitor IP allowlist. Purely restrictive
    // (it can only narrow who reaches the client), so no token
    // permission is required; invalid entries are dropped so a
    // typo can never widen access.
    let mut effective_ips: Vec<String> = allowed_ips
      .iter()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty())
      .collect();
    let before = effective_ips.len();
    effective_ips.retain(|e| crate::auth::valid_ip_entry(e));
    if effective_ips.len() != before && !handle.service_at(service_index).allowed_ips_invalid_warned
    {
      handle
        .service_at_mut(service_index)
        .allowed_ips_invalid_warned = true;
      warn!(
        "Client {} declared allowed_ips with invalid entries; dropping them",
        client_id
      );
    }
    if handle.service_at(service_index).allowed_ips != effective_ips {
      if !effective_ips.is_empty() {
        info!(
          "Client {} restricts visitors to {:?}",
          client_id, effective_ips
        );
      }
      handle.service_at_mut(service_index).allowed_ips = effective_ips;
    }
    // Warn once per change, not on every heartbeat.
    if protocol.is_some() && handle.client_protocol != protocol {
      handle.client_protocol = protocol;
      if let Some(p) = protocol
        && p != PROTOCOL_VERSION
      {
        warn!(
          "Client {} speaks tunnel protocol v{} but this server speaks v{}; \
                     update the older side to avoid subtle incompatibilities",
          client_id, p, PROTOCOL_VERSION
        );
      }
    }
    // Dynamic-token clients are subject to token pinning.
    if let Some(id) = handle.perms.token_id.clone() {
      deferred.pin = Some((
        id,
        handle.perms.token_name.clone().unwrap_or_default(),
        handle.perms.org_id.clone(),
      ));
    }
    if let Some(ref declared_scaling) = scaling {
      deferred.scaling = Some((
        declared_scaling.clone(),
        // The declaring service's own binds, not the connection's
        // union: an autoscaling record is armed per hostname of the
        // service that asked for it.
        handle
          .service_at(service_index)
          .effective_hostnames()
          .into_iter()
          .cloned()
          .collect::<Vec<String>>(),
        handle
          .service_at(service_index)
          .effective_path_bind()
          .cloned(),
        handle.perms.org_id.clone(),
        handle.perms.token_id.clone(),
        service_index,
      ));
    }
  }
}
