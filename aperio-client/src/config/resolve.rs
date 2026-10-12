//! The layers merged into the settings the client runs on: `~/.aperio.yaml`,
//! then `./aperio.yaml`, then the environment, then the command line, by the
//! one rule `aperio_config::layers` describes.
//!
//! What is left here is turning the merged document into the shape the rest
//! of the client reads: defaults applied, the process-wide values parsed, and
//! the services listed in the order they will run.

use super::*;
use aperio_config::layers::{self, Layer};
use serde_yaml::Mapping;

/// Resolves every client setting from the layers. Called at startup and again
/// on config hot-reload (with the freshly read files).
pub(crate) fn resolve_settings(
  cli: &CliArgs,
  home: &Mapping,
  local: &Mapping,
) -> Result<ClientSettings, String> {
  let env = layers::env_overlay(|name| std::env::var(name).ok())?;
  let assembled = layers::assemble(
    vec![(Layer::Home, home.clone()), (Layer::Local, local.clone())],
    vec![(Layer::Env, env), (Layer::Cli, cli.overlay()?)],
  )?;
  settings_from(assembled)
}

/// The merged configuration in the shape the client runs on.
pub(crate) fn settings_from(assembled: layers::Assembled) -> Result<ClientSettings, String> {
  let layers::Assembled {
    config,
    service,
    sources,
  } = assembled;
  let server = config.server();
  let nonempty = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

  // One service on the command line or in the environment runs on its own,
  // the way it always has; a file's services are what runs otherwise.
  let file_services = config.services.clone().unwrap_or_default();
  let (services, single) = match service {
    Some(entry) => {
      if !file_services.is_empty() {
        tracing::warn!(
          "A service was given on the command line or in the environment; ignoring the {} \
           entry/entries of the services: map",
          file_services.len()
        );
      }
      (vec![Service { name: None, entry }], true)
    }
    None => (
      file_services
        .into_iter()
        .map(|(name, entry)| Service {
          name: Some(name),
          entry,
        })
        .collect(),
      false,
    ),
  };

  let mut tunnels = Vec::new();
  for (name, entry) in config.tunnels.clone().unwrap_or_default() {
    tunnels.push(entry.decl(&name));
  }

  Ok(ClientSettings {
    token: nonempty(server.token.clone()),
    server: nonempty(server.url.clone()),
    server_urls: server.urls.clone().unwrap_or_default(),
    api_key: nonempty(server.api_key.clone()),
    ip_family: crate::dial::IpFamily::parse(server.ip_family.as_deref()),
    // Refused rather than defaulted: the value is a floor, see
    // `TlsPolicy::parse`. On hot-reload that means the edit is refused and the
    // previous configuration keeps serving.
    tls_policy: crate::dial::TlsPolicy::parse(
      server.min_tls_version.as_deref(),
      server.tls_cipher_suites.as_deref(),
    )?,
    // Refused rather than defaulted, on the same reasoning: a client told to
    // go through a proxy is on a network where going direct does not work,
    // so ignoring a value it could not read would produce a connection
    // failure whose cause is a typo three layers away.
    egress_proxy: match nonempty(server.proxy.clone()) {
      Some(raw) => Some(crate::egress::EgressProxy::parse(&raw)?),
      None => None,
    },
    client_id: nonempty(config.client_id.clone()),
    client_id_file: nonempty(config.client_id_file.clone()),
    name: nonempty(config.name.clone()),
    services,
    single,
    defaults: ServiceEntry {
      policy: config.defaults.clone().unwrap_or_default(),
      ..ServiceEntry::default()
    },
    tunnels,
    binds: config.binds.clone().unwrap_or_default(),
    subscribe: config
      .subscribe
      .clone()
      .unwrap_or_default()
      .iter()
      .map(aperio_config::SubscribeValue::entry)
      .collect(),
    messages_listen: nonempty(config.messages.as_ref().and_then(|m| m.listen.clone())),
    messages_mqtt_listen: nonempty(config.messages.as_ref().and_then(|m| m.mqtt_listen.clone())),
    otel_bridge: config.otel_bridge.clone(),
    scaling: config.scaling.clone(),
    idle_timeout: config.idle_timeout.map(|s| s.as_secs()).filter(|s| *s > 0),
    reload_drain_secs: config.reload_drain.map(|s| s.as_secs()).unwrap_or(10),
    bandwidth: nonempty(config.bandwidth.clone()),
    max_message_size: config
      .max_message_size
      .map(|b| b.0 as usize)
      .unwrap_or(32 * 1024 * 1024),
    allow_server_forward: config.allow_server_forward.unwrap_or(false),
    serve_spa: config.serve_spa.unwrap_or(false),
    serve_404: nonempty(config.serve_404.clone()),
    device_key: nonempty(config.device_key.clone()),
    device_key_file: nonempty(config.device_key_file.clone()),
    pid_file: nonempty(config.pid_file.clone()),
    sources,
  })
}
