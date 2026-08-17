//! That the schema endpoint serves both documents and nothing else, so an editor
//! pointed at it cannot be handed something that is not a schema.

use super::*;
use axum::body::to_bytes;

async fn body_json(resp: Response) -> serde_json::Value {
  let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
  serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn serves_both_schemas_and_rejects_anything_else() {
  // The client schema describes aperio.yaml…
  let resp = config_schema_handler(Path("client".to_string())).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let doc = body_json(resp).await;
  assert!(doc["properties"]["target"].is_object(), "{doc}");
  assert!(doc["properties"]["services"].is_object());

  // …and the server schema aperio-server.yaml. The two must not be the same
  // document, which a copy-paste slip in the match arms would produce.
  let resp = config_schema_handler(Path("SERVER".to_string())).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let server = body_json(resp).await;
  assert!(
    server["properties"]["max_body_size"].is_object(),
    "{server}"
  );
  assert!(server["properties"]["target"].is_null());

  // Unknown kinds 404 rather than falling back to one of them.
  let resp = config_schema_handler(Path("nonsense".to_string())).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
