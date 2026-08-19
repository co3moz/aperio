//! The heartbeat, which is also the full service announcement.
//!
//! One function, deliberately whole. It is long because a Ping *is* long: it
//! declares everything a connection serves, and every field has to be admitted
//! or refused against what the token permits before any of it takes effect.
//! Cutting it up would mean carrying the half-applied state between the pieces,
//! which is the shape this file has spent three fixes getting away from.

use axum::extract::ws::Message;
use tracing::{debug, info, warn};

use super::*;
use crate::protocol::TunnelMessage;

impl ConnCtx {
  /// Handles the heartbeat and full service announcement.
  pub(super) async fn on_ping(&self, msg: TunnelMessage) -> bool {
    let TunnelMessage::Ping {
      services,
      client_id: cid,
      name: declared_name,
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
        // The singular-field compatibility path: a client old enough to send
        // no service list is old enough not to know about this at all.
        server_side_target: None,
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
    // Set under the clients lock, acted on after it: a connection
    // beyond what the token is allowed for this service.
    let mut over_ceiling = false;
    // What the declarations decide but cannot do under the lock.
    let mut deferred = declare::Deferred::default();
    let mut ceiling_ctx: Option<(Option<String>, u32)> = None;
    // Bind context for the autoscaling upsert, captured under the
    // clients lock and used after it is released (the scaling store
    // must never be locked while the clients map is).
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
        // Connection-level state, written once. These are the *socket's*
        // figures rather than any service's, and they used to be assigned
        // inside the loop below, once per declaration with the same value
        // each time. Harmless while a connection carried one service, and
        // still harmless with several since the writes are idempotent, but it
        // is what kept the loop body from being liftable: a per-service pass
        // that also writes per-connection fields needs both scopes.
        // The declared id is `<base>-<service>` for the first
        // connection and `<base>-<service>-c<N>` for the rest, so it
        // names both the service and this connection's place in its
        // fan. Recorded here rather than trusted for anything else:
        // it is what lets the ceiling below be about *one service*
        // instead of the whole process.
        handle.declared_client_id = Some(cid.clone());
        // What the operator calls this client. Validated here rather than
        // trusted: it reaches the dashboard, the logs and an audit trail, and
        // it arrives on every heartbeat from a party the server does not
        // trust for anything else. An unusable one is dropped rather than
        // shown, because a name is only worth having if it is the name.
        let named = declared_name
          .as_deref()
          .map(str::trim)
          .filter(|n| aperio_config::validate_name("client", n).is_ok())
          .map(str::to_string);
        // Said once, when it first arrives or changes, rather than on every
        // heartbeat. The connect line above cannot carry it: a name arrives
        // on the first Ping, and until then all anyone has is the id.
        if named != handle.declared_name
          && let Some(ref n) = named
        {
          info!("Client {} calls itself {}", client_id, n);
        }
        handle.declared_name = named;
        // Counted after this block: `handle` is a mutable borrow of
        // the map the count has to walk.
        ceiling_ctx = Some((
          handle.instance_group.clone(),
          handle.perms.connection_ceiling(server_max_connections),
        ));
        handle.drain_secs = drain_secs;
        // Self-reported client health. Stored as sent, including absences: a
        // client that stops reporting a figure (an older build, or a platform
        // where it cannot be read) should show nothing rather than the last
        // value it happened to send, which would age silently.
        handle.cpu_percent = cpu_percent;
        handle.rss_bytes = rss_bytes;
        handle.rtt_ms = rtt_ms;
        handle.jitter_ms = jitter_ms;
        handle.reconnects = reconnects;
        // The self-reported instance ID is remembered (first value
        // wins) so failover `wait` mode can recognize this client
        // process when it reconnects under a new connection ID.
        if handle.reported_instance_id.is_none() && !cid.is_empty() {
          handle.reported_instance_id = Some(cid.clone());
        }
        // The client's build, which belongs to the connection rather than
        // to any service, so it is read rather than consumed here.
        if let Some(v) = version.as_ref() {
          handle.client_version = Some(v.clone());
        }
        handle.last_ping_at = Some(Instant::now());

        for (declaration, service_index) in declarations.iter().zip(indexes) {
          self.apply_declaration(handle, declaration, service_index, &mut deferred, protocol);
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
      && let Some((token_id, token_name, org)) = deferred.pin
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
      (state.config().scaling_enabled, deferred.scaling)
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
