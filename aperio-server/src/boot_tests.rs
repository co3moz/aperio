//! What this pins down: where a start's settings come from (the database,
//! else the first-start document, else the defaults), that the document is
//! read once and never again, and that every first-start document the docs
//! hand out is one this server accepts.

use super::*;

fn temp_dir() -> String {
  let dir = crate::test_support::test_temp_root().join(format!("boot-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().into_owned()
}

fn boot(data_dir: &str) -> Boot {
  Boot {
    token: "0123456789abcdef0123456789abcdef".to_string(),
    data_dir: data_dir.to_string(),
  }
}

/// Runs `f` with `APERIO_INIT_FILE` naming `path`, or unset.
fn with_init_file<T>(path: Option<&str>, f: impl FnOnce() -> T) -> T {
  let _lock = crate::test_support::config_lock();
  // SAFETY: every test touching the environment holds the config lock.
  unsafe {
    match path {
      Some(p) => std::env::set_var("APERIO_INIT_FILE", p),
      None => std::env::remove_var("APERIO_INIT_FILE"),
    }
  }
  let out = f();
  unsafe { std::env::remove_var("APERIO_INIT_FILE") };
  out
}

#[test]
fn an_empty_database_without_a_document_starts_on_the_defaults() {
  let dir = temp_dir();
  let (settings, init) = with_init_file(None, || load_settings(&boot(&dir))).unwrap();
  assert_eq!(settings, ServerSettings::default());
  assert!(init.is_none());
}

#[test]
fn the_first_start_document_is_read_once_and_then_the_database_wins() {
  let dir = temp_dir();
  let doc = std::path::Path::new(&dir).join("server-init.yaml");
  std::fs::write(&doc, "settings:\n  limits:\n    tunnels: 7\ntokens: []\n").unwrap();
  let doc = doc.to_string_lossy().into_owned();

  let (first, init) = with_init_file(Some(&doc), || load_settings(&boot(&dir))).unwrap();
  assert_eq!(first.limits.tunnels, 7);
  let init = init.expect("the rest of the document goes to the caller");
  assert!(init.tokens.is_some() && init.settings.is_none());

  // Edited afterwards: the database already has settings, so this changes
  // nothing, which is the whole contract of a first-start document.
  std::fs::write(&doc, "settings:\n  limits:\n    tunnels: 99\n").unwrap();
  let (second, init) = with_init_file(Some(&doc), || load_settings(&boot(&dir))).unwrap();
  assert_eq!(second.limits.tunnels, 7);
  assert!(init.is_none());
}

#[test]
fn a_document_the_server_cannot_honour_refuses_the_start() {
  let dir = temp_dir();
  let doc = std::path::Path::new(&dir).join("server-init.yaml");
  std::fs::write(&doc, "settings:\n  proxy:\n    trusted: [not-an-address]\n").unwrap();
  let doc = doc.to_string_lossy().into_owned();
  let Err(err) = with_init_file(Some(&doc), || load_settings(&boot(&dir))) else {
    panic!("a document the server cannot honour must refuse the start");
  };
  assert!(err.contains("proxy.trusted"), "{err}");
  // Nothing was stored, so fixing the file and starting again works.
  assert_eq!(
    crate::store::settings::SettingsStore::load(&dir)
      .get()
      .unwrap(),
    None
  );
}

/// The examples are the copy-and-adapt surface, and a first-start document
/// that does not start a server is a broken deployment handed out as
/// documentation. Read here the way the server reads one, which is the side a
/// change to the document's shape breaks.
#[test]
fn every_docs_example_document_starts_a_server() {
  let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/examples");
  let mut seen = 0;
  for folder in std::fs::read_dir(&root)
    .expect("docs/examples exists")
    .flatten()
  {
    let path = folder.path().join("server-init.yaml");
    if !path.exists() {
      continue;
    }
    seen += 1;
    let shown = path.display().to_string();
    let document = read_document(&shown).unwrap_or_else(|e| panic!("{e}"));
    let settings = document.settings.unwrap_or_default();
    crate::server::state_build::config::build_config(&settings, "tok", &temp_dir())
      .unwrap_or_else(|e| panic!("{shown}: {e}"));
  }
  assert!(seen >= 18, "only {seen} first-start documents found");
}

/// `aperio-config` lists the sections a document may carry so its own checks
/// can read one; the import is what defines them. Held together from here,
/// since a section added to the import is a change on this side.
#[test]
fn the_document_sections_aperio_config_knows_are_the_ones_the_import_reads() {
  let err = serde_json::from_str::<crate::api::export::ImportDump>(r#"{"no_such_section":1}"#)
    .err()
    .expect("an unknown section is refused")
    .to_string();
  let expected = err
    .split_once("expected one of ")
    .map(|(_, rest)| rest)
    .unwrap_or_default();
  let mut import: Vec<&str> = expected
    .split('`')
    .skip(1)
    .step_by(2)
    // What an export says about itself, and the settings, which are the
    // document's subject rather than one of its other sections.
    .filter(|f| !["settings", "exported_at", "server_version", "sections"].contains(f))
    .collect();
  import.sort_unstable();
  let mut known = aperio_config::server_settings::DOCUMENT_SECTIONS.to_vec();
  known.sort_unstable();
  assert_eq!(import, known);
}

/// The first-start document is read with the client's loader: a secret can
/// stay out of the file as `${VAR}`, and the document can be split with
/// `include:`, each resolved relative to the file that names it.
#[test]
fn a_first_start_document_takes_variables_and_includes() {
  let dir = temp_dir();
  let root = std::path::Path::new(&dir);
  std::fs::write(
    root.join("server-init.yaml"),
    "include: [oidc.yaml]\nsettings:\n  cache: {enabled: true}\n",
  )
  .unwrap();
  std::fs::write(
    root.join("oidc.yaml"),
    "settings:\n  dashboard: {fenced_login: true}\n  metrics: {token: '${APERIO_BOOT_TEST_UNSET:-scrape-token}'}\n",
  )
  .unwrap();
  let document = read_document(&root.join("server-init.yaml").display().to_string())
    .unwrap_or_else(|e| panic!("{e}"));
  let settings = document.settings.expect("the merged settings");
  assert!(settings.cache.enabled, "the including file's own setting");
  assert!(settings.dashboard.fenced_login, "the include's setting");
  assert_eq!(settings.metrics.token.as_deref(), Some("scrape-token"));

  // A typo is still refused, whichever file wrote it.
  std::fs::write(
    root.join("oidc.yaml"),
    "settings: {cahce: {enabled: true}}\n",
  )
  .unwrap();
  assert!(read_document(&root.join("server-init.yaml").display().to_string()).is_err());
}
