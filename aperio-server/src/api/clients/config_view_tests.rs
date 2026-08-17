//! What the config view says a service is actually running: the declared
//! value beside the effective one wherever they differ, an active overrule,
//! and the optional knobs a richly configured service sets.

use super::super::clients_tests::*;
use super::*;
use crate::test_support::*;
use axum::extract::Path;
use axum::extract::State;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn an_elastic_pool_is_rendered_as_its_range_and_its_current_size() {
  // The count is what the pool has open right now, and on its own it read as
  // a fixed setting: a dashboard showing `connections: 3` beside four live
  // connections, because 3 was the size the pool happened to be when that
  // connection announced itself. The range is what says the number moves.
  let state = Arc::new(test_state());
  insert_client(&state, "pool", |h| {
    h.sole_mut().service_name = Some("axum".to_string());
    h.sole_mut().connections = Some(4);
    h.sole_mut().connections_min = Some(1);
    h.sole_mut().connections_max = Some(5);
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("pool".to_string()),
    axum::extract::Query(Default::default()),
    headers,
  )
  .await;
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();
  assert!(
    yaml.contains("connections: { min: 1, max: 5 }  # 4 open right now"),
    "got:\n{yaml}"
  );

  // A fixed pool announces no range and is written the way the file wrote it.
  insert_client(&state, "fixed", |h| {
    h.sole_mut().service_name = Some("api".to_string());
    h.sole_mut().connections = Some(3);
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("fixed".to_string()),
    axum::extract::Query(Default::default()),
    headers,
  )
  .await;
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();
  assert!(yaml.contains("connections: 3"), "got:\n{yaml}");
  assert!(!yaml.contains("min:"), "got:\n{yaml}");
}

#[tokio::test]
async fn client_config_renders_yaml_with_declared_vs_effective_notes() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().service_name = Some("api".to_string());
    h.reported_instance_id = Some("my-box-0".to_string());
    h.sole_mut().connections = Some(10);
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec![
      "app.example.com".to_string(),
      "wild-fox.example.com".to_string(),
    ];
    h.sole_mut().random_hostname = Some("wild-fox.example.com".to_string());
    h.sole_mut().max_concurrent = Some(32);
    h.sole().bandwidth_bps.store(125_000, Ordering::Relaxed);
    // Opted into caching while the test server has its cache disabled.
    h.sole_mut().cache = true;
    // What the client itself resolved differently before announcing it.
    h.sole_mut().config_notes = vec![crate::protocol::ConfigNote {
      field: "bandwidth".to_string(),
      declared: "10mbit".to_string(),
      effective: "1mbit".to_string(),
      reason: "split across 10 parallel connections".to_string(),
    }];
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();

  assert!(yaml.contains("name: \"api\""), "got:\n{yaml}");
  assert!(yaml.contains("connections: 10"), "got:\n{yaml}");
  assert!(
    yaml.contains("  - \"app.example.com\"  # requested by the client"),
    "each hostname is labeled with where it came from:\n{yaml}"
  );
  assert!(
    yaml.contains("  - \"wild-fox.example.com\"  # random subdomain, assigned by the server"),
    "got:\n{yaml}"
  );
  // The client-reported difference rides along as a trailing comment.
  assert!(
    yaml.contains("bandwidth: \"1mbit\"  # declared 10mbit: split across 10 parallel connections"),
    "got:\n{yaml}"
  );
  assert!(
    yaml.contains("cache: true  # declared true: the server's response cache is disabled"),
    "a server-side adjustment is annotated too:\n{yaml}"
  );

  let notes = body["notes"].as_array().unwrap();
  assert_eq!(notes.len(), 2, "one from the client, one from the server");
  let bw = notes.iter().find(|n| n["field"] == "bandwidth").unwrap();
  assert_eq!(bw["declared"], "10mbit");
  assert_eq!(bw["effective"], "1mbit");
  assert_eq!(bw["source"], "client");
  let cache = notes.iter().find(|n| n["field"] == "cache").unwrap();
  assert_eq!(cache["effective"], "false");
  assert_eq!(cache["source"], "server");
}

#[tokio::test]
async fn client_config_renders_an_empty_hostname_list() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = None;
    h.sole_mut().declared_hostnames = Vec::new();
    h.sole_mut().assigned_hostnames = Vec::new();
    h.sole_mut().random_hostname = None;
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();

  // Serving no hostname is a state worth stating outright; omitting the key
  // would read as "not rendered yet" rather than "this connection has none".
  assert!(yaml.contains("hostname: []"), "got:\n{yaml}");
}

#[tokio::test]
async fn client_config_reports_an_active_overrule_and_hides_other_orgs() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().override_hostname_binds = vec!["moved.example.com".to_string()];
  })
  .await;
  insert_client(&state, "other", |h| {
    h.perms.org_id = Some("acme".to_string());
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    headers.clone(),
  )
  .await;
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();
  // Routing follows the overrule, so the document does too.
  assert!(
    yaml.contains("  - \"moved.example.com\"  # dashboard overrule"),
    "got:\n{yaml}"
  );
  assert!(
    !yaml.contains("  - \"app.example.com\""),
    "the overruled name no longer routes:\n{yaml}"
  );
  let note = body["notes"]
    .as_array()
    .unwrap()
    .iter()
    .find(|n| n["field"] == "hostname")
    .unwrap()
    .clone();
  assert_eq!(note["declared"], "app.example.com");
  assert_eq!(note["effective"], "moved.example.com");

  // A client of another organization is a 404, like everywhere else.
  let resp = client_config_handler(
    State(state.clone()),
    Path("other".to_string()),
    axum::extract::Query(Default::default()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn client_config_renders_every_optional_knob_and_server_overrule() {
  // One maximal connection, so every branch of the yaml renderer runs: the
  // dashboard overrules, the refused announcements, and each optional line.
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec![
      "app.example.com".to_string(),
      "wild-fox.example.com".to_string(),
    ];
    h.sole_mut().random_hostname = Some("wild-fox.example.com".to_string());
    h.sole_mut().override_hostname_binds = vec!["forced.example.com".to_string()];
    h.sole_mut().declared_path = Some("/api".to_string());
    h.sole_mut().override_path_bind = Some("/forced".to_string());
    h.sole_mut().public_denied_warned = true;
    h.sole_mut().visitor_auth_denied_warned = true;
    h.sole_mut().priority = 2;
    h.sole_mut().public = true;
    h.sole_mut().visitor_auth = Some("user:pass".to_string());
    h.sole_mut().allowed_ips = vec!["10.0.0.0/8".to_string(), "203.0.113.7".to_string()];
    h.sole_mut().denied = Some("https://example.com/no".to_string());
    h.sole_mut().cache = false;
    h.sole_mut().resilience = true;
    h.sole_mut().webhook_inbox = true;
    h.sole_mut().max_request_body = Some(1048576);
    h.sole_mut().response_timeout = Some(120);
    h.sole_mut().tcp_enabled = true;
    h.sole_mut().tunnels = vec![crate::protocol::TunnelDecl {
      name: Some("pg".to_string()),
      custom_name: None,
      target: "127.0.0.1:5432".to_string(),
      protocol: "tcp".to_string(),
      encrypt: true,
      idle_timeout: None,
      expose: None,
    }];
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();

  for line in [
    "priority: 2",
    "public: true",
    "auth: \"<set by the client>\"",
    "allowed_ips: [\"10.0.0.0/8\", \"203.0.113.7\"]",
    "denied: \"https://example.com/no\"",
    "resilience: true",
    "webhook_inbox: true",
    "max_request_body: 1048576",
    "response_timeout: 120",
    "tcp_target: \"<set by the client>\"",
    "tunnels:",
    "    encrypt: true",
  ] {
    assert!(yaml.contains(line), "missing `{line}` in:\n{yaml}");
  }

  // The three server-side refusals and the two overrules are all notes.
  let notes = body["notes"].as_array().unwrap();
  let fields: Vec<&str> = notes.iter().filter_map(|n| n["field"].as_str()).collect();
  for field in ["public", "auth", "hostname", "path"] {
    assert!(fields.contains(&field), "{fields:?}");
  }
  let hostname_note = notes.iter().find(|n| n["field"] == "hostname").unwrap();
  assert_eq!(hostname_note["effective"], "forced.example.com");
  assert!(
    hostname_note["declared"]
      .as_str()
      .unwrap()
      .contains("wild-fox.example.com"),
    "the assigned hostname is part of what the overrule replaced"
  );
}
