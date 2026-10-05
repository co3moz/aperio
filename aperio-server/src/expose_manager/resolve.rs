//! Shared eligibility and deterministic resolution for managed expose views
//! and relay opens. Raw tunnels are process-wide in the current client schema;
//! name an enabled service on multiplexed v8 clients when opening the stream.

use super::{AppState, Arc, ExposeTargetState, ManagedRule};
use axum::extract::ws::Message;
use tokio::sync::mpsc;

#[derive(Clone)]
pub(crate) struct Target {
  pub client_id: String,
  pub tunnel: String,
  pub transport: super::ExposeProtocol,
  pub org_id: Option<String>,
  pub target: String,
  pub protocol: u32,
  pub service: Option<String>,
  pub tx: mpsc::Sender<Message>,
}

pub(crate) async fn target(
  state: &Arc<AppState>,
  rule: &ManagedRule,
) -> Result<Target, ExposeTargetState> {
  let resource = &rule.resource;
  let org = if resource.spec.org_id == "master" {
    None
  } else {
    Some(resource.spec.org_id.as_str())
  };
  if let Some(org) = org.filter(|_| rule.legacy.is_none())
    && state.org_store.lock().await.find(org).is_none()
  {
    return Err(ExposeTargetState::Unavailable);
  }
  let legacy_org = match rule.legacy.as_ref().and_then(|r| r.explicit_org()) {
    Some(name) => Some(
      crate::tunnel::registry::org_id_for_name(state, name)
        .await
        .map_err(|_| ExposeTargetState::Waiting)?,
    ),
    None => None,
  };
  let clients = state.clients.read().await;
  let mut ordered: Vec<_> = clients.iter().collect();
  ordered.sort_by(|a, b| a.0.cmp(b.0));
  let mut failure = ExposeTargetState::Waiting;
  for (id, client) in ordered {
    for declaration in client.tunnels() {
      let matched = if let Some(legacy) = &rule.legacy {
        match &legacy.tunnel {
          Some(name) => {
            let (_, bare) = aperio_config::expose::split_qualified(name);
            crate::tunnel::registry::name_of(declaration) == bare
              && match (&legacy_org, legacy.token.as_deref().map(str::trim)) {
                (Some(org), Some(token)) => {
                  client.perms.org_id == *org && client.perms.token_name.as_deref() == Some(token)
                }
                (Some(org), None) => client.perms.org_id == *org,
                (None, token) => client.perms.token_name.as_deref() == token,
              }
          }
          None => declaration.expose.as_deref() == legacy.key.as_deref(),
        }
      } else {
        client.perms.org_id.as_deref() == org
          && crate::tunnel::registry::name_of(declaration) == resource.spec.tunnel
      };
      if !matched {
        continue;
      }
      if declaration.encrypt
        || !aperio_config::protocol_serves(
          &declaration.protocol,
          resource.spec.listener.protocol.as_str(),
        )
      {
        failure = ExposeTargetState::Incompatible;
        continue;
      }
      if !client.serves_process_scoped(state.config().client_down_threshold) {
        failure = ExposeTargetState::Unavailable;
        continue;
      }
      let service = if client.services.len() > 1 {
        client
          .services
          .iter()
          .find(|s| s.admin_enabled)
          .and_then(|s| s.service_name.clone())
      } else {
        None
      };
      return Ok(Target {
        client_id: id.clone(),
        tunnel: crate::tunnel::registry::name_of(declaration).to_string(),
        transport: resource.spec.listener.protocol,
        org_id: client.perms.org_id.clone(),
        tx: client.tx.clone(),
        target: declaration.target.clone(),
        protocol: client.client_protocol.unwrap_or(1),
        service,
      });
    }
  }
  Err(failure)
}

pub(crate) async fn still_serving(state: &Arc<AppState>, target: &Target) -> bool {
  let clients = state.clients.read().await;
  clients.get(&target.client_id).is_some_and(|c| {
    c.tx.same_channel(&target.tx)
      && c.perms.org_id == target.org_id
      && c.serves_process_scoped(state.config().client_down_threshold)
      && c.tunnels().iter().any(|d| {
        !d.encrypt
          && d.target == target.target
          && crate::tunnel::registry::name_of(d) == target.tunnel
          && aperio_config::protocol_serves(&d.protocol, target.transport.as_str())
      })
  })
}
