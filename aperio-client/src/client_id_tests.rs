//! What this pins down: an id given is used as given, an id kept on disk is
//! the same across runs, two config files on one machine get two ids, and a
//! place that cannot be written costs persistence and nothing else.

use super::*;

fn scratch(tag: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-client-id-{tag}-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

#[test]
fn a_configured_id_is_used_and_must_be_a_uuid() {
  let id = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
  assert_eq!(resolve(Some(id), None, None, None).unwrap(), id);
  let err = resolve(Some("not-an-id"), None, None, None).unwrap_err();
  assert!(err.contains("not a UUID"), "{err}");
}

#[test]
fn an_id_kept_in_a_file_is_the_same_on_the_next_run() {
  let dir = scratch("file");
  let file = dir.join("nested").join("client-id");
  let first = resolve(None, Some(file.to_str().unwrap()), None, None).unwrap();
  let second = resolve(None, Some(file.to_str().unwrap()), None, None).unwrap();
  assert_eq!(first, second);
  assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), first);
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn two_config_files_on_one_machine_get_two_ids() {
  let home = scratch("home");
  let a = home.join("a.yaml");
  let b = home.join("b.yaml");
  std::fs::write(&a, "").unwrap();
  std::fs::write(&b, "").unwrap();
  assert_ne!(default_file(&home, &a), default_file(&home, &b));
  // The default lives under the home, never next to the project's file.
  assert!(default_file(&home, &a).starts_with(home.join(".aperio").join("clients")));
  let first = resolve(None, None, Some(&a), Some(&home)).unwrap();
  let other = resolve(None, None, Some(&b), Some(&home)).unwrap();
  assert_ne!(first, other);
  assert_eq!(resolve(None, None, Some(&a), Some(&home)).unwrap(), first);
  let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_client_with_no_file_and_no_place_runs_with_a_fresh_id() {
  let one = resolve(None, None, None, None).unwrap();
  let two = resolve(None, None, None, None).unwrap();
  assert_ne!(one, two);
  assert!(uuid::Uuid::parse_str(&one).is_ok());
}

#[test]
fn a_file_holding_something_else_is_replaced_by_an_id() {
  let dir = scratch("garbage");
  let file = dir.join("client-id");
  std::fs::write(&file, "hello").unwrap();
  let id = resolve(None, Some(file.to_str().unwrap()), None, None).unwrap();
  assert!(uuid::Uuid::parse_str(&id).is_ok());
  assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), id);
  let _ = std::fs::remove_dir_all(dir);
}

/// The device key is kept beside the id, under the same name: the two halves
/// of the client's identity live in one place and go together.
#[test]
fn the_default_key_file_sits_beside_the_id() {
  let home = scratch("key");
  let config = home.join("aperio.yaml");
  std::fs::write(&config, "").unwrap();
  let id = default_file(&home, &config);
  let key = default_key_file(&home, &config);
  assert_eq!(id.parent(), key.parent());
  assert_eq!(id.file_stem(), key.file_stem());
  assert_eq!(key.extension().and_then(|e| e.to_str()), Some("key"));
  let _ = std::fs::remove_dir_all(home);
}
