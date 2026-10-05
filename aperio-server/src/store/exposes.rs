//! One durable desired-state transaction for exposes and delegation policies.
//! Sockets are staged by the runtime manager before committing this document.

use aperio_config::expose::{ExposeResource, ExposeSource};
use aperio_config::expose_policy::ExposePolicy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExposeDocument {
  pub version: u32,
  pub revision: u64,
  pub resources: Vec<ExposeResource>,
  pub policies: Vec<ExposePolicy>,
}

impl Default for ExposeDocument {
  fn default() -> Self {
    Self {
      version: 1,
      revision: 1,
      resources: Vec::new(),
      policies: Vec::new(),
    }
  }
}

impl ExposeDocument {
  pub fn validate(&self) -> Result<(), String> {
    if self.version != 1 || self.revision == 0 {
      return Err("unsupported expose document version/revision".into());
    }
    let mut ids = std::collections::HashSet::new();
    for resource in &self.resources {
      if resource.id.is_empty()
        || resource.id.starts_with("file:")
        || resource.revision == 0
        || resource.source != ExposeSource::Api
        || !ids.insert(&resource.id)
      {
        return Err("invalid or repeated expose identity".into());
      }
      let errors = resource.spec.validate();
      if !errors.is_empty() {
        return Err(format!(
          "{}: {}",
          resource.id,
          errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
        ));
      }
    }
    let mut orgs = std::collections::HashSet::new();
    for policy in &self.policies {
      if policy.revision == 0 || !orgs.insert(&policy.org_id) || !policy.validate().is_empty() {
        return Err("invalid or repeated expose policy".into());
      }
    }
    Ok(())
  }
}

pub(crate) struct ExposeStore {
  conn: rusqlite::Connection,
  pub document: ExposeDocument,
  pub load_error: Option<String>,
  pub volatile: bool,
  pub history_error: Option<String>,
}

impl ExposeStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = super::open_db(data_dir);
    Self::from_connection(conn)
  }

  pub fn from_connection(conn: rusqlite::Connection) -> Self {
    use rusqlite::OptionalExtension;
    let volatile = conn.path().is_none_or(|p| p.is_empty() || p == ":memory:");
    let loaded = conn
      .query_row(
        "SELECT data FROM expose_config WHERE id = 'desired'",
        [],
        |row| row.get::<_, String>(0),
      )
      .optional()
      .map_err(|e| e.to_string())
      .and_then(|raw| match raw {
        Some(raw) => serde_json::from_str::<ExposeDocument>(&raw).map_err(|e| e.to_string()),
        None => Ok(ExposeDocument::default()),
      })
      .and_then(|doc| {
        doc.validate()?;
        Ok(doc)
      });
    let (document, load_error) = match loaded {
      Ok(doc) => (doc, None),
      Err(error) => {
        tracing::error!("Expose configuration was not loaded; stored data preserved: {error}");
        (ExposeDocument::default(), Some(error))
      }
    };
    Self {
      conn,
      document,
      load_error,
      volatile,
      history_error: None,
    }
  }

  /// Session payloads contain metadata only. A global cap and per-org cap
  /// bound disk use even when organizations or expose resources are removed.
  pub fn record_session(&mut self, session: &crate::expose_manager::SessionView) {
    let result = (|| -> Result<(), String> {
      let data = serde_json::to_string(session).map_err(|e| e.to_string())?;
      let cutoff = super::tokens::now_secs()
        .saturating_sub(7 * 86400)
        .min(i64::MAX as u64) as i64;
      let transaction = self.conn.transaction().map_err(|e| e.to_string())?;
      transaction.execute("INSERT OR REPLACE INTO expose_sessions(id, org_id, ended_at, data) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![session.id, session.org_id, session.ended_at.unwrap_or(0).min(i64::MAX as u64) as i64, data]).map_err(|e| e.to_string())?;
      transaction
        .execute("DELETE FROM expose_sessions WHERE ended_at < ?1", [cutoff])
        .map_err(|e| e.to_string())?;
      transaction.execute("DELETE FROM expose_sessions WHERE id IN (SELECT id FROM expose_sessions WHERE org_id = ?1 ORDER BY ended_at DESC, id DESC LIMIT -1 OFFSET 256)", [&session.org_id]).map_err(|e| e.to_string())?;
      transaction.execute("DELETE FROM expose_sessions WHERE id IN (SELECT id FROM expose_sessions ORDER BY ended_at DESC, id DESC LIMIT -1 OFFSET 2048)", []).map_err(|e| e.to_string())?;
      transaction.commit().map_err(|e| e.to_string())
    })();
    self.history_error = result.err();
    if let Some(error) = &self.history_error {
      tracing::warn!("Expose session history could not be persisted: {error}");
    }
  }

  pub fn session_history(&self) -> Vec<crate::expose_manager::SessionView> {
    let result = (|| -> Result<Vec<crate::expose_manager::SessionView>, rusqlite::Error> {
      let mut statement = self.conn.prepare("SELECT data FROM expose_sessions WHERE ended_at >= ?1 ORDER BY ended_at DESC, id DESC LIMIT 2048")?;
      let rows = statement.query_map(
        [(super::tokens::now_secs()
          .saturating_sub(7 * 86400)
          .min(i64::MAX as u64) as i64)],
        |row| row.get::<_, String>(0),
      )?;
      Ok(
        rows
          .filter_map(Result::ok)
          .filter_map(|raw| serde_json::from_str(&raw).ok())
          .collect(),
      )
    })();
    result.unwrap_or_default()
  }

  pub fn commit(&mut self, mut next: ExposeDocument) -> Result<(), String> {
    if self.load_error.is_some() {
      return Err(
        "expose store requires recovery; invalid stored configuration was preserved".into(),
      );
    }
    next.revision = self
      .document
      .revision
      .checked_add(1)
      .ok_or("expose revision exhausted")?;
    next.validate()?;
    let json = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    let transaction = self.conn.transaction().map_err(|e| e.to_string())?;
    transaction.execute("INSERT INTO expose_config(id, data) VALUES ('desired', ?1) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
      [json]).map_err(|e| e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())?;
    self.document = next;
    Ok(())
  }
}
