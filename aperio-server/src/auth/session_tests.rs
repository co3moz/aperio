//! The session cookie: which name it takes on http and https, that the
//! `__Host-` prefix keeps a neighbouring host from displacing it, and what
//! logout and the session-status endpoint answer.

use super::super::tests::*;
use super::*;
use crate::test_support::*;
use std::sync::Arc;

// --- session cookie ---------------------------------------------------------

#[test]
fn session_cookie_parses_named_value_among_others() {
  let mut h = HeaderMap::new();
  h.insert(
    "cookie",
    "foo=1; aperio_session=abc-123; bar=2".parse().unwrap(),
  );
  assert_eq!(session_cookie(&h), Some("abc-123"));

  // Only the aperio_session cookie is returned; other cookies are ignored.
  let mut other = HeaderMap::new();
  other.insert("cookie", "foo=1; bar=2".parse().unwrap());
  assert_eq!(session_cookie(&other), None);

  // A leading cookie without spaces is still matched after trimming.
  let mut lead = HeaderMap::new();
  lead.insert("cookie", "aperio_session=xyz".parse().unwrap());
  assert_eq!(session_cookie(&lead), Some("xyz"));

  assert_eq!(session_cookie(&HeaderMap::new()), None);
}

#[test]
fn a_prefixed_session_cookie_cannot_be_displaced_by_a_neighbour() {
  // The reason the prefix exists here. This server also serves other people's
  // sites, so a tenant on a sibling hostname can set a cookie for the parent
  // domain, but only an unprefixed one, since `__Host-` is host-only by the
  // browser's own rule. The prefixed cookie therefore has to win, or the
  // attacker's session would quietly replace the operator's.
  let mut both = HeaderMap::new();
  both.insert(
    "cookie",
    "aperio_session=attacker; __Host-aperio_session=mine"
      .parse()
      .unwrap(),
  );
  assert_eq!(session_cookie(&both), Some("mine"));

  // Order on the wire is not a promise either.
  let mut reversed = HeaderMap::new();
  reversed.insert(
    "cookie",
    "__Host-aperio_session=mine; aperio_session=attacker"
      .parse()
      .unwrap(),
  );
  assert_eq!(session_cookie(&reversed), Some("mine"));

  // On its own the old name still works: sessions issued before the prefix,
  // and every deployment that cannot set `Secure`, keep logging in.
  let mut legacy = HeaderMap::new();
  legacy.insert("cookie", "aperio_session=legacy".parse().unwrap());
  assert_eq!(session_cookie(&legacy), Some("legacy"));

  assert_eq!(session_cookie_name(true), SESSION_COOKIE_SECURE);
  assert_eq!(session_cookie_name(false), SESSION_COOKIE_PLAIN);
}

#[test]
fn every_sign_in_path_issues_the_prefixed_cookie() {
  // The prefix is the whole defence against a neighbouring tenant hostname:
  // `__Host-` may only be set by the exact host, over https, so a cookie a
  // tenant sets for the parent domain can never displace it. Password sign-in
  // asked for the right name from the start; OIDC and both passkey paths
  // wrote `aperio_session=` verbatim, which handed those users an unprefixed
  // session on a deployment whose reader then could not tell it from one a
  // neighbour set.
  let secure = session_set_cookie(true, "tok");
  assert!(secure.starts_with("__Host-aperio_session=tok;"), "{secure}");
  assert!(secure.contains("; Secure"), "{secure}");
  assert!(secure.contains("HttpOnly"), "{secure}");
  assert!(secure.contains("Path=/"), "{secure}");
  // Without https the prefix cannot be used at all: the browser would reject
  // a `__Host-` cookie that is not `Secure`, so a plain deployment would lose
  // its session entirely.
  let plain = session_set_cookie(false, "tok");
  assert!(plain.starts_with("aperio_session=tok;"), "{plain}");
  assert!(!plain.contains("Secure"), "{plain}");
}

#[test]
fn no_sign_in_path_spells_the_session_cookie_itself() {
  // The parity check behind the test above: a fourth sign-in path added later
  // would pass every functional test while quietly writing the name by hand
  // again, which is exactly how this shipped. Nothing outside `auth.rs` may
  // format a `Set-Cookie` for the session; there is one builder for it.
  fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        walk(&path, out);
      } else if path.extension().is_some_and(|e| e == "rs")
        && !path.to_string_lossy().contains("_tests")
        && let Ok(text) = std::fs::read_to_string(&path)
      {
        if path.ends_with("auth.rs") {
          continue;
        }
        // A literal that *issues* a cookie, told from the ones that merely
        // name it (the constants, the reader, a request `Cookie:` header a
        // test builds) by the attributes only a `Set-Cookie` carries.
        for (i, _) in text.match_indices("aperio_session={") {
          let tail = &text[i..(i + 160).min(text.len())];
          if tail.contains("HttpOnly") {
            let line = text[..i].matches('\n').count() + 1;
            out.push(format!("{}:{}", path.display(), line));
          }
        }
      }
    }
  }
  let mut offenders = Vec::new();
  walk(std::path::Path::new("src"), &mut offenders);
  assert!(
    offenders.is_empty(),
    "these format a session cookie by hand instead of calling \
     auth::session_set_cookie, so they cannot follow secure_cookies:\n  {}",
    offenders.join("\n  ")
  );
}

// --- logout / session-status / page handlers --------------------------------

#[tokio::test]
async fn logout_clears_session_and_cookie() {
  let mut cfg = test_config();
  cfg.secure_cookies = true;
  let state = Arc::new(test_state_with(cfg));
  let token = seed_session(&state, Role::Admin, None, None).await;
  let resp = auth_logout_handler(State(state.clone()), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(cookie.contains("Max-Age=0"));
  assert!(cookie.contains("Secure"));
  // The session is gone from the store.
  assert!(state.sessions.lock().await.get(&token).is_none());
}

#[tokio::test]
async fn logout_without_cookie_still_ok() {
  let state = Arc::new(test_state());
  let resp = auth_logout_handler(State(state), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn session_handler_reports_named_user_and_totp() {
  let state = test_state();
  let (_uid, _secret) = totp_user(&state, "sess").await;
  let token = seed_session(&state, Role::Operator, Some("sess"), None).await;
  let state = Arc::new(state);
  let resp = auth_session_handler(State(state), cookie_headers(&token)).await;
  let body = json_body(resp).await;
  assert_eq!(body["username"], "sess");
  assert_eq!(body["role"], "operator");
  assert_eq!(body["totp"], true);
  assert_eq!(body["master_admin"], false);
}

#[tokio::test]
async fn session_handler_defaults_without_cookie() {
  let state = Arc::new(test_state());
  let resp = auth_session_handler(State(state), HeaderMap::new()).await;
  let body = json_body(resp).await;
  assert_eq!(body["username"], "aperio");
  assert_eq!(body["expires_in_seconds"], 0);
}

#[tokio::test]
async fn session_handler_master_admin_selected_org() {
  let state = test_state();
  let token = seed_session(&state, Role::Admin, None, Some("org-9".to_string())).await;
  let state = Arc::new(state);
  let resp = auth_session_handler(State(state), cookie_headers(&token)).await;
  let body = json_body(resp).await;
  assert_eq!(body["master_admin"], true);
  assert_eq!(body["selected_org"], "org-9");
}

#[tokio::test]
async fn session_handler_unknown_token_defaults() {
  let state = Arc::new(test_state());
  // A cookie whose token is not in the store -> zeroed defaults.
  let resp = auth_session_handler(
    State(state),
    cookie_headers("11111111-1111-1111-1111-111111111111"),
  )
  .await;
  let body = json_body(resp).await;
  assert_eq!(body["expires_in_seconds"], 0);
}

#[tokio::test]
async fn auth_page_handler_serves() {
  let resp = auth_page_handler().await;
  // Embedded asset may be present (200) or absent in a bare build; either way
  // the handler returns a response without panicking.
  let _ = resp.status();
}
