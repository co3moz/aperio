//! `aperio-client login`, `logout` and `profile`: signing in to a server's
//! admin API once, the way `gh auth login` or a kubeconfig context does, so
//! that `api` and `admin` calls carry a key without one being typed or
//! written into a project file.
//!
//! Signing in with a password mints an admin key for this machine and keeps
//! it; the session the password opened is only the way to that key, and is
//! closed again. Only the server admin mints keys, so anyone else is given
//! one and keeps it with `login --key -`.

use super::profiles::{Profile, Profiles, default_name, profiles_path};
use super::*;

/// `aperio-client login`.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct LoginArgs {
  /// The name to keep this server under (default: its host)
  #[arg(long, value_name = "NAME")]
  pub(crate) profile: Option<String>,
  /// Sign in as this dashboard user (default `aperio`, the built-in
  /// account, whose password is the master token)
  #[arg(long, value_name = "USER", default_value = "aperio")]
  pub(crate) user: String,
  /// The password, `-` to read it from stdin. For `aperio` it defaults to the
  /// tunnel token configured here (--server-token, APERIO_SERVER_TOKEN)
  #[arg(long, value_name = "PASSWORD")]
  pub(crate) password: Option<String>,
  /// The current code, for a user with two-factor authentication
  #[arg(long, value_name = "CODE")]
  pub(crate) totp: Option<String>,
  /// Keep an admin key someone already minted instead of signing in, `-` to
  /// read it from stdin
  #[arg(long, value_name = "KEY", conflicts_with_all = ["password", "totp"])]
  pub(crate) key: Option<String>,
  /// Role of the key minted for this machine: viewer, operator or admin
  #[arg(long, value_name = "ROLE", default_value = "admin")]
  pub(crate) role: String,
  /// Where the minted key acts: `*` (every organization), `master`, or a
  /// child organization's id
  #[arg(long, value_name = "ORG", default_value = "*")]
  pub(crate) org: String,
  /// Lifetime of the minted key: 30d, 1w, or never (default)
  #[arg(long, value_name = "DURATION")]
  pub(crate) expire: Option<String>,
}

/// `aperio-client profile ...`.
#[derive(clap::Subcommand, Clone, Debug)]
pub(crate) enum ProfileCmd {
  /// The servers signed in to, the one in use marked with `*`
  List,
  /// Use another profile for `api` and `admin` calls
  Use {
    /// The profile's name, as `profile list` prints it
    name: String,
  },
}

const NO_SERVER: &str =
  "the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)";

fn http_client() -> Result<reqwest::Client, String> {
  crate::ensure_crypto_provider();
  reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|e| format!("failed to build the HTTP client: {e}"))
}

/// Runs `login`, printing what happened; the process exit code.
pub(crate) async fn run_login(settings: &ClientSettings, args: &LoginArgs) -> i32 {
  match login(settings, args).await {
    Ok(done) => {
      eprintln!("{done}");
      0
    }
    Err(e) => {
      eprintln!("error: {e}");
      1
    }
  }
}

async fn login(settings: &ClientSettings, args: &LoginArgs) -> Result<String, String> {
  let server = settings.server.clone().ok_or(NO_SERVER)?;
  let path = profiles_path().ok_or("there is no home directory to keep the profile in")?;
  let mut profiles = Profiles::read(&path)?;
  let name = args
    .profile
    .clone()
    .unwrap_or_else(|| default_name(&server));
  let http = http_client()?;
  let profile = match &args.key {
    Some(raw) => {
      let key = read_maybe_stdin(raw)?.trim().to_string();
      // Checked before it is kept: a key that does not authenticate is a
      // typo, and keeping it would only move the error to the next call.
      send(&http, &server, Some(&key), Call::get("/aperio/api/session"))
        .await
        .map_err(|e| format!("the key was not accepted: {e}"))?;
      Profile {
        server: server.clone(),
        api_key: key,
        key_id: None,
      }
    }
    None => {
      let password = match &args.password {
        Some(raw) => read_maybe_stdin(raw)?,
        None if args.user == "aperio" => settings.token.clone().ok_or(
          "give the master token with --password - (read from stdin), or configure it as the tunnel token (--server-token, APERIO_SERVER_TOKEN)",
        )?,
        None => return Err("give the password with --password - (read from stdin)".to_string()),
      };
      let cookie = sign_in(&http, &server, &args.user, &password, args.totp.as_deref()).await?;
      let minted = mint(&http, &server, &cookie, &name, args).await;
      // The session was only the way to the key; a failure to close it
      // leaves one that expires on its own.
      if let Ok(req) = request(&http, &server, reqwest::Method::POST, "/aperio/auth/logout") {
        let _ = req.header(reqwest::header::COOKIE, &cookie).send().await;
      }
      let (key_id, key) = minted?;
      Profile {
        server: server.clone(),
        api_key: key,
        key_id: Some(key_id),
      }
    }
  };
  // Signing in again replaces this machine's key; the old one is revoked
  // with the new, when the new may, rather than left behind on the server.
  let replaced = profiles.profiles.get(&name).cloned();
  let fresh_key = profile.api_key.clone();
  profiles.insert(&name, profile);
  profiles.write(&path)?;
  let mut done = format!(
    "Signed in to {server}; profile `{name}` is in use. Its key is kept in {}.",
    path.display()
  );
  if let Some(old) = replaced.and_then(|p| p.key_id.filter(|_| p.api_key != fresh_key)) {
    let revoked = send(
      &http,
      &server,
      Some(&fresh_key),
      Call::delete(format!("/aperio/api/admin-keys/{old}")),
    )
    .await;
    if revoked.is_err() {
      done.push_str(&format!(
        " The key it replaces ({old}) is still in force on the server; revoke it from the dashboard or with `aperio-server admin revoke-key {old}`."
      ));
    }
  }
  Ok(done)
}

/// A request to `path` on `server`, before its credential.
fn request(
  http: &reqwest::Client,
  server: &str,
  method: reqwest::Method,
  path: &str,
) -> Result<reqwest::RequestBuilder, String> {
  Ok(http.request(method, build_http_url(server, path)?))
}

/// Signs in to the dashboard and returns the cookies it set, ready to send
/// back as one `Cookie` header.
async fn sign_in(
  http: &reqwest::Client,
  server: &str,
  user: &str,
  password: &str,
  totp: Option<&str>,
) -> Result<String, String> {
  let mut req =
    request(http, server, reqwest::Method::POST, "/aperio/auth")?.basic_auth(user, Some(password));
  if let Some(code) = totp {
    req = req.header("x-aperio-totp", code.trim());
  }
  let response = req
    .send()
    .await
    .map_err(|e| format!("could not reach {server}: {e}"))?;
  let status = response.status();
  if status == reqwest::StatusCode::UNAUTHORIZED
    && response
      .headers()
      .get("x-aperio-totp")
      .is_some_and(|v| v == "required")
  {
    return Err(format!(
      "{user} has two-factor authentication: add --totp <code>"
    ));
  }
  let cookie = session_cookies(response.headers());
  if status.is_client_error() || status.is_server_error() || cookie.is_empty() {
    return Err(format!("signing in as {user} was refused ({status})"));
  }
  Ok(cookie)
}

/// The `name=value` pairs of every `Set-Cookie`, joined for a `Cookie`
/// header.
pub(crate) fn session_cookies(headers: &reqwest::header::HeaderMap) -> String {
  headers
    .get_all(reqwest::header::SET_COOKIE)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .filter_map(|v| v.split(';').next())
    .map(str::trim)
    .filter(|pair| pair.contains('=') && !pair.ends_with('='))
    .collect::<Vec<_>>()
    .join("; ")
}

/// Mints this machine's admin key with the session `cookie` carries.
async fn mint(
  http: &reqwest::Client,
  server: &str,
  cookie: &str,
  profile: &str,
  args: &LoginArgs,
) -> Result<(String, String), String> {
  let mut body = Map::new();
  let label: String = format!("login {profile}").chars().take(64).collect();
  body.insert("name".into(), Value::String(label));
  body.insert("role".into(), Value::String(args.role.clone()));
  if args.org.trim() != "master" {
    body.insert("org_id".into(), Value::String(args.org.trim().to_string()));
  }
  put_opt(&mut body, "ttl_seconds", ttl_field(&args.expire, true)?);
  let response = request(
    http,
    server,
    reqwest::Method::POST,
    "/aperio/api/admin-keys",
  )?
  .header(reqwest::header::COOKIE, cookie)
  .json(&Value::Object(body))
  .send()
  .await
  .map_err(|e| format!("could not reach {server}: {e}"))?;
  let status = response.status();
  let text = response.text().await.unwrap_or_default();
  if status == reqwest::StatusCode::FORBIDDEN {
    return Err(
      "only the server admin mints admin keys; ask them for one and keep it with `aperio-client login --key -`"
        .to_string(),
    );
  }
  if !status.is_success() {
    return Err(format!(
      "minting a key was refused ({status}): {}",
      text.trim()
    ));
  }
  let answer: Value =
    serde_json::from_str(&text).map_err(|e| format!("an unexpected answer: {e}"))?;
  match (answer["id"].as_str(), answer["key"].as_str()) {
    (Some(id), Some(key)) => Ok((id.to_string(), key.to_string())),
    _ => Err("an unexpected answer: no key in it".to_string()),
  }
}

/// Runs `logout`: revokes the profile's key when this machine minted it, and
/// forgets it.
pub(crate) async fn run_logout(name: Option<&str>) -> i32 {
  match logout(name).await {
    Ok(done) => {
      eprintln!("{done}");
      0
    }
    Err(e) => {
      eprintln!("error: {e}");
      1
    }
  }
}

async fn logout(name: Option<&str>) -> Result<String, String> {
  let path = profiles_path().ok_or("there is no home directory with profiles in it")?;
  let mut profiles = Profiles::read(&path)?;
  let name = match name {
    Some(n) => n.to_string(),
    None => profiles.current.clone().ok_or("not signed in anywhere")?,
  };
  let profile = profiles
    .remove(&name)
    .ok_or_else(|| format!("no profile `{name}`"))?;
  let mut done = format!("Signed out of {} (profile `{name}`).", profile.server);
  if let Some(id) = &profile.key_id {
    let http = http_client()?;
    match send(
      &http,
      &profile.server,
      Some(&profile.api_key),
      Call::delete(format!("/aperio/api/admin-keys/{id}")),
    )
    .await
    {
      Ok(_) => done.push_str(" Its key is revoked."),
      Err(e) => done.push_str(&format!(
        " Its key ({id}) could not be revoked ({e}); revoke it from the dashboard or with `aperio-server admin revoke-key {id}`."
      )),
    }
  }
  profiles.write(&path)?;
  Ok(done)
}

/// Runs `profile list` or `profile use`.
pub(crate) fn run_profile(command: &ProfileCmd) -> i32 {
  let result = (|| -> Result<(), String> {
    let path = profiles_path().ok_or("there is no home directory with profiles in it")?;
    let mut profiles = Profiles::read(&path)?;
    match command {
      ProfileCmd::List => {
        if profiles.profiles.is_empty() {
          println!("Not signed in anywhere; `aperio-client login --server-url <url>` signs in.");
        }
        for (name, profile) in &profiles.profiles {
          let mark = if profiles.current.as_deref() == Some(name) {
            "*"
          } else {
            " "
          };
          println!("{mark} {name}  {}", profile.server);
        }
        Ok(())
      }
      ProfileCmd::Use { name } => {
        if !profiles.profiles.contains_key(name) {
          return Err(format!("no profile `{name}`"));
        }
        profiles.current = Some(name.clone());
        profiles.write(&path)?;
        eprintln!("Profile `{name}` is in use.");
        Ok(())
      }
    }
  })();
  match result {
    Ok(()) => 0,
    Err(e) => {
      eprintln!("error: {e}");
      1
    }
  }
}
