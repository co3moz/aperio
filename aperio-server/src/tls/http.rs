//! The ACME client's HTTP, carried by the server's own reqwest.
//!
//! instant-acme brings a hyper and rustls stack of its own unless it is
//! handed a client. Handing it this one keeps a single TLS stack in the
//! binary, and puts the authority's traffic on the same egress as every
//! other call the server makes: the configured `outbound.proxy`, the host's
//! certificate store, and no redirects followed.

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use instant_acme::{BodyWrapper, BytesResponse, Error, HttpClient};

pub(crate) struct ReqwestAcme(pub(crate) reqwest::Client);

impl ReqwestAcme {
  /// A client built the way every outbound one is.
  pub(crate) fn outbound() -> Result<Self, String> {
    crate::outbound::client_builder()
      .timeout(std::time::Duration::from_secs(30))
      .build()
      .map(ReqwestAcme)
      .map_err(|e| format!("could not build the ACME client: {e}"))
  }
}

fn other(e: impl std::error::Error + Send + Sync + 'static) -> Error {
  Error::Other(Box::new(e))
}

impl HttpClient for ReqwestAcme {
  fn request(
    &self,
    req: axum::http::Request<BodyWrapper<Bytes>>,
  ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, Error>> + Send>> {
    let client = self.0.clone();
    Box::pin(async move {
      let (parts, body) = req.into_parts();
      let body = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(never) => match never {},
      };
      let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).map_err(other)?;
      let mut outgoing = client.request(method, parts.uri.to_string()).body(body);
      for (name, value) in &parts.headers {
        outgoing = outgoing.header(name.as_str(), value.as_bytes());
      }
      let response = outgoing.send().await.map_err(other)?;
      let mut out = axum::http::Response::builder().status(response.status().as_u16());
      for (name, value) in response.headers() {
        out = out.header(name.as_str(), value.as_bytes());
      }
      let bytes = response.bytes().await.map_err(other)?;
      let response = out.body(Full::new(bytes)).map_err(other)?;
      Ok(BytesResponse::from(response))
    })
  }
}
