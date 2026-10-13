//! What a written `auth:` becomes once it is in force, and in particular the
//! two answers the request path asks for: does this gate anything, and does
//! this credential open it.

use super::*;

/// Compiles a policy from the yaml an operator would write.
fn policy(yaml: &str) -> Policy {
  Policy::compile(&serde_yaml::from_str::<AuthSetting>(yaml).expect("a valid auth: value"))
}

#[test]
fn the_scalar_spelling_compiles_to_one_basic_login() {
  let from_scalar = policy("\"admin:s3cret\"");
  let from_block = policy("{method: basic, users: \"admin:s3cret\"}");
  assert_eq!(from_scalar, from_block);
  assert!(from_scalar.gates());
  assert!(from_scalar.admits_credential("admin:s3cret"));
  assert!(!from_scalar.admits_credential("admin:wrong"));
}

#[test]
fn a_credential_nobody_could_present_still_gates() {
  // This is the direction a mistake has to fail: a value that no login could
  // ever match still gates, admitting nobody, rather than reading as "no
  // gate" and turning one typo into every route on the server going public.
  let typo = policy("\"nocolon\"");
  assert!(typo.gates(), "a present credential always gates");
  assert!(
    !typo.admits_credential("user:password"),
    "no ordinary login opens it"
  );
  // What *does* open it is the literal itself, which a browser cannot send
  // (it always writes a colon) but curl can. The settings check refuses to
  // store one; the point here is only that the mistake fails closed.
  assert!(typo.admits_credential("nocolon"));

  // Only saying nothing means nothing.
  let absent = Policy::default();
  assert!(!absent.gates());
  assert!(absent.admits_everyone());
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
  assert!(!p.admits_credential("anything:atall"));
}

#[test]
fn a_bearer_secret_opens_its_gate_and_nothing_else_does() {
  let p = policy("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  assert!(p.gates());
  assert!(p.admits_bearer("0123456789abcdef-secret", false));
  assert!(!p.admits_bearer("something-else-entirely", false));
  // A bearer gate has no `user:password` half, so the login form cannot open
  // it.
  assert!(!p.admits_credential("0123456789abcdef-secret"));
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

/// A gate written in the settings is the gate in force, and one the server
/// cannot honour is refused with its setting named.
///
/// The failure this is here to prevent is an operator who wrote a gate and
/// got no gate: a server that answers as if nothing were configured, while
/// the settings say otherwise, is the worst shape this can take, because the
/// operator's evidence that the route is protected is what they wrote.
#[test]
fn a_gate_in_the_settings_is_the_gate_in_force() {
  let dir = crate::test_support::test_temp_root().join(format!("gate-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir = dir.to_string_lossy().into_owned();
  let build = |yaml: &str| {
    let settings = aperio_config::server_settings::ServerSettings::from_yaml(yaml).unwrap();
    crate::server::state_build::config::build_config(&settings, "tok", &dir)
  };

  let none = build("{}").unwrap();
  assert!(!none.visitor_auth.gates(), "no gate written, no gate");

  let gated = build("access:\n  method: basic\n  users: 'dan:hunter2'\n").unwrap();
  assert!(gated.visitor_auth.gates());
  assert!(gated.visitor_auth.admits_credential("dan:hunter2"));

  let err = build("access:\n  method: ldap\n")
    .err()
    .expect("a method that does not exist is refused, not dropped");
  assert!(err.starts_with("access"), "{err}");
}

#[test]
fn the_aperio_method_gates_without_a_credential_and_says_so() {
  let alone = policy("{method: aperio}");
  assert_eq!(alone.method_names(), vec!["aperio"]);
  assert!(alone.gates());
  assert!(!alone.admits_everyone());
  assert!(alone.has_aperio());
  assert!(alone.aperio_only());
  // No credential of any kind opens it from the request itself.
  assert!(!alone.admits_credential("a:b"));
  assert!(!alone.admits_bearer("whatever-secret-this-is", false));
  assert!(!alone.has_direct_method());
  assert_eq!(alone.challenge(), None);
  // Beside a bearer it is one of two doors, and no longer alone.
  let mixed = policy("[{method: aperio}, {method: bearer, secret: 0123456789abcdef}]");
  assert_eq!(mixed.method_names(), vec!["aperio", "bearer"]);
  assert!(mixed.has_aperio());
  assert!(!mixed.aperio_only());
  assert!(mixed.has_direct_method());
  assert!(!policy("{method: basic, users: [a:b]}").has_aperio());
}

#[test]
fn a_forward_knows_where_it_is_asked_from_and_a_clients_is_always_the_client() {
  let server_side = policy("{method: forward, url: 'http://127.0.0.1:7070/c'}");
  let cfg = server_side.forward_methods().next().unwrap();
  assert!(!cfg.via_client);
  assert!(
    cfg.cache.is_zero(),
    "the server's own asks every time by default"
  );
  let over = policy("{method: forward, url: 'http://127.0.0.1:7070/c', via: client}");
  let cfg = over.forward_methods().next().unwrap();
  assert!(cfg.via_client);
  assert_eq!(
    cfg.cache.as_secs(),
    30,
    "over the tunnel a verdict is remembered by default"
  );
  let told = policy("{method: forward, url: 'http://127.0.0.1:7070/c', via: client, cache: 0}");
  assert!(told.forward_methods().next().unwrap().cache.is_zero());
  // Declared by a client, the server never dials it, whatever the entry says.
  let setting: aperio_config::AuthSetting =
    serde_yaml::from_str("{method: forward, url: 'http://127.0.0.1:7070/c', via: server}").unwrap();
  let declared = Policy::compile_from(&setting, true);
  assert!(declared.forward_methods().next().unwrap().via_client);
}

// --- sso: what the mutation sweep found nobody had pinned -------------------

const SSO: &str = "{method: sso, issuer: https://id.example.com, client_id: visitors, client_secret: s3cret, allowed_emails: \"*@example.com\"}";

#[test]
fn a_clients_sso_is_never_compiled_and_an_operators_is() {
  // `sso` is the operator's: the provider and its secret are written on the
  // server, and a client declaring one is refused at the declaration. The
  // compiler holds the same line, so a declaration that got past it would
  // still not become a sign-in at a provider the client chose.
  let setting: AuthSetting = serde_yaml::from_str(SSO).unwrap();
  let operators = Policy::compile_from(&setting, false);
  assert!(operators.has_sso() && operators.sso_only());
  let clients = Policy::compile_from(&setting, true);
  assert!(!clients.has_sso());
}

#[test]
fn an_sso_methods_scopes_default_only_when_nothing_was_written() {
  let written = policy(
    "{method: sso, issuer: https://id.example.com, client_id: v, client_secret: s, allowed_emails: \"*\", scopes: \"openid profile\"}",
  );
  assert_eq!(
    written.sso_methods().next().unwrap().scopes,
    "openid profile"
  );
  for blank in [
    "{method: sso, issuer: https://id.example.com, client_id: v, client_secret: s, allowed_emails: \"*\"}",
    "{method: sso, issuer: https://id.example.com, client_id: v, client_secret: s, allowed_emails: \"*\", scopes: \"  \"}",
  ] {
    assert_eq!(
      policy(blank).sso_methods().next().unwrap().scopes,
      "openid email",
      "{blank}"
    );
  }
}

#[test]
fn an_sso_methods_key_tells_providers_and_clients_apart() {
  // The runtime discovered for a method is cached under its key, and the
  // callback checks the key it started under against the gate in force: a
  // key that did not tell two methods apart would hand one provider's
  // runtime to another's gate, and admit a sign-in the gate no longer asks
  // for.
  let key = |yaml: &str| policy(yaml).sso_methods().next().unwrap().key();
  let a = key(SSO);
  let same = key(
    "{method: sso, issuer: https://id.example.com/, client_id: visitors, client_secret: other, allowed_emails: \"*@elsewhere.test\"}",
  );
  let other_provider = key(
    "{method: sso, issuer: https://other.example.com, client_id: visitors, client_secret: s3cret, allowed_emails: \"*@example.com\"}",
  );
  let other_client = key(
    "{method: sso, issuer: https://id.example.com, client_id: staff, client_secret: s3cret, allowed_emails: \"*@example.com\"}",
  );
  assert!(!a.is_empty());
  assert_eq!(
    a, same,
    "a trailing slash, the secret and the list are not identity"
  );
  assert_ne!(a, other_provider);
  assert_ne!(a, other_client);
}
