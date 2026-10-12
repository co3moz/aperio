//! What this pins down: the one merge rule and its order (defaults, home
//! file, local file, environment, command line), that `defaults:` reaches
//! every service without beating what a service writes itself, that the
//! environment's names come from the schema and cannot collide, and that a
//! key nothing reads is an error that names what was meant.

use super::*;

fn yaml(text: &str) -> Mapping {
  parse_document(text).expect("test yaml parses")
}

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
  move |name| {
    pairs
      .iter()
      .find(|(k, _)| *k == name)
      .map(|(_, v)| v.to_string())
  }
}

fn setting(var: &str) -> &'static EnvSetting {
  client_env()
    .iter()
    .find(|s| s.var == var)
    .unwrap_or_else(|| panic!("{var} is not in the table"))
}

// ---------------------------------------------------------------------------
// The environment's names
// ---------------------------------------------------------------------------

#[test]
fn every_variable_names_one_setting() {
  let mut seen = std::collections::BTreeMap::new();
  for s in client_env() {
    if let Some(other) = seen.insert(s.var.clone(), s.key()) {
      panic!(
        "{} is the variable of both `{other}` and `{}`",
        s.var,
        s.key()
      );
    }
  }
  // A table this short means the schema walk broke, not that the client lost
  // its settings.
  assert!(seen.len() > 80, "only {} variables", seen.len());
}

#[test]
fn a_variable_is_the_key_it_writes() {
  // Document keys carry their path; `defaults:` keys are written bare, since
  // that is where one service's settings go when there is no file.
  assert_eq!(setting("APERIO_SERVER_URL").key(), "server.url");
  assert_eq!(setting("APERIO_MESSAGES_LISTEN").key(), "messages.listen");
  assert_eq!(setting("APERIO_TIMEOUT").key(), "defaults.timeout");
  assert_eq!(
    setting("APERIO_HEALTH_INTERVAL").key(),
    "defaults.health.interval"
  );
  assert_eq!(
    setting("APERIO_LIMITS_REQUEST_BODY").key(),
    "defaults.limits.request_body"
  );
  assert_eq!(setting("APERIO_TARGET").scope, Scope::Service);
  assert_eq!(setting("LOG_LEVEL").key(), "log_level");
}

#[test]
fn a_policy_is_written_whole_not_field_by_field() {
  // `access:` is a method grammar, not a block of settings: its fields would
  // otherwise each get a variable no one would set.
  assert!(client_env().iter().any(|s| s.var == "APERIO_ACCESS"));
  assert!(
    !client_env()
      .iter()
      .any(|s| s.var.starts_with("APERIO_ACCESS_"))
  );
}

#[test]
fn a_block_has_no_variable_of_its_own_only_its_fields_do() {
  for block in [
    "APERIO_SERVER",
    "APERIO_HEALTH",
    "APERIO_LIMITS",
    "APERIO_CACHE",
  ] {
    assert!(
      !client_env().iter().any(|s| s.var == block),
      "{block} should not exist; its fields have variables"
    );
  }
}

#[test]
fn a_variable_reads_as_the_type_of_its_key() {
  let read = |var: &str, raw: &str| parse_env_value(setting(var), raw).unwrap();
  assert_eq!(read("APERIO_INSPECTOR", "0"), Value::Bool(false));
  assert_eq!(read("APERIO_SERVE_SPA", "true"), Value::Bool(true));
  assert_eq!(read("APERIO_PRIORITY", "2"), Value::Number(2.into()));
  // A duration takes its unit, or a bare number of seconds.
  assert_eq!(read("APERIO_TIMEOUT", "30s"), Value::String("30s".into()));
  assert_eq!(read("APERIO_TIMEOUT", "30"), Value::Number(30.into()));
  // A list is comma-separated, and a key that is one or many splits only when
  // there is more than one.
  assert_eq!(
    read("APERIO_ALLOWED_IPS", "10.0.0.0/8, 1.2.3.4")
      .as_sequence()
      .unwrap()
      .len(),
    2
  );
  assert_eq!(
    read("APERIO_HOSTNAME", "a.example.com"),
    Value::String("a.example.com".into())
  );
  assert_eq!(
    read("APERIO_HOSTNAME", "a.example.com,b.example.com")
      .as_sequence()
      .unwrap()
      .len(),
    2
  );
  // A map of strings is k=v,k=v.
  let labels = read("APERIO_METRICS_LABELS", "env=prod,region=eu");
  assert_eq!(labels.as_mapping().unwrap().len(), 2);
  // Anything can be given as yaml.
  let access = read("APERIO_ACCESS", "{method: basic, users: 'a:b'}");
  assert!(access.is_mapping());
  assert_eq!(
    read("APERIO_ACCESS", "public"),
    Value::String("public".into())
  );
}

#[test]
fn a_flag_that_is_not_a_flag_is_refused() {
  let err = parse_env_value(setting("APERIO_INSPECTOR"), "maybe").unwrap_err();
  assert!(err.contains("true or false"), "{err}");
}

#[test]
fn a_removed_variable_refuses_the_start_and_names_its_replacement() {
  let err = env_overlay(env(&[("APERIO_PUBLIC", "1")])).unwrap_err();
  assert!(err.contains("APERIO_ACCESS=public"), "{err}");
  let err = env_overlay(env(&[("APERIO_RETRY_BACKOFF", "100")])).unwrap_err();
  assert!(err.contains("100ms"), "{err}");
}

#[test]
fn the_environment_writes_the_document_defaults_and_the_one_service() {
  let overlay = env_overlay(env(&[
    ("APERIO_SERVER_URL", "https://s.example.com"),
    ("APERIO_TIMEOUT", "1m"),
    ("APERIO_TARGET", "3000"),
    ("APERIO_UNRELATED", "ignored"),
  ]))
  .unwrap();
  assert_eq!(
    overlay.file["server"]["url"],
    Value::String("https://s.example.com".into())
  );
  assert_eq!(
    overlay.file["defaults"]["timeout"],
    Value::String("1m".into())
  );
  // A bare port is a number here, as it is in a file, and the target reads
  // either spelling as the same text the client expands.
  assert_eq!(overlay.service["target"], Value::Number(3000.into()));
  let entry: ServiceEntry =
    serde_yaml::from_value(Value::Mapping(overlay.service.clone())).unwrap();
  assert_eq!(entry.target.as_deref(), Some("3000"));
}

// ---------------------------------------------------------------------------
// The merge
// ---------------------------------------------------------------------------

#[test]
fn mappings_merge_and_everything_else_is_replaced() {
  let mut base = yaml("server: {url: a, token: t}\ndefaults: {allowed_ips: [1.1.1.1, 2.2.2.2]}");
  merge(
    &mut base,
    yaml("server: {url: b}\ndefaults: {allowed_ips: [3.3.3.3]}"),
  );
  assert_eq!(base["server"]["url"], Value::String("b".into()));
  assert_eq!(base["server"]["token"], Value::String("t".into()));
  assert_eq!(
    base["defaults"]["allowed_ips"].as_sequence().unwrap().len(),
    1
  );
}

#[test]
fn a_later_layer_wins_key_by_key() {
  let home = yaml("server: {url: https://home, token: home-token}\nname: from_home");
  let local = yaml("server: {url: https://local}");
  let mut env_layer = Overlay::default();
  env_layer.set(Scope::File, "name", Value::String("from_env".into()));
  let mut cli = Overlay::default();
  cli.set(
    Scope::File,
    "server.url",
    Value::String("https://cli".into()),
  );
  let got = assemble(
    vec![(Layer::Home, home), (Layer::Local, local)],
    vec![(Layer::Env, env_layer), (Layer::Cli, cli)],
  )
  .unwrap();
  let server = got.config.server();
  assert_eq!(server.url.as_deref(), Some("https://cli"));
  assert_eq!(server.token.as_deref(), Some("home-token"));
  // The environment beats the file, which is what a container expects.
  assert_eq!(got.config.name.as_deref(), Some("from_env"));
  assert_eq!(got.sources["server.url"], Layer::Cli);
  assert_eq!(got.sources["server.token"], Layer::Home);
  assert_eq!(got.sources["name"], Layer::Env);
}

#[test]
fn defaults_reach_every_service_and_a_service_keeps_its_own() {
  let file = yaml(
    "defaults: {timeout: 60s, health: {interval: 5s, threshold: 3}}\n\
     services:\n  web: {target: '3000', health: {interval: 1s}}\n  api: {target: '4000', timeout: 2m}",
  );
  let got = assemble(vec![(Layer::Local, file)], Vec::new()).unwrap();
  let services = got.config.services.unwrap();
  let web = &services["web"].policy;
  // A block merges with the default block: web wrote only its interval.
  assert_eq!(web.timeout, Some(crate::units::Span::secs(60)));
  let health = web.health.as_ref().unwrap();
  assert_eq!(health.interval, Some(crate::units::Span::secs(1)));
  assert_eq!(health.threshold, Some(3));
  assert_eq!(
    services["api"].policy.timeout,
    Some(crate::units::Span::secs(120))
  );
}

#[test]
fn the_environment_sets_defaults_and_a_service_still_keeps_its_own() {
  let file = yaml("services:\n  web: {target: '3000'}\n  api: {target: '4000', timeout: 2m}");
  let overlay = env_overlay(env(&[("APERIO_TIMEOUT", "45s")])).unwrap();
  let got = assemble(vec![(Layer::Local, file)], vec![(Layer::Env, overlay)]).unwrap();
  let services = got.config.services.unwrap();
  assert_eq!(
    services["web"].policy.timeout,
    Some(crate::units::Span::secs(45))
  );
  assert_eq!(
    services["api"].policy.timeout,
    Some(crate::units::Span::secs(120))
  );
}

#[test]
fn services_keep_the_order_the_file_writes_them_in() {
  let file = yaml("services:\n  zeta: {target: '1'}\n  alpha: {target: '2'}\n  mid: {target: '3'}");
  let got = assemble(vec![(Layer::Local, file)], Vec::new()).unwrap();
  let names: Vec<String> = got.config.services.unwrap().keys().cloned().collect();
  assert_eq!(names, ["zeta", "alpha", "mid"]);
}

#[test]
fn the_command_line_service_is_laid_over_defaults_too() {
  let file = yaml("defaults: {access: public, timeout: 10s}");
  let overlay = env_overlay(env(&[
    ("APERIO_TARGET", "http://localhost:3000"),
    ("APERIO_HOSTNAME", "app.example.com"),
  ]))
  .unwrap();
  let got = assemble(vec![(Layer::Local, file)], vec![(Layer::Env, overlay)]).unwrap();
  let service = got.service.expect("a target names a service");
  assert_eq!(service.target.as_deref(), Some("http://localhost:3000"));
  assert_eq!(service.policy.access, Some(crate::Access::Public));
  assert_eq!(service.policy.timeout, Some(crate::units::Span::secs(10)));
}

#[test]
fn a_hostname_with_no_target_is_refused_rather_than_ignored() {
  let overlay = env_overlay(env(&[("APERIO_HOSTNAME", "app.example.com")])).unwrap();
  let err = assemble(Vec::new(), vec![(Layer::Env, overlay)]).unwrap_err();
  assert!(err.contains("APERIO_TARGET"), "{err}");
}

#[test]
fn a_type_error_names_the_service_it_is_in() {
  let file = yaml("services:\n  web: {target: '3000'}\n  api: {target: '4000', priority: high}");
  let err = assemble(vec![(Layer::Local, file)], Vec::new()).unwrap_err();
  assert!(err.starts_with("services.api"), "{err}");
}

#[test]
fn an_x_key_is_left_for_yaml_anchors() {
  let file =
    yaml("x-common: &common {timeout: 5s}\nservices:\n  web: {target: '3000', <<: *common}");
  assert!(check_document(&file, "aperio.yaml").is_ok());
  assert!(assemble(vec![(Layer::Local, file)], Vec::new()).is_ok());
}

// ---------------------------------------------------------------------------
// Keys nothing reads
// ---------------------------------------------------------------------------

fn refusal(text: &str) -> String {
  check_document(&yaml(text), "aperio.yaml").unwrap_err()
}

#[test]
fn a_typo_is_refused_with_the_key_that_was_meant() {
  let err = refusal("servr: {url: x}");
  assert!(err.contains("did you mean `server`"), "{err}");
  let err = refusal("services:\n  web: {target: '1', timeot: 5s}");
  assert!(
    err.contains("`services.web.timeot`") && err.contains("`timeout`"),
    "{err}"
  );
  let err = refusal("defaults: {health: {intervall: 5s}}");
  assert!(
    err.contains("`defaults.health.intervall`") && err.contains("`interval`"),
    "{err}"
  );
}

#[test]
fn a_key_an_earlier_release_read_says_what_replaced_it() {
  assert!(refusal("public: true").contains("`access:`"));
  assert!(refusal("timeout: 30").contains("`defaults:`"));
  assert!(
    refusal("services:\n  web: {target: '1', name: web}").contains("the key it is written under")
  );
  assert!(refusal("services:\n  web: {target: '1', retry: {backoff: 100}}").contains("100ms"));
  assert!(refusal("bind-tunnels: {pg: 1}").contains("`binds:`"));
}

#[test]
fn a_key_that_changed_shape_says_what_the_shape_is_now() {
  assert!(refusal("server: https://x").contains("`server: {url:"));
  assert!(refusal("services:\n  - target: '1'").contains("map keyed by name"));
  assert!(refusal("defaults: {cache: true}").contains("`cache: {enabled: true}`"));
}

#[test]
fn every_mistake_in_a_file_is_reported_at_once() {
  let err = refusal("servr: {}\ntoken: t");
  assert_eq!(err.lines().count(), 2, "{err}");
}

// ---------------------------------------------------------------------------
// What a reload cannot apply
// ---------------------------------------------------------------------------

#[test]
fn the_keys_applied_at_startup_come_from_the_schema() {
  let paths = restart_paths();
  for key in [
    "server.proxy",
    "server.ip_family",
    "otel_bridge",
    "log_level",
    "client_id",
  ] {
    assert!(paths.iter().any(|p| p == key), "{key} in {paths:?}");
  }
  // A service setting applies on a reload: the services are rebuilt.
  assert!(
    !paths
      .iter()
      .any(|p| p.starts_with("defaults") || p.starts_with("services"))
  );
}

#[test]
fn a_reload_names_the_startup_keys_it_changed() {
  let before = assemble(
    vec![(
      Layer::Local,
      yaml("server: {proxy: 'a:1'}\nlog_level: info"),
    )],
    Vec::new(),
  )
  .unwrap()
  .config;
  let after = assemble(
    vec![(
      Layer::Local,
      yaml("server: {proxy: 'b:1'}\nlog_level: info\ndefaults: {timeout: 5s}"),
    )],
    Vec::new(),
  )
  .unwrap()
  .config;
  assert_eq!(
    restart_changes(&restart_values(&before), &restart_values(&after)),
    vec!["server.proxy".to_string()]
  );
}
