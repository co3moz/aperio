//! The heartbeat, which is also the full service announcement.
//!
//! One function, deliberately whole. It is long because a Ping *is* long: it
//! declares everything a connection serves, and every field has to be admitted
//! or refused against what the token permits before any of it takes effect.
//! Cutting it up would mean carrying the half-applied state between the pieces,
//! which is the shape this file has spent three fixes getting away from.

use axum::extract::ws::Message;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

use super::*;
use crate::protocol::TunnelMessage;

impl ConnCtx {
  /// Handles the heartbeat and full service announcement.
  pub(super) async fn on_ping(&self, msg: TunnelMessage) -> bool {
    let TunnelMessage::Ping {
      services,
      client_id: cid,
      timestamp,
      visitor_auth_methods,
      path_bind,
      hostname_bind,
      hostname_binds,
      max_concurrent,
      tcp,
      version,
      protocol,
      backend_healthy,
      backend_probed,
      priority,
      bandwidth_bps,
      service,
      service_custom_name,
      public,
      visitor_auth,
      allowed_ips,
      tunnels,
      cache,
      resilience,
      no_capture,
      max_request_body,
      response_timeout,
      client_key,
      webhook_inbox,
      denied,
      scaling,
      connections,
      connections_min,
      connections_max,
      config_notes,
      metrics_labels,
      drain_secs,
      cpu_percent,
      rss_bytes,
      rtt_ms,
      jitter_ms,
      reconnects,
    } = msg
    else {
      return true;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    debug!("Heartbeat from client {}: {}", cid, timestamp);

    // The one place that says what a Ping describes (#46). Today it is always
    // exactly one service, whichever spelling arrived, and the rest of this
    // function reads that one. When the server learns to serve several, this
    // is the line that stops being a `[0]`.
    //
    // A list of more than one is refused rather than half-served. The server
    // cannot route to a second service yet, so accepting the declaration
    // would be a connection that establishes and then serves less than it was
    // told to, which is the failure mode the whole protocol-gate work exists
    // to prevent.
    // A list of several is served now. What used to be refused here, and the
    // reason the refusal stood so long, was that routing, ejection and the
    // per-service state all keyed on the connection; each of those was moved
    // onto the service before this line was allowed to change.
    //
    // An empty list is still nothing to serve. A client saying so means it
    // has withdrawn everything, which is a disconnect written the long way,
    // and treating it as "no list" would silently keep serving what it just
    // retired.
    let declared_services = match services {
      Some(list) if list.is_empty() => {
        warn!(
          "Client {} declared an empty service list; refusing rather than going on serving \
           what it just said it no longer offers",
          cid
        );
        return false;
      }
      // Refused before anything is built from it, which is the point of doing
      // it here rather than after the names are collected: everything below
      // this line is work proportional to the length, some of it quadratic,
      // all of it under the `clients` write lock, and the allocation at the
      // end is one `ServiceState` per entry.
      //
      // A 20 MB frame (the default `max_body_size` doubled) holds several
      // million `{}` entries, so an authenticated client could hold the write
      // lock through a quadratic pass over them, blocking every other client
      // and every dashboard request, and then ask for the memory. A tunnel
      // token is not supposed to be able to take the front door down.
      Some(list) if list.len() > MAX_DECLARED_SERVICES => {
        warn!(
          "Client {} declared {} services on one connection; the ceiling is {}. Refusing the \
           connection rather than serving part of what it asked for.",
          cid,
          list.len(),
          MAX_DECLARED_SERVICES
        );
        return false;
      }
      Some(list) => list,
      None => Vec::new(),
    };
    // The names, taken before the list is consumed below: they are what says
    // which existing service each declaration is about.
    let declared_names: Vec<Option<String>> = declared_services
      .iter()
      .map(|d| d.service.clone())
      .collect();

    // The list is authoritative when it is there, which is the promise the
    // protocol makes and the reason the two spellings can never half-agree:
    // a client that describes its work as a list has said everything about
    // that service inside the entry, so reading a singular field alongside
    // it would let a value the client did not write win over one it did.
    // Absent, every singular field stands exactly as before.
    //
    // Written as one binding rather than a field-by-field merge on purpose.
    // A merge is where a field gets forgotten, and forgetting one here is
    // silent: the entry's value is dropped and the singular default takes
    // its place, so the service comes up with a setting the client did not
    // ask for and no error anywhere.
    // The declarations this Ping is about, as `ServiceDecl` values rather
    // than as a tuple of loose locals.
    //
    // A client that sent no list still described exactly one service, in the
    // singular fields, so it becomes one entry here. That is what lets the
    // rest of this function talk about *a declaration* without caring which
    // spelling it arrived in, and what lets it eventually talk about several.
    let declarations: Vec<crate::protocol::ServiceDecl> = if declared_services.is_empty() {
      vec![crate::protocol::ServiceDecl {
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
      }]
    } else {
      declared_services
    };

    // Update client's reported binds and heartbeat time. Only the
    // server-assigned connection ID is trusted for state updates;
    // the client-declared `cid` is ignored to prevent a client from
    // mutating another connection's state.
    // Token pinning context captured under the clients lock and used
    // after it is released: (token id, token name, org).
    let mut pin_ctx: Option<(String, String, Option<String>)> = None;
    // Set under the clients lock, acted on after it: a connection
    // beyond what the token is allowed for this service.
    let mut over_ceiling = false;
    let mut ceiling_ctx: Option<(Option<String>, u32)> = None;
    // Bind context for the autoscaling upsert, captured under the
    // clients lock and used after it is released (the scaling store
    // must never be locked while the clients map is).
    let mut scaling_ctx: Option<ScalingBindCtx> = None;
    {
      let mut clients = state.clients.write().await;
      if let Some(handle) = clients.get_mut(client_id) {
        // Which of this connection's services the declaration is about.
        //
        // Without a list there is nothing to match: the singular fields have
        // always described the connection's one service, and still do. With
        // one, the answer comes from `match_declarations` rather than from
        // position, so a client that names its services keeps each one's
        // ejection state, warn-once flags and counters across a heartbeat
        // even if it reorders them.
        // Which of this connection's services each declaration is about,
        // decided once for the whole Ping before any of them is applied.
        let indexes: Vec<usize> = if declared_names.is_empty() {
          vec![0]
        } else {
          match crate::state::match_declarations(&handle.services, &declared_names) {
            // Unreachable while the length refusal above stands: two
            // declarations are needed to repeat a name, and a list of two is
            // turned away before the names are read. Kept, and said out loud
            // rather than left to be found: it is what stops a duplicate
            // becoming a silent merge on the day the length refusal goes, and
            // that day is the point of this entry.
            Err(name) => {
              warn!(
                "Client {} declared two services both named '{}'; refusing the connection \
                 rather than guessing which one it meant",
                cid, name
              );
              return false;
            }
            // A declaration this connection does not carry means the client
            // changed what the connection serves while it was open. Refused
            // for the same reason a second service is: the server would have
            // to decide what becomes of the old service's ejection state and
            // statistics, and every answer to that is a guess. Reconnecting
            // is unambiguous and is what the client already does on a config
            // change.
            // Reconcile: the connection ends up carrying exactly what the
            // Ping declared, in the order it declared it. A service that
            // matched keeps everything the wire does not carry, which is the
            // point of matching by name; one this connection has just gained
            // starts fresh; one no longer declared is dropped by not being
            // moved into the new list.
            //
            // Rebuilt rather than patched in place because removing an entry
            // shifts every index after it, and the indexes are what the rest
            // of this function writes through.
            Ok(matched) => {
              let pacer_cell = handle
                .services
                .first()
                .map(|s| s.bandwidth_bps.clone())
                .unwrap_or_default();
              // What the *token* granted this connection, and the random
              // subdomain the server handed it. Both are settled at connect
              // time onto the one service a connection starts with, and both
              // belong to the connection rather than to that service, so a
              // service declared later has to be given them too. Without this
              // a multiplexed client's second service came up with no assigned
              // binds at all: a token whose grant is what makes the route
              // reachable served its first service and silently not the rest.
              let (granted_hostnames, random_hostname) = handle
                .services
                .first()
                .map(|s| (s.assigned_hostnames.clone(), s.random_hostname.clone()))
                .unwrap_or_default();
              let mut old: Vec<Option<crate::state::ServiceState>> =
                handle.services.drain(..).map(Some).collect();
              let mut fresh = Vec::with_capacity(matched.len());
              for m in &matched {
                let carried = m
                  .and_then(|i| old.get_mut(i).and_then(Option::take))
                  .unwrap_or_else(|| {
                    // The connection's own cell, so a service added mid-flight
                    // announces into the one the writer actually reads, and
                    // the connection's assigned binds, which are the token's
                    // and the server's rather than any one service's.
                    let mut service =
                      crate::state::ServiceState::newly_declared(pacer_cell.clone());
                    service.assigned_hostnames = granted_hostnames.clone();
                    service.random_hostname = random_hostname.clone();
                    service
                  });
                fresh.push(carried);
              }
              let retired = old.iter().filter(|s| s.is_some()).count();
              if retired > 0 {
                info!(
                  "Client {} stopped declaring {} service(s); they leave routing",
                  cid, retired
                );
              }
              handle.services = fresh;
              (0..matched.len()).collect()
            }
          }
        };

        // One pass per declaration. The body below describes a single
        // service, which is what it always did; what changed is that it now
        // says *which*, and that saying it more than once is a matter of
        // going round again rather than of rewriting any of it.
        for (declaration, service_index) in declarations.iter().zip(indexes) {
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
          // The declared id is `<base>-<service>` for the first
          // connection and `<base>-<service>-c<N>` for the rest, so it
          // names both the service and this connection's place in its
          // fan. Recorded here rather than trusted for anything else:
          // it is what lets the ceiling below be about *one service*
          // instead of the whole process.
          handle.declared_client_id = Some(cid.clone());
          // Counted after this block: `handle` is a mutable borrow of
          // the map the count has to walk.
          ceiling_ctx = Some((
            handle.instance_group.clone(),
            handle.perms.connection_ceiling(server_max_connections),
          ));
          if handle.service_at(service_index).config_notes != config_notes {
            handle.service_at_mut(service_index).config_notes = config_notes;
          }
          // Sanitized on arrival rather than on the way out: a series, once
          // scraped, is in the metrics backend whatever the server does later.
          let metrics_labels = crate::metrics_labels::sanitize(&metrics_labels);
          if handle.service_at(service_index).metrics_labels != metrics_labels {
            handle.service_at_mut(service_index).metrics_labels = metrics_labels;
          }
          handle.drain_secs = drain_secs;
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
          // Self-reported client health. Stored as sent, including absences: a
          // client that stops reporting a figure (an older build, or a platform
          // where it cannot be read) should show nothing rather than the last
          // value it happened to send, which would age silently.
          handle.cpu_percent = cpu_percent;
          handle.rss_bytes = rss_bytes;
          handle.rtt_ms = rtt_ms;
          handle.jitter_ms = jitter_ms;
          handle.reconnects = reconnects;
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
          // The self-reported instance ID is remembered (first value
          // wins) so failover `wait` mode can recognize this client
          // process when it reconnects under a new connection ID.
          if handle.reported_instance_id.is_none() && !cid.is_empty() {
            handle.reported_instance_id = Some(cid.clone());
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
          // The client's build, which belongs to the connection rather than
          // to any service, so it is read rather than consumed here.
          if let Some(v) = version.as_ref() {
            handle.client_version = Some(v.clone());
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
          if effective_ips.len() != before
            && !handle.service_at(service_index).allowed_ips_invalid_warned
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
          handle.last_ping_at = Some(Instant::now());
          // Dynamic-token clients are subject to token pinning.
          if let Some(id) = handle.perms.token_id.clone() {
            pin_ctx = Some((
              id,
              handle.perms.token_name.clone().unwrap_or_default(),
              handle.perms.org_id.clone(),
            ));
          }
          if let Some(ref declared_scaling) = scaling {
            scaling_ctx = Some((
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
      if let Some((group, ceiling)) = ceiling_ctx {
        over_ceiling =
          service_connection_over_ceiling(&clients, client_id, group.as_deref(), &cid, ceiling);
      }
    }

    // A connection past what this token may hold for one service.
    // Refused here rather than at the handshake because the service
    // it belongs to is only known once the client says so, and said
    // out loud rather than dropped silently: a client that opened
    // more than it may is a config to fix, not a mystery. A current
    // client never reaches this, it reads the ceiling from the
    // handshake header and opens that many.
    if over_ceiling {
      let ceiling = perms.connection_ceiling(server_max_connections);
      warn!(
        "Client {} ({}) opened more parallel connections than permitted for one \
                   service; closing this one. Ceiling {} ({})",
        cid,
        client_ip,
        ceiling,
        match perms.max_connections {
          Some(_) => "the token's own, at or below the server's",
          None => "the server's max_connections_per_service",
        }
      );
      return false;
    }

    // Trust-on-first-use token pinning (APERIO_TOKEN_PINNING): pin the
    // first device key seen for a dynamic token and reject a later
    // connection that presents a different (or missing) key. Done
    // outside the clients lock so we never hold two store locks.
    if state.config().token_pinning
      && let Some((token_id, token_name, org)) = pin_ctx
    {
      let verdict = {
        let mut store = state.token_store.lock().await;
        match client_key.as_deref() {
          Some(key) => store.pin_key(&token_id, key),
          // No key announced while pinning is required: reject (fail
          // closed). A key-less client can never satisfy pinning, so
          // enabling APERIO_TOKEN_PINNING requires every client to
          // carry a device key (APERIO_DEVICE_KEY[_FILE]).
          None => Ok(crate::store::tokens::PinOutcome::Mismatch),
        }
      };
      match verdict {
        Ok(crate::store::tokens::PinOutcome::Mismatch) => {
          warn!(
            "Token pinning: client {} presented token '{}' without a matching device key, rejecting the connection",
            client_id, token_name
          );
          state
            .audit_in(
              "token_pin_mismatch",
              &token_name,
              client_ip,
              org.clone(),
              &format!("token={token_name} client={client_id}"),
            )
            .await;
          state
            .emit_event_in(
              "token_pin_mismatch",
              serde_json::json!({"token": token_name, "client_id": client_id}),
              org,
            )
            .await;
          return false;
        }
        Ok(crate::store::tokens::PinOutcome::Pinned) => {
          info!(
            "Token pinning: pinned token '{}' to the connecting device",
            token_name
          );
        }
        // The pin was made and rolled back because it could not be saved.
        // **Refuse**, for the reason pinning exists: a pin that is not written
        // down does not bind the next connection to this device, so admitting
        // this one would leave the operator with a control that reports itself
        // enabled and holds nothing. The same argument as an auth gate that
        // opens when its check is unreachable.
        Err(crate::store::tokens::NotWritten::NotPersisted) => {
          warn!(
            "Token pinning: could not record the pin for token '{}', refusing the connection",
            token_name
          );
          return false;
        }
        // Match, or a token that disappeared between authorization and here.
        // Both leave the store as it was, and neither is this block's decision
        // to make.
        Ok(crate::store::tokens::PinOutcome::Match)
        | Err(crate::store::tokens::NotWritten::NoSuchRecord) => {}
      }
    }

    // Autoscaling: arm (or refresh) one record per hostname this
    // client serves. The record deliberately outlives the connection,
    // which is the whole point of `min: 0`: the server must be able
    // to call the endpoint when nothing is running. A fleet of
    // identical replicas converges on one record per bind, because
    // the store dedupes by a hash of the declaration.
    // The declaration travels with the context it was captured from. Reading
    // it back off the first entry, as this did, silently armed nothing when a
    // *later* service was the one that asked for scaling: the context was
    // set, the declaration was not, and the tuple pattern simply failed.
    if let (true, Some((decl, hostnames, path, org, token_id, scaling_service))) =
      (state.config().scaling_enabled, scaling_ctx)
    {
      for hostname in hostnames {
        let record =
          crate::api::scaling::record_from_decl(&decl, org.clone(), &hostname, path.as_deref());
        let record = match record {
          Ok(record) => record,
          Err(e) => {
            let warned = {
              let mut clients = state.clients.write().await;
              // `get` rather than `service_at`, which indexes and would panic.
              // The index was decided under a lock this has since released and
              // re-taken, and while nothing can currently shrink the list in
              // between (a connection's Pings are handled one at a time, and
              // nothing else writes `services`), that is an invariant held
              // across a hundred lines and a lock boundary. The cost of being
              // wrong is the whole process; the cost of asking is one `match`,
              // and a warn-once flag that could not be set is worth exactly a
              // suppressed log line.
              match clients
                .get_mut(client_id)
                .and_then(|handle| handle.services.get_mut(scaling_service))
              {
                Some(service) => {
                  let already = service.scaling_invalid_warned;
                  service.scaling_invalid_warned = true;
                  already
                }
                None => true,
              }
            };
            if !warned {
              warn!(
                "Client {} declared an invalid scaling block: {}",
                client_id, e
              );
            }
            break;
          }
        };
        let id = record.id.clone();
        let outcome = {
          let mut store = state.scaling_store.lock().await;
          store.upsert(
            record,
            token_id.as_deref(),
            crate::store::tokens::now_secs(),
          )
        };
        match outcome {
          crate::store::scaling::Upsert::Unchanged => {}
          other => {
            // A changed declaration re-arms a record the breaker may
            // have disarmed: the operator just told us something new.
            state.scaling_runtime.lock().await.rearm(&id);
            info!(
              "Autoscaling record {} for {} ({:?})",
              if other == crate::store::scaling::Upsert::Created {
                "armed"
              } else {
                "updated"
              },
              hostname,
              other
            );
          }
        }
      }
    }
    let pong = TunnelMessage::Pong {
      timestamp,
      version: Some(env!("CARGO_PKG_VERSION").to_string()),
      protocol: Some(PROTOCOL_VERSION),
    };
    if let Ok(pong_str) = serde_json::to_string(&pong) {
      let _ = tx_write.send(Message::Text(pong_str.into())).await;
    }

    true
  }
}

#[cfg(test)]
#[path = "ping_tests.rs"]
mod tests;
