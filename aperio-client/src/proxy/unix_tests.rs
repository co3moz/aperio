use super::*;

#[test]
fn test_is_unix_target() {
  assert!(is_unix_target("unix:///var/run/app.sock"));
  assert!(is_unix_target("unix://./app.sock"));
  assert!(!is_unix_target("http://localhost:3000"));
  assert!(!is_unix_target("h2c://127.0.0.1:50051"));
}

#[test]
fn test_unix_socket_path() {
  assert_eq!(
    unix_socket_path("unix:///var/run/app.sock").as_deref(),
    Some("/var/run/app.sock")
  );
  assert_eq!(
    unix_socket_path("unix://./app.sock").as_deref(),
    Some("./app.sock")
  );
  assert_eq!(unix_socket_path("unix://"), None);
  assert_eq!(unix_socket_path("http://x"), None);
}
