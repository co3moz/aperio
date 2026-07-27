//! The `aperio.yaml` / `aperio-server.yaml` JSON Schemas, served to the
//! dashboard's configuration builder at
//! `GET /aperio/api/config/schema/{kind}`.
//!
//! The same documents `aperio-client` writes into `schemas/` at build time and
//! `aperio-server --print-schema` prints, derived from the shared
//! `aperio-config` types. Serving them rather than bundling a copy into the
//! dashboard is what keeps the builder honest: it describes the settings the
//! *running* server understands, so a field cannot quietly outlive or predate
//! the binary it is configuring.

use axum::{
  Json,
  extract::Path,
  http::StatusCode,
  response::{IntoResponse, Response},
};

/// Returns the requested configuration schema: `client` for `aperio.yaml`,
/// `server` for `aperio-server.yaml`.
#[utoipa::path(get, path = "/aperio/api/config/schema/{kind}", tag = "dashboard",
  description = "JSON Schema of a configuration file, for editors and the dashboard's config builder.",
  params(("kind" = String, Path, description = "client or server")),
  responses(
    (status = 200, description = "JSON Schema document", body = serde_json::Value),
    (status = 404, description = "Unknown schema kind")))]
pub(crate) async fn config_schema_handler(Path(kind): Path<String>) -> Response {
  let schema = match kind.trim().to_ascii_lowercase().as_str() {
    "client" => aperio_config::schema_json(),
    "server" => aperio_config::server_schema_json(),
    _ => return (StatusCode::NOT_FOUND, "Unknown schema kind").into_response(),
  };
  // The generated document is valid JSON by construction; parsing it back is
  // only so axum sends it as `application/json` rather than a quoted string.
  match serde_json::from_str::<serde_json::Value>(&schema) {
    Ok(value) => Json(value).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      format!("schema is not valid JSON: {e}"),
    )
      .into_response(),
  }
}

#[cfg(test)]
#[path = "config_schema_tests.rs"]
mod tests;
