//! Stopping properly: the minimum dashboard role each route demands, and the
//! drain budget a shutdown spends, including what `auto` resolves to.

use crate::*;

// ---------------------------------------------------------------------------
// required_role, minimum dashboard role per route
// ---------------------------------------------------------------------------

#[test]
fn test_required_role_self_service_routes() {
  use crate::required_role;
  use crate::store::users::Role;
  use axum::http::Method;
  // Self-service /api/me/* is open to any signed-in role, even for mutations.
  assert_eq!(
    required_role("/api/me/totp/setup", &Method::POST),
    Role::Viewer
  );
  assert_eq!(required_role("/api/me/totp", &Method::DELETE), Role::Viewer);
  assert_eq!(
    required_role("/api/me/passkeys", &Method::GET),
    Role::Viewer
  );
}

#[test]
fn test_required_role_admin_only_routes() {
  use crate::required_role;
  use crate::store::users::Role;
  use axum::http::Method;
  // Routes that can change who controls the server are admin-only, including
  // their GETs.
  for (path, method) in [
    ("/api/users", Method::GET),
    ("/api/users", Method::POST),
    ("/api/users/123", Method::PUT),
    ("/api/settings", Method::GET),
    ("/api/settings", Method::PUT),
    ("/api/export", Method::GET),
    ("/api/import", Method::POST),
    ("/api/sessions", Method::GET),
    ("/api/sessions/abc", Method::DELETE),
    ("/api/orgs", Method::GET),
    ("/api/orgs/o1/quota", Method::PUT),
    ("/api/admin-keys", Method::GET),
    ("/api/admin-keys/k1", Method::DELETE),
  ] {
    assert_eq!(
      required_role(path, &method),
      Role::Admin,
      "{path} {method} must be admin-only"
    );
  }
}

#[test]
fn test_required_role_reads_vs_mutations() {
  use crate::required_role;
  use crate::store::users::Role;
  use axum::http::Method;
  // Generic reads are open to viewers...
  assert_eq!(required_role("/api/stats", &Method::GET), Role::Viewer);
  assert_eq!(required_role("/api/logs", &Method::HEAD), Role::Viewer);
  // ...and generic mutations require operator.
  assert_eq!(required_role("/api/purge", &Method::POST), Role::Operator);
  assert_eq!(
    required_role("/api/tokens/t1", &Method::DELETE),
    Role::Operator
  );
  assert_eq!(
    required_role("/api/clients/c1/enabled", &Method::POST),
    Role::Operator
  );
}

// ---------------------------------------------------------------------------
// shutdown_drain_budget (planned_features #58)
// ---------------------------------------------------------------------------

#[test]
fn shutdown_drain_defaults_to_not_waiting() {
  // Unset is the behavior the server has always had: notify, flush, close.
  // Waiting is something an operator asks for, not something a version bump
  // starts doing to their deploys.
  assert_eq!(
    shutdown_drain_budget(None, false, []),
    std::time::Duration::ZERO
  );
}

#[test]
fn shutdown_drain_uses_the_configured_number_over_anything_announced() {
  // The operator's number wins even when clients ask for more: this is the
  // one place the platform's SIGKILL timer is known, and it is not known here.
  assert_eq!(
    shutdown_drain_budget(Some(5), true, [60, 90]),
    std::time::Duration::from_secs(5)
  );
}

#[test]
fn shutdown_drain_auto_takes_the_longest_client_and_caps_it() {
  // The longest, not the average: the drain is over when the slowest client
  // has finished, and an average cuts short exactly the one that needed time.
  assert_eq!(
    shutdown_drain_budget(None, true, [3, 12, 7]),
    std::time::Duration::from_secs(12)
  );
  // A client is not the operator, so what it announces cannot hold the
  // process past what the platform will wait before SIGKILL.
  assert_eq!(
    shutdown_drain_budget(None, true, [3600]),
    std::time::Duration::from_secs(SHUTDOWN_DRAIN_AUTO_CAP)
  );
  // `auto` with nothing connected has nothing to size itself from.
  assert_eq!(
    shutdown_drain_budget(None, true, []),
    std::time::Duration::ZERO
  );
}
