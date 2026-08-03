//! What these pin down: that templating never rewrites something it was not
//! asked to, and that a suggestion is only offered when it is worth acting on.

use super::*;

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
  move |name| {
    pairs
      .iter()
      .find(|(k, _)| *k == name)
      .map(|(_, v)| (*v).to_string())
  }
}

#[test]
fn expands_the_one_spelling_it_claims_to() {
  let vars = env(&[("ENV", "prod"), ("REGION", "eu")]);
  assert_eq!(
    expand_vars("hostname: ${ENV}.example.com", &vars).unwrap(),
    "hostname: prod.example.com"
  );
  assert_eq!(
    expand_vars("a: ${ENV}-${REGION}", &vars).unwrap(),
    "a: prod-eu"
  );
}

#[test]
fn leaves_a_bare_dollar_alone() {
  let vars = env(&[("ENV", "prod")]);
  // `$` appears in generated passwords, regular expressions and shell
  // snippets inside `run:`. Rewriting those to make templating prettier would
  // corrupt files that work today.
  for text in [
    "password: hunter$ENV2",
    "regex: ^/api/.*$",
    "run: echo $HOME > /tmp/x",
    "psk: a$b$c",
  ] {
    assert_eq!(expand_vars(text, &vars).unwrap(), text);
  }
}

#[test]
fn an_unset_variable_is_an_error_not_an_empty_string() {
  let vars = env(&[]);
  // Substituting nothing produces a file that parses and means something
  // else: `hostname: .example.com`, or a token that is the empty string.
  let err = expand_vars("hostname: ${ENV}.example.com", &vars).unwrap_err();
  assert!(err.contains("ENV"), "{err}");
  assert!(
    err.contains(":-"),
    "the error names the escape hatch: {err}"
  );
}

#[test]
fn a_default_covers_an_unset_variable() {
  let vars = env(&[]);
  assert_eq!(
    expand_vars("hostname: ${ENV:-dev}.example.com", &vars).unwrap(),
    "hostname: dev.example.com"
  );
  // An empty default is a deliberate empty string, unlike an absent one.
  assert_eq!(expand_vars("a: '${X:-}'", &vars).unwrap(), "a: ''");
}

#[test]
fn an_environment_value_beats_the_default() {
  let vars = env(&[("ENV", "prod")]);
  assert_eq!(expand_vars("${ENV:-dev}", &vars).unwrap(), "prod");
}

#[test]
fn a_malformed_reference_is_refused() {
  let vars = env(&[]);
  assert!(expand_vars("a: ${ENV", &vars).is_err());
  assert!(expand_vars("a: ${}", &vars).is_err());
  assert!(expand_vars("a: ${not a name}", &vars).is_err());
}

#[test]
fn multibyte_text_survives_expansion() {
  let vars = env(&[("ENV", "prod")]);
  // Byte indexing over a char boundary is the classic way a scanner like this
  // panics, and config files carry Turkish, Japanese and emoji in custom names.
  assert_eq!(
    expand_vars("custom_name: 'Müşteri ${ENV} 世界 🎉'", &vars).unwrap(),
    "custom_name: 'Müşteri prod 世界 🎉'"
  );
}

const KEYS: &[&str] = &[
  "hostname",
  "target",
  "connections",
  "max_concurrent",
  "security_headers",
  "tls",
  "path",
];

#[test]
fn suggests_the_key_that_was_meant() {
  assert_eq!(suggest("hostnme", KEYS.iter().copied()), Some("hostname"));
  assert_eq!(
    suggest("conections", KEYS.iter().copied()),
    Some("connections")
  );
  assert_eq!(
    suggest("max_concurent", KEYS.iter().copied()),
    Some("max_concurrent")
  );
}

#[test]
fn treats_a_dash_or_a_capital_as_the_key_itself() {
  // These are not guesses: somebody wrote the right key in the wrong style.
  assert_eq!(
    suggest("max-concurrent", KEYS.iter().copied()),
    Some("max_concurrent")
  );
  assert_eq!(suggest("Hostname", KEYS.iter().copied()), Some("hostname"));
  assert_eq!(
    suggest("SECURITY-HEADERS", KEYS.iter().copied()),
    Some("security_headers")
  );
}

#[test]
fn stays_quiet_when_nothing_is_close() {
  // A wrong suggestion is worse than none: it sends somebody to change a key
  // that was never the problem.
  assert_eq!(suggest("completely_different", KEYS.iter().copied()), None);
  assert_eq!(suggest("backend_url", KEYS.iter().copied()), None);
}

#[test]
fn never_guesses_at_a_short_name() {
  // `tls` and `ttl` are one letter apart and mean nothing like each other; a
  // three-letter name has too few letters for a near miss to carry meaning.
  assert_eq!(suggest("ttl", KEYS.iter().copied()), None);
  assert_eq!(suggest("tcp", KEYS.iter().copied()), None);
}

#[test]
fn the_known_keys_come_from_the_schema_and_are_not_empty() {
  let (top, service) = known_keys();
  // Read from the generated schema rather than listed by hand, so a key added
  // next release is understood without anyone remembering to add it here.
  assert!(top.contains(&"services".to_string()), "{top:?}");
  assert!(top.contains(&"connections".to_string()));
  assert!(service.contains(&"target".to_string()), "{service:?}");
  assert!(service.contains(&"depends_on".to_string()));
}
