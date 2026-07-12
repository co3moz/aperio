use super::{env_name, env_value};

#[test]
fn env_name_follows_the_naming_standard() {
  assert_eq!(env_name("max_body_size"), "APERIO_MAX_BODY_SIZE");
  assert_eq!(env_name("server_token"), "APERIO_SERVER_TOKEN");
  // Dashes normalize like the CLI surface does.
  assert_eq!(env_name("lb-strategy"), "APERIO_LB_STRATEGY");
  // Bare keys stay un-prefixed.
  assert_eq!(env_name("host"), "HOST");
  assert_eq!(env_name("port"), "PORT");
  assert_eq!(env_name("log_level"), "LOG_LEVEL");
  // Already-prefixed keys are not double-prefixed.
  assert_eq!(env_name("aperio_cache"), "APERIO_CACHE");
}

#[test]
fn env_value_renders_scalars_and_lists() {
  use serde_yaml::Value;
  assert_eq!(env_value(&Value::Bool(true)).as_deref(), Some("1"));
  assert_eq!(env_value(&Value::Bool(false)).as_deref(), Some("0"));
  assert_eq!(
    env_value(&Value::Number(8080.into())).as_deref(),
    Some("8080")
  );
  assert_eq!(env_value(&Value::String("x".into())).as_deref(), Some("x"));
  let list: Value = serde_yaml::from_str("[10.0.0.0/8, 192.168.0.1]").unwrap();
  assert_eq!(env_value(&list).as_deref(), Some("10.0.0.0/8,192.168.0.1"));
  // Nested structures are not representable.
  let nested: Value = serde_yaml::from_str("[[1]]").unwrap();
  assert_eq!(env_value(&nested), None);
  assert_eq!(env_value(&Value::Null), None);
}
