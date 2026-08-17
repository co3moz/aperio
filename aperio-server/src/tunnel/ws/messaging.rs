//! Messages between clients, as they arrive on a tunnel connection: what this
//! process subscribes to, what it acknowledges, and what it may publish.

use axum::extract::ws::Message;
use tracing::warn;

use super::*;
use crate::protocol::TunnelMessage;

impl ConnCtx {
  /// Handles topic filters this process wants; refusals are reported back.
  pub(super) async fn on_subscribe(&self, msg: TunnelMessage) {
    let TunnelMessage::Subscribe { topics } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    let refused = crate::tunnel::pubsub::set_subscriptions(state, client_id, topics, true).await;
    for (topic, why) in refused {
      warn!("Client {client_id} cannot subscribe to '{topic}': {why}");
      // Tell the client too. A refusal only the server can see
      // leaves the other operator watching a subscription that
      // never delivers, which looks exactly like a topic nobody
      // publishes on.
      let notice = TunnelMessage::SubscribeRefused { topic, reason: why };
      if let Ok(json) = serde_json::to_string(&notice) {
        let clients = state.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
          let _ = handle.tx.try_send(Message::Text(json.into()));
        }
      }
    }
  }

  /// Handles topic filters this process no longer wants.
  pub(super) async fn on_unsubscribe(&self, msg: TunnelMessage) {
    let TunnelMessage::Unsubscribe { topics } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    crate::tunnel::pubsub::set_subscriptions(state, client_id, topics, false).await;
  }

  /// Handles a QoS 1 delivery acknowledged.
  pub(super) async fn on_publish_ack(&self, msg: TunnelMessage) {
    let TunnelMessage::PublishAck { id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    crate::tunnel::pubsub::acknowledge(state, client_id, &id).await;
  }

  /// Handles a message published by this client into its organization.
  pub(super) async fn on_publish(&self, msg: TunnelMessage) {
    let TunnelMessage::Publish {
      topic,
      payload,
      qos,
      ..
    } = msg
    else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    // Every refusal is reported back, not only logged: the local
    // application that sent this was told the message was accepted
    // the moment it reached the client, so the server's log is the
    // only other place the truth exists.
    let refusal = if !crate::tunnel::pubsub::may_use_topic(perms, &topic) {
      Some("the token does not carry this topic; add it to the token's topics".to_string())
    } else {
      use base64::prelude::*;
      match BASE64_STANDARD.decode(&payload) {
        Err(e) => Some(format!("the payload is not valid Base64: {e}")),
        Ok(bytes) => crate::tunnel::pubsub::publish(
          state,
          perms.org_id.as_deref(),
          &topic,
          &bytes,
          crate::tunnel::pubsub::Publisher::Client(client_id),
          qos,
        )
        .await
        .err(),
      }
    };
    if let Some(why) = refusal {
      warn!("Client {client_id} cannot publish to '{topic}': {why}");
      let notice = TunnelMessage::PublishRefused { topic, reason: why };
      if let Ok(json) = serde_json::to_string(&notice) {
        let clients = state.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
          let _ = handle.tx.try_send(Message::Text(json.into()));
        }
      }
    }
  }
}
