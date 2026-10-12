//! Turning what a client reads, two files, the environment and the command
//! line, into one configuration, by one rule for every key.
//!
//! The rule is a merge. Each layer is a yaml document; a later layer is laid
//! over the earlier ones, mappings merge key by key, and anything else (a
//! scalar, a list) is replaced. The order is the one a container user
//! expects: built-in defaults, `~/.aperio.yaml`, `./aperio.yaml`, the
//! environment, the command line.
//!
//! The environment is not read key by key in code. Its variables are derived
//! from the schema, one per scalar setting, so a key and its variable cannot
//! drift apart and a new key has a variable the day it is written:
//!
//! * a key of the document is `APERIO_<PATH>` (`server.url` is
//!   `APERIO_SERVER_URL`),
//! * a key of `defaults:` is written without the prefix (`defaults.timeout` is
//!   `APERIO_TIMEOUT`), since that is where one service's settings go when
//!   there is no file to put them in,
//! * the keys that say where one service's traffic comes from and goes to
//!   (`target`, `hostname`, ...) describe the single service the command line
//!   and the environment can run without a file.

use serde_json::Value as Json;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::{FileConfig, ServiceEntry};

/// Where a value came from, lowest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
  /// `~/.aperio.yaml`.
  Home,
  /// `./aperio.yaml`, or the `--config` file.
  Local,
  /// An `APERIO_*` variable.
  Env,
  /// A command-line argument.
  Cli,
}

impl Layer {
  /// How `check` names the layer.
  pub fn label(&self) -> &'static str {
    match self {
      Layer::Home => "~/.aperio.yaml",
      Layer::Local => "the config file",
      Layer::Env => "the environment",
      Layer::Cli => "the command line",
    }
  }
}

/// Which part of the configuration a variable or a flag writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
  /// A key of the document: `server.url`, `name`.
  File,
  /// A key of `defaults:`.
  Defaults,
  /// A key of the one service the command line and the environment describe.
  Service,
}

/// One setting the environment can write.
#[derive(Clone, Debug)]
pub struct EnvSetting {
  /// The variable, `APERIO_SERVER_URL`.
  pub var: String,
  /// The key it writes, relative to its scope: `server.url`, `timeout`.
  pub path: String,
  /// Which part of the configuration the path is in.
  pub scope: Scope,
  /// The key's schema, which says how a variable's text becomes a value.
  schema: Json,
}

impl EnvSetting {
  /// The key as a file writes it: `defaults.timeout`, `server.url`, or the
  /// service key bare.
  pub fn key(&self) -> String {
    match self.scope {
      Scope::Defaults => format!("defaults.{}", self.path),
      _ => self.path.clone(),
    }
  }
}

/// The keys of a service that say where its traffic comes from and goes to.
/// On the command line and in the environment they describe the one service
/// those two can run; in a file they belong to a `services:` entry.
pub const SERVICE_KEYS: &[&str] = &[
  "target",
  "serve",
  "tcp_target",
  "hostname",
  "path",
  "custom_name",
];

/// Top-level keys the environment does not write. A map of entries has no
/// flat spelling, and `include` is relative to the file that writes it.
const NOT_FROM_ENV: &[&str] = &["defaults", "services", "tunnels", "binds", "include"];

/// The client's schema as JSON, generated once.
pub fn client_schema() -> &'static Json {
  static SCHEMA: OnceLock<Json> = OnceLock::new();
  SCHEMA.get_or_init(|| serde_json::from_str(&crate::schema_json()).unwrap_or(Json::Null))
}

/// Every variable the client reads, in schema order.
pub fn client_env() -> &'static [EnvSetting] {
  static TABLE: OnceLock<Vec<EnvSetting>> = OnceLock::new();
  TABLE.get_or_init(|| {
    let root = client_schema();
    let mut out = Vec::new();
    if let Some(props) = root.get("properties").and_then(Json::as_object) {
      for (key, node) in props {
        if NOT_FROM_ENV.contains(&key.as_str()) {
          continue;
        }
        leaves(root, node, key, &mut |path, schema| {
          out.push(EnvSetting {
            var: env_var(path),
            path: path.to_string(),
            scope: Scope::File,
            schema: schema.clone(),
          })
        });
      }
    }
    if let Some(policy) = def(root, "ServicePolicy").and_then(|d| d.get("properties")) {
      for (key, node) in policy.as_object().into_iter().flatten() {
        leaves(root, node, key, &mut |path, schema| {
          out.push(EnvSetting {
            var: env_var(path),
            path: path.to_string(),
            scope: Scope::Defaults,
            schema: schema.clone(),
          })
        });
      }
    }
    if let Some(entry) = def(root, "ServiceEntry").and_then(|d| d.get("properties")) {
      for key in SERVICE_KEYS {
        if let Some(node) = entry.get(*key) {
          out.push(EnvSetting {
            var: env_var(key),
            path: key.to_string(),
            scope: Scope::Service,
            schema: node.clone(),
          });
        }
      }
    }
    out
  })
}

/// The keys a running client applies only when it starts, marked in the schema
/// with `x-reload: restart`: what dials the server, the process's identity,
/// the OTel bridge, logging. A reload that changes one says so rather than
/// reporting a change it did not make.
pub fn restart_paths() -> &'static [String] {
  static PATHS: OnceLock<Vec<String>> = OnceLock::new();
  PATHS.get_or_init(|| {
    let root = client_schema();
    let mut out = Vec::new();
    fn walk(root: &Json, node: &Json, path: &str, out: &mut Vec<String>) {
      if node.get("x-reload").and_then(Json::as_str) == Some("restart") {
        out.push(path.to_string());
        return;
      }
      for block in variants(root, node) {
        for (key, child) in block
          .get("properties")
          .and_then(Json::as_object)
          .into_iter()
          .flatten()
        {
          walk(root, child, &format!("{path}.{key}"), out);
        }
      }
    }
    for (key, node) in root
      .get("properties")
      .and_then(Json::as_object)
      .into_iter()
      .flatten()
    {
      walk(root, node, key, &mut out);
    }
    out
  })
}

/// The values of the [`restart_paths`] in `config`, to compare across a
/// reload.
pub fn restart_values(config: &FileConfig) -> BTreeMap<String, Json> {
  let doc = serde_json::to_value(config).unwrap_or(Json::Null);
  restart_paths()
    .iter()
    .map(|path| {
      let value = path
        .split('.')
        .try_fold(&doc, |node, part| node.get(part))
        .cloned()
        .unwrap_or(Json::Null);
      (path.clone(), value)
    })
    .collect()
}

/// The restart-only keys whose value differs between two configurations.
pub fn restart_changes(
  before: &BTreeMap<String, Json>,
  after: &BTreeMap<String, Json>,
) -> Vec<String> {
  restart_paths()
    .iter()
    .filter(|path| before.get(*path) != after.get(*path))
    .cloned()
    .collect()
}

/// The variable for a key: `server.url` is `APERIO_SERVER_URL`. `log_level`
/// is the bare `LOG_LEVEL`, the one name every program already reads.
pub fn env_var(path: &str) -> String {
  if path == "log_level" {
    return "LOG_LEVEL".to_string();
  }
  format!(
    "APERIO_{}",
    path.replace(['.', '-'], "_").to_ascii_uppercase()
  )
}

/// A definition of the schema by name.
fn def<'a>(root: &'a Json, name: &str) -> Option<&'a Json> {
  root.get("$defs").and_then(|d| d.get(name))
}

/// Follows a `$ref` to its definition.
fn resolve<'a>(root: &'a Json, node: &'a Json) -> &'a Json {
  match node.get("$ref").and_then(Json::as_str) {
    Some(r) => r
      .strip_prefix("#/$defs/")
      .and_then(|name| def(root, name))
      .unwrap_or(node),
    None => node,
  }
}

/// The shapes a value may take: refs followed, `anyOf`/`oneOf` expanded, and
/// `null` (which is how an optional key says "may be absent") left out.
fn variants<'a>(root: &'a Json, node: &'a Json) -> Vec<&'a Json> {
  let node = resolve(root, node);
  let nested = node
    .get("anyOf")
    .or_else(|| node.get("oneOf"))
    .and_then(Json::as_array);
  match nested {
    Some(list) => list.iter().flat_map(|v| variants(root, v)).collect(),
    None if node.get("type").and_then(Json::as_str) == Some("null") => Vec::new(),
    None => vec![node],
  }
}

/// The JSON types a value may have, without `null`.
fn types(root: &Json, node: &Json) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  for v in variants(root, node) {
    match v.get("type") {
      Some(Json::String(t)) => out.push(t.clone()),
      Some(Json::Array(ts)) => out.extend(ts.iter().filter_map(Json::as_str).map(str::to_string)),
      _ if v.get("const").is_some() || v.get("enum").is_some() => out.push("string".into()),
      _ if v.get("properties").is_some() => out.push("object".into()),
      _ => {}
    }
  }
  out.retain(|t| t != "null");
  out.dedup();
  out
}

/// The object shapes a value may take, as `(properties, additionalProperties)`.
fn objects<'a>(root: &'a Json, node: &'a Json) -> Vec<&'a Json> {
  variants(root, node)
    .into_iter()
    .filter(|v| {
      v.get("properties").is_some()
        || v.get("additionalProperties").is_some()
        || v.get("type").and_then(Json::as_str) == Some("object")
    })
    .collect()
}

/// True for a value the schema marks as written whole (`x-env: whole`),
/// looked for through the `anyOf` an optional key is wrapped in.
fn written_whole(root: &Json, node: &Json) -> bool {
  let marked = |n: &Json| resolve(root, n).get("x-env").and_then(Json::as_str) == Some("whole");
  marked(node)
    || node
      .get("anyOf")
      .and_then(Json::as_array)
      .is_some_and(|list| list.iter().any(marked))
}

/// Calls `found` for every key under `node` that has a variable of its own:
/// a scalar, a list of scalars, a map of strings, or a value with a scalar
/// spelling. A block that is only a block has none, its children do.
fn leaves(root: &Json, node: &Json, path: &str, found: &mut dyn FnMut(&str, &Json)) {
  if written_whole(root, node) {
    found(path, node);
    return;
  }
  let shapes = variants(root, node);
  let blocks: Vec<&Json> = shapes
    .iter()
    .copied()
    .filter(|v| v.get("properties").is_some())
    .collect();
  let scalar = shapes.iter().any(|v| v.get("properties").is_none());
  if scalar || blocks.is_empty() {
    found(path, node);
  }
  for block in blocks {
    for (key, child) in block["properties"].as_object().into_iter().flatten() {
      leaves(root, child, &format!("{path}.{key}"), found);
    }
  }
}

/// Reads one variable's text as the value its key takes.
///
/// A value written as yaml (`{method: basic, ...}`, `[a, b]`) is read as yaml,
/// so even a block can be given whole. Otherwise the key's type decides: a
/// boolean takes `1`/`0`/`true`/`false`, a list is comma-separated, a map of
/// strings is `k=v,k=v`, and a number is a number.
pub fn parse_env_value(setting: &EnvSetting, raw: &str) -> Result<Value, String> {
  let root = client_schema();
  let text = raw.trim();
  if text.starts_with('{') || text.starts_with('[') {
    return serde_yaml::from_str(text).map_err(|e| format!("not valid yaml: {e}"));
  }
  let types = types(root, &setting.schema);
  let has = |t: &str| types.iter().any(|x| x == t);
  if has("boolean") {
    match text.to_ascii_lowercase().as_str() {
      "1" | "true" | "yes" | "on" => return Ok(Value::Bool(true)),
      "0" | "false" | "no" | "off" => return Ok(Value::Bool(false)),
      _ if types.len() == 1 => {
        return Err(format!("`{text}` is not true or false (write 1 or 0)"));
      }
      _ => {}
    }
  }
  if has("integer")
    && let Ok(n) = text.parse::<i64>()
  {
    return Ok(Value::Number(n.into()));
  }
  if has("number")
    && let Ok(n) = text.parse::<f64>()
  {
    return Ok(Value::Number(n.into()));
  }
  if has("array") && (!has("string") || text.contains(',')) {
    return Ok(Value::Sequence(
      text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_string()))
        .collect(),
    ));
  }
  if has("object") && !has("string") {
    let mut map = Mapping::new();
    for pair in text.split(',').filter(|p| !p.trim().is_empty()) {
      let (k, v) = pair
        .split_once('=')
        .ok_or_else(|| format!("`{pair}` is not key=value (write k=v,k=v or a yaml mapping)"))?;
      map.insert(
        Value::String(k.trim().to_string()),
        Value::String(v.trim().to_string()),
      );
    }
    return Ok(Value::Mapping(map));
  }
  if has("string") {
    return Ok(Value::String(text.to_string()));
  }
  Err(format!(
    "`{text}` is not a value this key takes; write it as yaml"
  ))
}

/// What a layer above the files writes: keys of the document (including
/// `defaults:`), and keys of the one service it describes.
#[derive(Default, Debug, Clone)]
pub struct Overlay {
  /// Keys of the document.
  pub file: Mapping,
  /// Keys of the one service the command line and the environment describe.
  pub service: Mapping,
}

impl Overlay {
  /// Writes `value` at `path` in `scope`.
  pub fn set(&mut self, scope: Scope, path: &str, value: Value) {
    match scope {
      Scope::File => set_path(&mut self.file, path, value),
      Scope::Defaults => set_path(&mut self.file, &format!("defaults.{path}"), value),
      Scope::Service => set_path(&mut self.service, path, value),
    }
  }
}

/// The environment as an overlay. Refuses a variable this client no longer
/// reads, naming the one to use instead: a variable that is silently ignored
/// is a setting an operator believes is in force.
pub fn env_overlay(lookup: impl Fn(&str) -> Option<String>) -> Result<Overlay, String> {
  let set = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
  let removed: Vec<String> = crate::removed::REMOVED_CLIENT_ENV
    .iter()
    .filter(|r| set(r.key).is_some())
    .map(|r| format!("{} {}", r.key, r.hint))
    .collect();
  if !removed.is_empty() {
    return Err(removed.join("\n"));
  }
  let mut overlay = Overlay::default();
  for setting in client_env() {
    let Some(raw) = set(&setting.var) else {
      continue;
    };
    let value = parse_env_value(setting, &raw).map_err(|e| format!("{}: {e}", setting.var))?;
    overlay.set(setting.scope, &setting.path, value);
  }
  Ok(overlay)
}

/// Writes `value` at a dotted path, making the mappings on the way.
pub fn set_path(doc: &mut Mapping, path: &str, value: Value) {
  let mut parts = path.split('.').peekable();
  let mut node = doc;
  while let Some(part) = parts.next() {
    let key = Value::String(part.to_string());
    if parts.peek().is_none() {
      node.insert(key, value);
      return;
    }
    let slot = node
      .entry(key)
      .or_insert_with(|| Value::Mapping(Mapping::new()));
    if !slot.is_mapping() {
      *slot = Value::Mapping(Mapping::new());
    }
    node = slot.as_mapping_mut().expect("just made a mapping");
  }
}

/// Lays `overlay` over `base`: mappings merge key by key, anything else is
/// replaced.
pub fn merge(base: &mut Mapping, overlay: Mapping) {
  for (key, value) in overlay {
    match (base.get_mut(&key), value) {
      (Some(Value::Mapping(existing)), Value::Mapping(incoming)) => merge(existing, incoming),
      (_, value) => {
        base.insert(key, value);
      }
    }
  }
}

/// The paths of every value in `doc`, for saying where a setting came from.
fn record(doc: &Mapping, prefix: &str, layer: Layer, out: &mut BTreeMap<String, Layer>) {
  for (key, value) in doc {
    let Some(key) = key.as_str() else { continue };
    let path = if prefix.is_empty() {
      key.to_string()
    } else {
      format!("{prefix}.{key}")
    };
    match value {
      Value::Mapping(inner) if !inner.is_empty() => record(inner, &path, layer, out),
      _ => {
        out.insert(path, layer);
      }
    }
  }
}

/// Every layer read, merged and validated.
#[derive(Debug, Clone, Default)]
pub struct Assembled {
  /// The document, with every `services:` entry laid over `defaults:`.
  pub config: FileConfig,
  /// The one service the command line or the environment describes, laid
  /// over `defaults:`, when one of them names a target.
  pub service: Option<ServiceEntry>,
  /// Which layer each value came from, by path. The command-line service's
  /// keys are under `service.`.
  pub sources: BTreeMap<String, Layer>,
}

/// Merges the layers, lowest first, into one configuration.
///
/// Each file is expected to have passed [`check_document`] already, so a
/// mistake is reported against the file that made it.
pub fn assemble(
  files: Vec<(Layer, Mapping)>,
  overlays: Vec<(Layer, Overlay)>,
) -> Result<Assembled, String> {
  let mut doc = Mapping::new();
  let mut service = Mapping::new();
  let mut sources = BTreeMap::new();
  for (layer, file) in files {
    record(&file, "", layer, &mut sources);
    merge(&mut doc, file);
  }
  for (layer, overlay) in overlays {
    record(&overlay.file, "", layer, &mut sources);
    record(&overlay.service, "service", layer, &mut sources);
    merge(&mut doc, overlay.file);
    merge(&mut service, overlay.service);
  }
  doc.remove("include");
  doc.retain(|k, _| !k.as_str().is_some_and(|k| k.starts_with("x-")));

  let defaults = match doc.get("defaults") {
    Some(Value::Mapping(m)) => m.clone(),
    _ => Mapping::new(),
  };
  if let Some(Value::Mapping(services)) = doc.get_mut("services") {
    for (_, entry) in services.iter_mut() {
      let own = match std::mem::take(entry) {
        Value::Mapping(m) => m,
        _ => Mapping::new(),
      };
      let mut laid = defaults.clone();
      merge(&mut laid, own);
      *entry = Value::Mapping(laid);
    }
  }

  let config = deserialize_config(&doc)?;
  let named = |key: &str| service.get(key).is_some_and(|v| !v.is_null());
  let service = if named("target") || named("serve") || named("tcp_target") {
    let mut laid = defaults;
    merge(&mut laid, service);
    Some(
      serde_yaml::from_value::<ServiceEntry>(Value::Mapping(laid))
        .map_err(|e| format!("the service given on the command line or in the environment: {e}"))?,
    )
  } else if let Some(key) = SERVICE_KEYS.iter().find(|k| named(k)) {
    return Err(format!(
      "`{key}` on the command line or in the environment describes a single service, which \
       needs a target: give one as an argument, or set {} or {}",
      env_var("target"),
      env_var("serve")
    ));
  } else {
    None
  };
  Ok(Assembled {
    config,
    service,
    sources,
  })
}

/// Deserializes the merged document, and when that fails, finds the key that
/// made it fail so the message can name it: a type error deep inside forty
/// services is no help without the service.
fn deserialize_config(doc: &Mapping) -> Result<FileConfig, String> {
  let whole = match serde_yaml::from_value::<FileConfig>(Value::Mapping(doc.clone())) {
    Ok(config) => return Ok(config),
    Err(e) => e.to_string(),
  };
  for (key, value) in doc {
    let Some(name) = key.as_str() else { continue };
    if name == "services"
      && let Value::Mapping(entries) = value
    {
      for (service, entry) in entries {
        if let Err(e) = serde_yaml::from_value::<ServiceEntry>(entry.clone()) {
          return Err(format!("services.{}: {e}", service.as_str().unwrap_or("?")));
        }
      }
    }
    let mut alone = Mapping::new();
    alone.insert(key.clone(), value.clone());
    if let Err(e) = serde_yaml::from_value::<FileConfig>(Value::Mapping(alone)) {
      return Err(format!("{name}: {e}"));
    }
  }
  Err(whole)
}

/// Reads a document's text into a mapping: yaml merge keys (`<<: *common`)
/// applied, and a file that is empty read as no settings at all.
pub fn parse_document(text: &str) -> Result<Mapping, String> {
  let mut value: Value = serde_yaml::from_str(text).map_err(|e| format!("invalid yaml: {e}"))?;
  value
    .apply_merge()
    .map_err(|e| format!("invalid yaml merge: {e}"))?;
  match value {
    Value::Mapping(m) => Ok(m),
    Value::Null => Ok(Mapping::new()),
    _ => Err("must be a mapping of settings".to_string()),
  }
}

/// Checks a file's keys against the schema: a key nothing reads is an error
/// naming the key it was probably meant to be, or, for a key an earlier
/// release read, what replaced it. `origin` names the file in the message.
///
/// Top-level keys starting with `x-` are left alone, the place for yaml
/// anchors a file reuses (`x-common: &common`), as in compose files.
pub fn check_document(doc: &Mapping, origin: &str) -> Result<(), String> {
  let root = client_schema();
  let mut errors = Vec::new();
  for (key, value) in doc {
    let Some(name) = key.as_str() else {
      errors.push(format!("{origin}: a key must be a string"));
      continue;
    };
    if name.starts_with("x-") {
      continue;
    }
    if let Some(hint) = crate::removed::shape_hint(name, value) {
      errors.push(format!("{origin}: `{name}` {hint}"));
      continue;
    }
    match root.get("properties").and_then(|p| p.get(name)) {
      Some(node) => match name {
        "services" => {
          if let Value::Mapping(entries) = value {
            for (service, entry) in entries {
              let service = service.as_str().unwrap_or("?");
              check_service(
                root,
                entry,
                &format!("services.{service}"),
                origin,
                &mut errors,
              );
            }
          }
        }
        "defaults" => check_service(root, value, "defaults", origin, &mut errors),
        _ => check_value(root, node, value, name, origin, &mut errors),
      },
      None => errors.push(unknown(root, root, name, name, origin)),
    }
  }
  if errors.is_empty() {
    Ok(())
  } else {
    Err(errors.join("\n"))
  }
}

/// A `services:` entry or `defaults:`, whose keys have their own hints.
fn check_service(root: &Json, value: &Value, at: &str, origin: &str, errors: &mut Vec<String>) {
  let Value::Mapping(entry) = value else {
    return;
  };
  let schema = if at == "defaults" {
    def(root, "ServicePolicy")
  } else {
    def(root, "ServiceEntry")
  };
  let Some(schema) = schema else { return };
  for (key, value) in entry {
    let Some(name) = key.as_str() else { continue };
    let path = format!("{at}.{name}");
    match schema.get("properties").and_then(|p| p.get(name)) {
      Some(node) => {
        if let Some(hint) = crate::removed::shape_hint(name, value) {
          errors.push(format!("{origin}: `{path}` {hint}"));
          continue;
        }
        check_value(root, node, value, &path, origin, errors);
      }
      None => errors.push(unknown(root, schema, name, &path, origin)),
    }
  }
}

/// Recurses into a value with its schema, reporting unknown keys of blocks.
fn check_value(
  root: &Json,
  node: &Json,
  value: &Value,
  path: &str,
  origin: &str,
  errors: &mut Vec<String>,
) {
  match value {
    Value::Mapping(map) => {
      let blocks = objects(root, node);
      let props: Vec<&Json> = blocks.iter().filter_map(|b| b.get("properties")).collect();
      if props.is_empty() {
        // A map keyed by name: each value has the same schema.
        if let Some(each) = blocks
          .iter()
          .find_map(|b| b.get("additionalProperties"))
          .filter(|s| s.is_object())
        {
          for (key, inner) in map {
            let key = key.as_str().unwrap_or("?");
            check_value(root, each, inner, &format!("{path}.{key}"), origin, errors);
          }
        }
        return;
      }
      for (key, inner) in map {
        let Some(name) = key.as_str() else { continue };
        let child = format!("{path}.{name}");
        match props.iter().find_map(|p| p.get(name)) {
          Some(schema) => check_value(root, schema, inner, &child, origin, errors),
          None => {
            let parent = blocks
              .iter()
              .find(|b| b.get("properties").is_some())
              .copied()
              .unwrap_or(node);
            errors.push(unknown(root, parent, name, &child, origin));
          }
        }
      }
    }
    Value::Sequence(items) => {
      let item_schema = variants(root, node)
        .into_iter()
        .find_map(|v| v.get("items"))
        .cloned();
      if let Some(item) = item_schema {
        for (i, inner) in items.iter().enumerate() {
          check_value(root, &item, inner, &format!("{path}[{i}]"), origin, errors);
        }
      }
    }
    _ => {}
  }
}

/// The message for a key nothing reads: what replaced it, when an earlier
/// release read it, or the key it was probably meant to be.
fn unknown(root: &Json, parent: &Json, name: &str, path: &str, origin: &str) -> String {
  use crate::removed::KeyScope;
  let (scope, relative) = if let Some(rest) = path.strip_prefix("defaults.") {
    (KeyScope::Service, rest)
  } else if let Some(rest) = path.strip_prefix("services.") {
    (
      KeyScope::Service,
      rest.split_once('.').map(|(_, r)| r).unwrap_or(rest),
    )
  } else {
    (KeyScope::Top, path)
  };
  if let Some(hint) = crate::removed::client_hint(scope, relative) {
    return format!("{origin}: `{path}` {hint}");
  }
  let parent = resolve(root, parent);
  let known: Vec<&str> = parent
    .get("properties")
    .and_then(Json::as_object)
    .map(|o| o.keys().map(String::as_str).collect())
    .unwrap_or_default();
  match crate::authoring::suggest(name, known.iter().copied()) {
    Some(hint) => format!("{origin}: `{path}` is not a setting; did you mean `{hint}`?"),
    None => format!("{origin}: `{path}` is not a setting"),
  }
}

#[cfg(test)]
#[path = "layers_tests.rs"]
mod tests;
