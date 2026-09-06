use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The client's `otel_bridge:` block.
///
/// `PartialEq` so a config reload can notice that this block changed: the
/// bridge is the one facility a running client cannot rebuild, so the change
/// is reported rather than silently ignored.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OtelBridge {
  /// Address for the OTLP/HTTP receiver, the one every SDK can reach with
  /// `OTEL_EXPORTER_OTLP_ENDPOINT`. Default: `127.0.0.1:4318`.
  #[schemars(extend("examples" = ["127.0.0.1:4318"]))]
  pub listen: Option<String>,
  /// Address for the OTLP/gRPC receiver, for SDKs pinned to that transport.
  /// Unset = no gRPC listener. Conventionally port 4317.
  #[schemars(extend("examples" = ["127.0.0.1:4317"]))]
  pub listen_grpc: Option<String>,
  /// How exports reach the server: `tunnel` sends them as frames on the
  /// WebSocket this client already holds, `https` posts them to the server's
  /// endpoint as ordinary requests. Default: `tunnel`.
  ///
  /// `tunnel` is the one that keeps the "exactly one outbound connection"
  /// property that makes this worth having; `https` exists because a client
  /// whose telemetry is bursty may prefer it to stay off the tunnel entirely,
  /// where it would share flow control with proxied traffic.
  #[schemars(extend("examples" = ["tunnel", "https"]))]
  pub transport: Option<String>,
  /// Exports to hold when the far end is not keeping up. Past this, the
  /// newest is dropped and counted, never waited on: an exporter that cannot
  /// hand off its batch blocks the application it is instrumenting, so
  /// telemetry must never be the reason a tunnel stalls. Default: `256`.
  #[schemars(extend("examples" = [256]))]
  pub queue: Option<usize>,
}

/// How a visitor gate is written, on either side of the tunnel.
///
/// Three spellings of one thing. The scalar `auth: "user:password"` predates
/// the grammar and keeps working, folding to a single `basic` method; one
/// block names a method with its settings; a list is admitted when **any** of
/// its methods admits the visitor, which is what "a browser signs in, a script
/// presents a key" needs and what a single choice cannot say.
///
/// The variants are ordered for `untagged`: a YAML scalar can only be the
/// first, a mapping only the second, a sequence only the third, so a value is
/// never read as the wrong shape.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum AuthSetting {
  /// `auth: "user:password"`, the spelling that predates the method grammar.
  Credentials(String),
  /// One method: `auth: {method: none}`. Boxed because a method entry grew
  /// fields as methods were added, and an unboxed variant makes every
  /// `AuthSetting`, including the one-word scalar, as large as the largest
  /// method's settings.
  One(Box<AuthMethodSpec>),
  /// Several methods, any one of which admits the visitor.
  Any(Vec<AuthMethodSpec>),
}

/// One entry of an `auth:` policy: which method gates the route, and the
/// settings that method needs.
///
/// `method` is deliberately a string here rather than an enum: an unknown one
/// should be refused by name, with the available methods listed, and a serde
/// "unknown variant" error inside an untagged enum says only that nothing
/// matched. The same reason `alert_rules` parses its `metric` by hand.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthMethodSpec {
  /// The gate: `none` (deliberately open), `basic` (a `user:password`
  /// login), `bearer`, `jwt`, `forward`, or `aperio` (this server's own
  /// sign-in: a dashboard session reaching the hostname's organization, with
  /// no credential to invent).
  #[schemars(extend("examples" = ["basic", "none", "aperio"]))]
  pub method: String,
  /// `basic`: the credentials that open this gate, each `user:password`.
  /// A single value or a list.
  #[serde(default)]
  #[schemars(extend("examples" = ["admin:s3cret", ["admin:s3cret", "ops:hunter2"]]))]
  pub users: Option<Credentials>,
  /// `bearer`: the secret a caller presents as `Authorization: Bearer <secret>`.
  /// One or a list, all alternatives on the same gate, so a key can be
  /// rotated by adding the new one before withdrawing the old.
  #[serde(default)]
  #[schemars(extend("examples" = ["${API_SECRET}", ["${API_SECRET}", "${API_SECRET_PREVIOUS}"]]))]
  pub secret: Option<Credentials>,
  /// `bearer`: also accept the secret as `?aperio_token=`, for the callers
  /// that cannot set a header. **Off by default**: a query string reaches the
  /// access log, the `Referer` header, browser history and every proxy in
  /// front, so this is a trade an operator makes on purpose.
  #[serde(default)]
  #[schemars(extend("examples" = [true]))]
  pub query: Option<bool>,
  /// `jwt`: URL of the issuer's JWKS, the public keys its tokens are signed
  /// with. Fetched and cached, and re-fetched when a token names a key id
  /// that is not in the cache. Subject to the server's outbound policy.
  #[serde(default)]
  #[schemars(extend("examples" = ["https://accounts.example.com/.well-known/jwks.json"]))]
  pub jwks_url: Option<String>,
  /// `jwt`: shared secret for HMAC-signed tokens (`HS256`), for an issuer
  /// that is your own service rather than a provider with public keys.
  /// Mutually exclusive with `jwks_url`.
  #[serde(default)]
  #[schemars(extend("examples" = ["${JWT_SECRET}"]))]
  pub hmac_secret: Option<String>,
  /// `jwt`: required `iss` claim. Strongly recommended: without it any issuer
  /// whose key the JWKS happens to carry is accepted.
  #[serde(default)]
  #[schemars(extend("examples" = ["https://accounts.example.com"]))]
  pub issuer: Option<String>,
  /// `jwt`: required `aud` claim, one or a list of accepted values.
  #[serde(default)]
  #[schemars(extend("examples" = ["aperio", ["aperio", "internal-tools"]]))]
  pub audience: Option<Credentials>,
  /// `jwt`: further claims a token must carry, each as an exact value the
  /// claim must equal (`{groups: engineering}`).
  #[serde(default)]
  #[schemars(extend("examples" = [{"groups": "engineering"}]))]
  pub claims: Option<std::collections::BTreeMap<String, String>>,
  /// `jwt`: read the token from this cookie instead of the `Authorization`
  /// header, which is where an identity-aware proxy in front usually puts it
  /// (Cloudflare Access writes `CF_Authorization`).
  #[serde(default)]
  #[schemars(extend("examples" = ["CF_Authorization"]))]
  pub cookie: Option<String>,
  /// `forward`: the endpoint asked about each request. `2xx` admits it;
  /// anything else refuses it, and the endpoint's own answer is what the
  /// visitor gets, so it can redirect to a login of its own.
  #[serde(default)]
  #[schemars(extend("examples" = ["http://127.0.0.1:7070/_authcheck"]))]
  pub url: Option<String>,
  /// `forward`: request headers copied to the subrequest.
  /// Default: `cookie` and `authorization`, the two that carry an identity.
  #[serde(default)]
  #[schemars(extend("examples" = [["cookie", "authorization", "x-api-key"]]))]
  pub request_headers: Option<Vec<String>>,
  /// `forward`: headers of a `2xx` answer copied onto the request that goes
  /// to the backend. This is how the pattern delivers an identity, and an
  /// open list is how it becomes a header injection, so it is named
  /// explicitly and empty by default.
  #[serde(default)]
  #[schemars(extend("examples" = [["x-auth-user", "x-auth-groups"]]))]
  pub response_headers: Option<Vec<String>>,
  /// `forward`: seconds to wait for the endpoint. A timeout **refuses** the
  /// request: an auth gate that opens when its check is unreachable is not a
  /// gate. Default: `5`.
  #[serde(default)]
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// `forward`: seconds to remember a verdict for an identical credential,
  /// so a busy route does not pay a round trip per request. `0` asks every
  /// time, and is the default when the endpoint is the server's own; over the
  /// tunnel (`via: client`) the default is `30`, since the cold path there is
  /// two round trips before the visitor's own request starts.
  #[serde(default)]
  #[schemars(extend("examples" = [30]))]
  pub cache: Option<u64>,
  /// `forward`: whose network the endpoint is dialed from. `server` (the
  /// default) is the server's; `client` asks over the tunnel, and the client
  /// serving the route calls its own internal endpoint, so `localhost:7070`
  /// means the client's localhost. A policy written on a client must say
  /// `client`, so the same word cannot mean two networks in two files.
  #[serde(default)]
  #[schemars(extend("examples" = ["client"]))]
  pub via: Option<String>,
}

/// Where a `forward` method's endpoint is dialed from.
pub const FORWARD_VIA: &[&str] = &["server", "client"];

/// True when this entry is a `forward` asked over the tunnel.
pub fn forward_via_client(spec: &AuthMethodSpec) -> bool {
  spec.method.trim().eq_ignore_ascii_case("forward")
    && spec
      .via
      .as_deref()
      .is_some_and(|v| v.trim().eq_ignore_ascii_case("client"))
}

/// Validates a policy a *client* declares for one of its services: the
/// general rules, and one more, that a `forward` here says `via: client`.
/// The server never dials a URL a client chose, so a client-side `forward`
/// without it would be a gate the server cannot ask and would refuse
/// everyone; better refused where it is written.
pub fn validate_client_declared_auth(setting: &AuthSetting) -> Result<(), String> {
  validate_auth_setting(setting)?;
  for (i, spec) in setting.methods().iter().enumerate() {
    if spec.method.trim().eq_ignore_ascii_case("forward") && !forward_via_client(spec) {
      return Err(format!(
        "`auth:` entry #{}: a `forward` declared on a client has to say `via: client`, since its \
         endpoint is on this client's network and the server never dials a URL a client chose",
        i + 1
      ));
    }
  }
  Ok(())
}

/// A `basic` method's credentials: one `user:password` or a list of them.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(untagged)]
pub enum Credentials {
  /// A single `user:password`.
  One(String),
  /// Several, any of which opens the gate.
  Many(Vec<String>),
}

impl Credentials {
  /// The credentials as a list, whichever way they were written.
  pub fn as_slice(&self) -> &[String] {
    match self {
      Credentials::One(one) => std::slice::from_ref(one),
      Credentials::Many(many) => many,
    }
  }
}

impl AuthSetting {
  /// The policy as a list of method entries, whichever way it was written.
  ///
  /// The scalar spelling becomes exactly what it has always meant: one
  /// `basic` method carrying that one `user:password`.
  pub fn methods(&self) -> Vec<AuthMethodSpec> {
    match self {
      AuthSetting::Credentials(creds) => vec![AuthMethodSpec {
        method: "basic".to_string(),
        users: Some(Credentials::One(creds.clone())),
        ..Default::default()
      }],
      AuthSetting::One(spec) => vec![spec.as_ref().clone()],
      AuthSetting::Any(specs) => specs.clone(),
    }
  }

  /// The single `user:password` this policy is equivalent to, when it is
  /// equivalent to one.
  ///
  /// This is what travels to a server that predates the grammar, and what the
  /// existing scalar surfaces (`APERIO_SERVER_AUTH`, the dashboard's editable
  /// value) still read. `None` means the policy says something the scalar
  /// cannot: no credential at all, several of them, or another method.
  pub fn as_single_credential(&self) -> Option<&str> {
    match self {
      AuthSetting::Credentials(creds) => Some(creds.as_str()),
      AuthSetting::One(spec) => spec.single_credential(),
      AuthSetting::Any(specs) => match specs.as_slice() {
        [only] => only.single_credential(),
        _ => None,
      },
    }
  }
}

impl AuthMethodSpec {
  /// The one `user:password` this entry is equivalent to, if it is a `basic`
  /// method carrying exactly one.
  fn single_credential(&self) -> Option<&str> {
    if !self.method.trim().eq_ignore_ascii_case("basic") {
      return None;
    }
    match self.users.as_ref()?.as_slice() {
      [only] => Some(only.as_str()),
      _ => None,
    }
  }
}

/// The methods a visitor gate may name today, in the order they are listed
/// back to an operator who names one that does not exist.
///
/// Deliberately a closed set: the open version was considered and withdrawn
/// (`planned_features.md` #103). Further methods each arrive as their own
/// entry rather than as a plugin interface.
pub const AUTH_METHODS: &[&str] = &["none", "basic", "bearer", "jwt", "forward", "aperio"];

/// Shortest `bearer` secret accepted.
///
/// A bearer secret is one opaque string compared verbatim: unlike a
/// `user:password` there is no second half, no login form, and no lockout in
/// front of it, so its length is the whole of its strength. Operators reach
/// for short strings while testing and leave them in, which is the failure
/// this refuses at the point it is written.
pub const MIN_BEARER_SECRET_LEN: usize = 16;

/// Is this a `basic` credential the login path could ever match?
///
/// A value without the separator, or with an empty half, is refused where it
/// is written rather than at the moment a visitor fails to get in: a gate
/// nobody can open looks exactly like a gate that is broken.
fn credential_is_usable(raw: &str) -> bool {
  match raw.split_once(':') {
    Some((user, password)) => !user.is_empty() && !password.is_empty(),
    None => false,
  }
}

/// Validates a visitor-auth policy, naming what is wrong and where.
///
/// Called from both sides: the client refuses to start on its own `auth:`,
/// and the server refuses to start on the file's. A policy that parses but
/// cannot admit anybody is the failure worth catching here, since it presents
/// as "the password does not work" hours later.
pub fn validate_auth_setting(setting: &AuthSetting) -> Result<(), String> {
  let methods = setting.methods();
  if methods.is_empty() {
    return Err("`auth:` is an empty list; remove it, or write `{method: none}` to say the route is deliberately open".to_string());
  }
  if methods.len() > 1
    && methods
      .iter()
      .any(|m| m.method.trim().eq_ignore_ascii_case("none"))
  {
    return Err(
      "`method: none` admits everyone, so listing it beside another method makes that method unreachable; keep one or the other"
        .to_string(),
    );
  }
  for (i, spec) in methods.iter().enumerate() {
    let at = |what: String| format!("`auth:` entry #{}: {}", i + 1, what);
    let name = spec.method.trim().to_ascii_lowercase();
    if !AUTH_METHODS.contains(&name.as_str()) {
      return Err(at(format!(
        "`{}` is not a method ({})",
        spec.method,
        AUTH_METHODS.join(", ")
      )));
    }
    if let Some(via) = spec.via.as_deref() {
      if name != "forward" {
        return Err(at(format!(
          "`via:` belongs to `method: forward`, not `{}`",
          spec.method
        )));
      }
      if !FORWARD_VIA.contains(&via.trim().to_ascii_lowercase().as_str()) {
        return Err(at(format!(
          "`via: {via}` is not a place to ask from ({})",
          FORWARD_VIA.join(", ")
        )));
      }
    }
    match name.as_str() {
      "none" => {
        if spec.users.is_some()
          || spec.secret.is_some()
          || spec.jwks_url.is_some()
          || spec.hmac_secret.is_some()
          || spec.url.is_some()
        {
          return Err(at(
            "`method: none` is the open gate and takes no credentials".to_string(),
          ));
        }
      }
      "aperio" => {
        if spec.users.is_some()
          || spec.secret.is_some()
          || spec.jwks_url.is_some()
          || spec.hmac_secret.is_some()
          || spec.url.is_some()
        {
          return Err(at(
            "`method: aperio` is this server's own sign-in and takes no credentials: the visitor signs in at /aperio/auth"
              .to_string(),
          ));
        }
      }
      "bearer" => {
        if spec.users.is_some() {
          return Err(at(
            "`method: bearer` takes `secret:`, not `users:`; it has no user half".to_string(),
          ));
        }
        let Some(secrets) = spec.secret.as_ref() else {
          return Err(at(
            "`method: bearer` needs `secret:`, the value a caller presents as `Authorization: Bearer`"
              .to_string(),
          ));
        };
        if secrets.as_slice().is_empty() {
          return Err(at("`secret:` is empty".to_string()));
        }
        for secret in secrets.as_slice() {
          if secret.trim().is_empty() {
            return Err(at(
              "a blank `secret:` would be a gate that opens for an empty header".to_string(),
            ));
          }
          if secret.len() < MIN_BEARER_SECRET_LEN {
            return Err(at(format!(
              "this `secret:` is {} characters; a bearer secret is compared verbatim and has no user half to slow a guess down, so it carries the whole of the gate and needs at least {MIN_BEARER_SECRET_LEN}",
              secret.len()
            )));
          }
        }
      }
      "forward" => {
        if spec.users.is_some() || spec.secret.is_some() {
          return Err(at(
            "`method: forward` asks an endpoint; it takes `url:`, not `users:` / `secret:`"
              .to_string(),
          ));
        }
        let Some(url) = spec.url.as_deref() else {
          return Err(at(
            "`method: forward` needs `url:`, the endpoint asked about each request".to_string(),
          ));
        };
        if !url.starts_with("https://") && !url.starts_with("http://") {
          return Err(at(format!("`url:` is not a URL: `{url}`")));
        }
        for (label, list) in [
          ("request_headers", spec.request_headers.as_ref()),
          ("response_headers", spec.response_headers.as_ref()),
        ] {
          for name in list.into_iter().flatten() {
            if name.trim().is_empty() {
              return Err(at(format!("`{label}:` has a blank entry")));
            }
            if name.contains('*') {
              return Err(at(format!(
                "`{label}:` takes header names, not patterns; `{name}` would be a rule nobody can read back"
              )));
            }
          }
        }
      }
      "jwt" => {
        if spec.users.is_some() || spec.secret.is_some() {
          return Err(at(
            "`method: jwt` verifies a signature; it takes `jwks_url:` or `hmac_secret:`, not `users:` / `secret:`".to_string(),
          ));
        }
        match (spec.jwks_url.as_deref(), spec.hmac_secret.as_deref()) {
          (None, None) => {
            return Err(at(
              "`method: jwt` needs `jwks_url:` (the issuer's public keys) or `hmac_secret:` (a shared secret)".to_string(),
            ));
          }
          (Some(_), Some(_)) => {
            return Err(at(
              "`method: jwt` takes `jwks_url:` or `hmac_secret:`, not both: they are two different ways of knowing who signed a token".to_string(),
            ));
          }
          (Some(url), None) => {
            if !url.starts_with("https://") && !url.starts_with("http://") {
              return Err(at(format!("`jwks_url:` is not a URL: `{url}`")));
            }
          }
          (None, Some(secret)) => {
            if secret.len() < MIN_BEARER_SECRET_LEN {
              return Err(at(format!(
                "this `hmac_secret:` is {} characters; it is the whole of what proves a token was not written by its bearer, so it needs at least {MIN_BEARER_SECRET_LEN}",
                secret.len()
              )));
            }
          }
        }
        if spec.issuer.as_deref().is_some_and(|i| i.trim().is_empty()) {
          return Err(at("`issuer:` is blank".to_string()));
        }
        if let Some(claims) = spec.claims.as_ref() {
          for key in claims.keys() {
            if key.trim().is_empty() {
              return Err(at("a claim requirement has a blank name".to_string()));
            }
          }
        }
      }
      "basic" => {
        if spec.secret.is_some() {
          return Err(at(
            "`method: basic` takes `users:`, not `secret:`; `secret:` belongs to `bearer`"
              .to_string(),
          ));
        }
        let Some(users) = spec.users.as_ref() else {
          return Err(at(
            "`method: basic` needs `users:`, one or more `user:password`".to_string(),
          ));
        };
        if users.as_slice().is_empty() {
          return Err(at("`users:` is empty".to_string()));
        }
        for cred in users.as_slice() {
          if !credential_is_usable(cred) {
            return Err(at(format!(
              "`{cred}` is not `user:password`; a value without both halves can never be logged in with"
            )));
          }
        }
      }
      _ => unreachable!("method checked against AUTH_METHODS above"),
    }
  }
  Ok(())
}
