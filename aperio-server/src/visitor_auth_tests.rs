//! What a written `auth:` becomes once it is in force, and in particular the
//! two answers the request path asks for: does this gate anything, and does
//! this credential open it.

use super::*;

/// Compiles a policy from the yaml an operator would write.
fn policy(yaml: &str) -> Policy {
  Policy::compile(&serde_yaml::from_str::<AuthSetting>(yaml).expect("a valid auth: value"))
}

#[test]
fn the_scalar_spelling_compiles_to_exactly_what_it_always_meant() {
  // Every file written before the grammar existed goes through this path, so
  // it is the one equivalence that cannot drift.
  let from_scalar = Policy::from_credentials("admin:s3cret");
  let from_yaml = policy("\"admin:s3cret\"");
  assert_eq!(from_scalar, from_yaml);
  assert!(from_scalar.gates());
  assert!(from_scalar.admits_credential("admin:s3cret"));
  assert!(!from_scalar.admits_credential("admin:wrong"));
  assert_eq!(from_scalar.as_single_credential(), Some("admin:s3cret"));
}

#[test]
fn a_credential_nobody_could_present_still_gates() {
  // The environment variable and the dashboard field are free text, and this
  // is the direction a mistake in them has to fail. `APERIO_SERVER_AUTH=secret`
  // has always produced a gate nobody can open; reading it as "no gate" would
  // turn one typo into every route on the server going public.
  let typo = Policy::from_credentials("nocolon");
  assert!(typo.gates(), "a present credential always gates");
  assert!(
    !typo.admits_credential("user:password"),
    "no ordinary login opens it"
  );
  // What *does* open it is the literal itself, which a browser cannot send
  // (it always writes a colon) but curl can. That is what this value has
  // always meant, and it is why the file refuses to write one: the point here
  // is only that the mistake fails closed rather than open.
  assert!(typo.admits_credential("nocolon"));

  // Only saying nothing means nothing.
  for raw in ["", "   "] {
    let p = Policy::from_credentials(raw);
    assert!(!p.gates(), "{raw:?} should configure no gate");
    assert!(p.admits_everyone());
  }
}

#[test]
fn an_open_gate_and_an_absent_one_are_different_things() {
  // Both admit everyone today. #108 is where the difference starts to decide
  // something, and collapsing them now would be the thing to undo then.
  let open = policy("{method: none}");
  let absent = Policy::default();
  assert!(open.admits_everyone() && absent.admits_everyone());
  assert_ne!(open, absent);
  assert_eq!(open.method_names(), vec!["none"]);
  assert!(absent.method_names().is_empty());
}

#[test]
fn several_credentials_are_alternatives_on_one_gate() {
  let p = policy("{method: basic, users: [\"alice:one\", \"bob:two\"]}");
  assert!(p.gates());
  assert!(p.admits_credential("alice:one"));
  assert!(p.admits_credential("bob:two"));
  assert!(!p.admits_credential("carol:three"));
  // Not expressible as the one scalar the old surfaces carry, and it says so.
  assert_eq!(p.as_single_credential(), None);
}

#[test]
fn a_method_that_does_not_exist_never_becomes_an_open_gate() {
  // The compile step runs on every hot reload and cannot refuse, so the
  // direction it fails in is the whole of its safety: an entry it cannot read
  // is dropped, and the methods beside it stay in force.
  let mixed = policy("[{method: basic, users: \"a:b\"}, {method: ldap}]");
  assert!(mixed.gates(), "a dropped entry must not open the gate");
  assert!(mixed.admits_credential("a:b"));
  assert_eq!(mixed.method_names(), vec!["basic"]);

  // And a policy that is nothing but unreadable leaves no gate, which is the
  // pre-grammar behaviour for a setting the server does not understand, not
  // an open one it invented.
  let all_unknown = policy("{method: ldap}");
  assert!(all_unknown.method_names().is_empty());
  assert_eq!(all_unknown, Policy::default());
}

#[test]
fn a_basic_method_with_no_usable_credential_locks_rather_than_opens() {
  // Unreachable through a config file, which refuses this where it is written,
  // so this pins the direction the compile step fails in when it is reached
  // some other way.
  let p = policy("{method: basic, users: [\"nocolon\"]}");
  assert!(p.gates(), "a basic method is a gate whatever is in it");
  assert!(!p.admits_credential("user:password"));
}

#[test]
fn the_open_gate_admits_without_a_credential_and_carries_none() {
  let p = policy("{method: none}");
  assert!(!p.gates());
  assert!(p.admits_everyone());
  assert_eq!(p.as_single_credential(), None);
  assert!(!p.admits_credential("anything:atall"));
}

#[test]
fn a_bearer_secret_opens_its_gate_and_nothing_else_does() {
  let p = policy("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  assert!(p.gates());
  assert!(p.admits_bearer("0123456789abcdef-secret", false));
  assert!(!p.admits_bearer("something-else-entirely", false));
  // A bearer gate has no `user:password` half, so the login form cannot open
  // it and the scalar surfaces cannot describe it.
  assert!(!p.admits_credential("0123456789abcdef-secret"));
  assert_eq!(p.as_single_credential(), None);
  assert_eq!(p.method_names(), vec!["bearer"]);
}

#[test]
fn the_query_form_is_off_until_a_method_asks_for_it() {
  // The query string is the form that reaches logs and history, so it is a
  // decision per gate rather than a property of the secret.
  let header_only = policy("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  assert!(header_only.admits_bearer("0123456789abcdef-secret", false));
  assert!(
    !header_only.admits_bearer("0123456789abcdef-secret", true),
    "the same secret in a URL must not open a gate that never asked for it"
  );
  assert!(!header_only.accepts_query_token());

  let opted_in = policy("{method: bearer, secret: \"0123456789abcdef-secret\", query: true}");
  assert!(opted_in.admits_bearer("0123456789abcdef-secret", true));
  assert!(opted_in.accepts_query_token());
}

#[test]
fn several_secrets_are_alternatives_so_a_key_can_be_rotated() {
  let p = policy("{method: bearer, secret: [\"0123456789abcdef-new\", \"0123456789abcdef-old\"]}");
  assert!(p.admits_bearer("0123456789abcdef-new", false));
  assert!(p.admits_bearer("0123456789abcdef-old", false));
  assert!(!p.admits_bearer("0123456789abcdef-gone", false));
}

#[test]
fn a_gate_answers_in_the_shape_its_caller_can_act_on() {
  // What decides between a 401 and a login redirect: whether the gate has
  // anything a caller could present on the request itself.
  let bearer = policy("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  assert!(bearer.has_direct_method());
  assert_eq!(bearer.challenge(), Some("Bearer"));

  let basic = policy("{method: basic, users: \"a:b\"}");
  assert!(
    !basic.has_direct_method(),
    "basic is a login form; there is nothing to answer a 401 with"
  );
  assert_eq!(basic.challenge(), None);

  // A route offering both answers a script and a browser each in their own
  // shape, which is the whole reason the policy is a list.
  let both = policy(
    "[{method: basic, users: \"a:b\"}, {method: bearer, secret: \"0123456789abcdef-secret\"}]",
  );
  assert!(both.gates());
  assert!(both.admits_credential("a:b"));
  assert!(both.admits_bearer("0123456789abcdef-secret", false));
  assert_eq!(both.challenge(), Some("Bearer"));
  assert_eq!(both.method_names(), vec!["basic", "bearer"]);
}
