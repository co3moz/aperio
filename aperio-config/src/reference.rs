//! The settings tables of `docs/reference.md`, rendered from the schemas.
//!
//! A setting is described once, in the doc comment on its field. The JSON
//! Schema carries that text to an editor and to the dashboard's settings form,
//! and this module lays the same text out as the reference page, so the page
//! cannot say anything the schema does not. Written by hand, the tables were a
//! second and a third copy of every description, and a check could only ask
//! whether a row existed, never whether it was still true.
//!
//! `reference_tests.rs` holds the page to the schema, and rewrites it when run
//! with `APERIO_BLESS=1`.

use serde_json::Value as Json;

use crate::layers::{self, Scope};

/// What renders one region of the page.
pub type Render = fn() -> String;

/// The generated regions of `docs/reference.md`, by marker name, with what
/// fills each.
pub const REGIONS: &[(&str, Render)] = &[
  ("client-settings", client_reference),
  ("server-settings", server_reference),
];

/// `doc` with the region `name` replaced by `body`, or `None` when the page
/// has no such region.
pub fn splice(doc: &str, name: &str, body: &str) -> Option<String> {
  let open = format!("<!-- BEGIN GENERATED {name} -->");
  let close = format!("<!-- END GENERATED {name} -->");
  let start = doc.find(&open)? + open.len();
  let end = doc[start..].find(&close)? + start;
  Some(format!(
    "{}\n\n{}\n\n{}",
    &doc[..start],
    body.trim(),
    &doc[end..]
  ))
}

/// A description as one table cell. The doc comment's line breaks are where
/// its source wrapped, not where a reader should, and a `|` would end the cell.
fn cell(text: &str) -> String {
  text
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
    .replace('|', "\\|")
}

/// A description as prose under a heading: the same text, paragraphs kept.
fn prose(text: &str) -> String {
  text
    .split("\n\n")
    .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
    .filter(|p| !p.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// The schema node a `$ref` names, followed through.
fn deref<'a>(root: &'a Json, node: &'a Json) -> &'a Json {
  match node.get("$ref").and_then(Json::as_str) {
    Some(r) => r
      .rsplit('/')
      .next()
      .and_then(|name| root["$defs"].get(name))
      .unwrap_or(node),
    None => node,
  }
}

/// The struct a property holds, when it holds one: directly, by `$ref`, or as
/// the one non-null shape of an optional field. A map or a list is not one.
fn block<'a>(root: &'a Json, node: &'a Json) -> Option<&'a Json> {
  let target = deref(root, node);
  if target.get("properties").is_some() {
    return Some(target);
  }
  let shapes: Vec<&Json> = ["anyOf", "oneOf"]
    .iter()
    .filter_map(|k| target.get(*k).and_then(Json::as_array))
    .flatten()
    .filter(|s| s.get("type").and_then(Json::as_str) != Some("null"))
    .collect();
  match shapes.as_slice() {
    [only] => {
      let inner = deref(root, only);
      inner.get("properties").is_some().then_some(inner)
    }
    _ => None,
  }
}

/// A property's description: its own, or the one on the type it names.
fn describe(root: &Json, node: &Json) -> String {
  node
    .get("description")
    .or_else(|| deref(root, node).get("description"))
    .and_then(Json::as_str)
    .unwrap_or_default()
    .to_string()
}

/// The text without the sentence that states the default, for a table whose
/// own column states it from the code. A sentence ends at a full stop outside
/// a code span, so `0.5` inside backticks does not end one.
fn without_default(text: &str) -> String {
  let mut sentences: Vec<String> = Vec::new();
  let mut current = String::new();
  let mut in_code = false;
  let chars: Vec<char> = text.chars().collect();
  for (i, c) in chars.iter().enumerate() {
    current.push(*c);
    if *c == '`' {
      in_code = !in_code;
    }
    let ends = *c == '.' && !in_code && chars.get(i + 1).is_none_or(|n| n.is_whitespace());
    if ends {
      sentences.push(std::mem::take(&mut current));
    }
  }
  sentences.push(current);
  sentences
    .into_iter()
    .map(|s| s.trim().to_string())
    .filter(|s| {
      !s.is_empty()
        && !s
          .strip_prefix("Default")
          .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == ':' || c == '`'))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// A default value as a table cell.
fn default_cell(value: Option<&Json>) -> String {
  match value {
    None | Some(Json::Null) => String::new(),
    Some(Json::String(s)) if s.is_empty() => "`\"\"`".to_string(),
    Some(Json::String(s)) => format!("`{}`", s.replace('|', "\\|")),
    Some(other) => format!("`{}`", other.to_string().replace('|', "\\|")),
  }
}

/// Every row of a section: a field per row, and a block's fields under it.
fn server_rows(
  root: &Json,
  def: &Json,
  defaults: Option<&Json>,
  prefix: &str,
  rows: &mut Vec<[String; 3]>,
) {
  for (key, child) in def["properties"].as_object().into_iter().flatten() {
    let path = format!("{prefix}.{key}");
    let default = child
      .get("default")
      .or_else(|| defaults.and_then(|d| d.get(key)));
    let text = cell(&without_default(&describe(root, child)));
    match block(root, child) {
      Some(inner) => {
        rows.push([format!("`{path}`"), text, String::new()]);
        server_rows(root, inner, default, &path, rows);
      }
      None => rows.push([format!("`{path}`"), text, default_cell(default)]),
    }
  }
}

fn table(head: &[&str], rows: &[Vec<String>]) -> String {
  let mut out = format!(
    "| {} |\n|{}\n",
    head.join(" | "),
    " --- |".repeat(head.len())
  );
  for row in rows {
    out.push_str(&format!("| {} |\n", row.join(" | ")));
  }
  out
}

/// The server's settings document, one section per heading.
pub fn server_reference() -> String {
  let root: Json =
    serde_json::from_str(&crate::server_schema_json()).expect("the server schema is valid JSON");
  let mut out = String::new();
  for (section, node) in root["properties"].as_object().into_iter().flatten() {
    out.push_str(&format!("### `{section}`\n\n"));
    let text = prose(&describe(&root, node));
    match block(&root, node) {
      Some(def) => {
        if !text.is_empty() {
          out.push_str(&format!("{text}\n\n"));
        }
        let mut rows = Vec::new();
        server_rows(&root, def, node.get("default"), section, &mut rows);
        let rows: Vec<Vec<String>> = rows.into_iter().map(Vec::from).collect();
        out.push_str(&table(&["Setting", "Description", "Default"], &rows));
      }
      // A list or a map at the top (`sites`): there are no fields to tabulate,
      // and its shape is written up where it can be shown as yaml.
      None => out.push_str(&format!("{}\n", without_default(&text))),
    }
    out.push('\n');
  }
  out
}

/// The client's settings: what the environment can write, grouped by where
/// the key lives, then what only a file can say.
pub fn client_reference() -> String {
  let root = layers::client_schema();
  let mut out = String::new();
  let groups = [
    (
      Scope::File,
      "### The client",
      "What the client as a whole is, and how it reaches its server.",
    ),
    (
      Scope::Service,
      "### A service: where its traffic goes",
      "The one service of the command line and the environment, or a key of a `services:` entry.",
    ),
    (
      Scope::Defaults,
      "### A service: what it does with its traffic",
      "Written under `defaults:` for every service, or on one `services:` entry, which wins.",
    ),
  ];
  for (scope, heading, lead) in groups {
    out.push_str(&format!("{heading}\n\n{lead}\n\n"));
    let rows: Vec<Vec<String>> = layers::client_env()
      .iter()
      .filter(|s| s.scope == scope)
      .map(|s| {
        vec![
          format!("`{}`", s.var),
          format!("`{}`", s.path),
          cell(&describe(root, s.schema())),
        ]
      })
      .collect();
    out.push_str(&table(&["Variable", "Key", "Description"], &rows));
    out.push('\n');
  }

  out.push_str(
    "### Written only in a file\n\n\
     Maps of entries and the keys inside them, which have no flat spelling.\n\n",
  );
  let mut rows: Vec<Vec<String>> = Vec::new();
  let props = &root["properties"];
  let mut row =
    |key: String, node: &Json| rows.push(vec![format!("`{key}`"), cell(&describe(root, node))]);
  for key in ["defaults", "services", "include"] {
    row(key.to_string(), &props[key]);
  }
  let entry = &root["$defs"]["ServiceEntry"]["properties"];
  let policy = &root["$defs"]["ServicePolicy"]["properties"];
  for (key, node) in entry.as_object().into_iter().flatten() {
    if !layers::SERVICE_KEYS.contains(&key.as_str()) && policy.get(key).is_none() {
      row(format!("services.<name>.{key}"), node);
    }
  }
  for (key, def, spelled) in [
    ("tunnels", "TunnelEntry", "tunnels.<name>"),
    ("binds", "BindEntry", "binds.<name>"),
    ("subscribe", "SubscribeEntry", "subscribe[]"),
  ] {
    if key != "subscribe" {
      row(key.to_string(), &props[key]);
    }
    for (field, node) in root["$defs"][def]["properties"]
      .as_object()
      .into_iter()
      .flatten()
    {
      row(format!("{spelled}.{field}"), node);
    }
  }
  out.push_str(&table(&["Key", "Description"], &rows));
  out
}

#[cfg(test)]
#[path = "reference_tests.rs"]
mod tests;
