//! Two questions a route answers before anything is dispatched: whether a
//! method may be retried, and whether a visitor credential is well formed.

use super::*;
use crate::routing::method_retryable;

// --- method_retryable -------------------------------------------------------

#[test]
fn method_retryable_rules() {
  for m in ["GET", "HEAD", "OPTIONS", "PUT", "DELETE", "TRACE"] {
    assert!(method_retryable(m, false), "{m} should be retryable");
  }
  // Non-idempotent methods only when the opt-in is set.
  assert!(!method_retryable("POST", false));
  assert!(!method_retryable("PATCH", false));
  assert!(method_retryable("POST", true));
  assert!(method_retryable("PATCH", true));
}

// --- valid_visitor_creds ----------------------------------------------------

#[test]
fn visitor_creds_require_user_and_password() {
  assert!(valid_visitor_creds("user:password"));
  assert!(valid_visitor_creds("u:p"));
  // The password may itself contain ':' (only the first is the separator).
  assert!(valid_visitor_creds("user:pa:ss"));
  // Missing separator or an empty half is rejected.
  assert!(!valid_visitor_creds("userpassword"));
  assert!(!valid_visitor_creds(":password"));
  assert!(!valid_visitor_creds("user:"));
  assert!(!valid_visitor_creds(""));
  assert!(!valid_visitor_creds(":"));
}

#[test]
pub(crate) fn test_method_retryable() {
  // Idempotent methods may always fail over.
  for m in ["GET", "HEAD", "OPTIONS", "PUT", "DELETE", "TRACE"] {
    assert!(method_retryable(m, false), "{m} must be retryable");
  }
  // Non-idempotent methods need the explicit opt-in.
  for m in ["POST", "PATCH"] {
    assert!(!method_retryable(m, false), "{m} must not retry by default");
    assert!(method_retryable(m, true), "{m} must retry with the opt-in");
  }
}
