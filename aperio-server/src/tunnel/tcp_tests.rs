use super::*;

fn dynamic_perms(token_id: &str) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: Vec::new(),
    paths: Vec::new(),
    token_name: Some(format!("token-{token_id}")),
    token_id: Some(token_id.to_string()),
    allow_public: false,
    org_id: None,
  }
}

#[test]
fn test_same_token() {
  let master = ClientPerms::master();
  let a = dynamic_perms("a");
  let a2 = dynamic_perms("a");
  let b = dynamic_perms("b");

  // The master token may bind any client's tunnels.
  assert!(same_token(&master, &a));
  assert!(same_token(&master, &master));

  // A dynamic token only matches clients using the very same token.
  assert!(same_token(&a, &a2));
  assert!(!same_token(&a, &b));

  // A dynamic token never matches a master-token client, and a
  // master-token OWNER is only bindable by the master token itself.
  assert!(!same_token(&a, &master));
}
