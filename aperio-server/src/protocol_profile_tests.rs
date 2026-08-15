//! Tests that the written minimum stays true: every message classified, every
//! message documented, and the profile itself small enough to be a profile.

use super::*;

/// The document and the table describe the same protocol.
///
/// This is the half the compiler cannot do. `reach` is exhaustive, so a new
/// variant cannot go unclassified; nothing but this stops it going
/// undocumented, and a written minimum that omits a message type is worse
/// than none, because a device implementer reads it as complete.
#[test]
fn the_document_lists_every_message_the_protocol_has() {
  let doc = include_str!("../../docs/embedded-profile.md");
  let missing: Vec<String> = variant_names()
    .into_iter()
    .filter(|name| !doc.contains(&format!("`{name}`")))
    .collect();
  assert!(
    missing.is_empty(),
    "docs/embedded-profile.md does not mention: {missing:?}. \
     A new message type has to be classified for a device, not only added."
  );
}

/// And the document does not invent messages the protocol does not have,
/// which is how a spec drifts in the other direction: a device implementer
/// writing handlers for something nobody sends.
#[test]
fn the_document_invents_no_messages() {
  let doc = include_str!("../../docs/embedded-profile.md");
  let known = variant_names();
  // Backticked CamelCase words in the table rows are message names.
  let mut invented = Vec::new();
  for line in doc.lines().filter(|l| l.starts_with("| `")) {
    let name: String = line
      .trim_start_matches("| `")
      .chars()
      .take_while(|c| c.is_ascii_alphanumeric())
      .collect();
    if !name.is_empty() && !known.contains(&name) {
      invented.push(name);
    }
  }
  assert!(invented.is_empty(), "not in the protocol: {invented:?}");
}

/// Reading the enum out of the source is a small parser, and a small parser
/// that silently found nothing would make both tests above pass forever.
#[test]
fn the_variant_list_is_read_correctly() {
  let names = variant_names();
  assert!(
    names.len() > 30,
    "expected the whole protocol, got {}: {names:?}",
    names.len()
  );
  for expected in [
    "Ping",
    "Pong",
    "Request",
    "Response",
    "StreamPause",
    "PublishRefused",
  ] {
    assert!(names.contains(&expected.to_string()), "missing {expected}");
  }
  assert!(
    !names.iter().any(|n| n == "Some" || n == "None"),
    "the parser picked up something that is not a variant: {names:?}"
  );
}
