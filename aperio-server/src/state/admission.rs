//! What a caller may do right now: token quotas and byte accounting, the
//! organization fence over hostnames, per-route and per-IP rate budgets, and
//! the maintenance flag that answers for a whole hostname.
//!
//! These are `impl AppState` rather than free functions because every one of
//! them is a question about the *whole* server, the stores and the live
//! connections together, and they are here rather than in `state.rs` because
//! they are the half of `AppState` a request path asks on the way in, not the
//! half that describes what the server is.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use super::client::ClientHandle;
use super::limits::{RateLimitState, TOKEN_MAP_GC_THRESHOLD, gc_token_daily_bytes, gc_token_rate};
use super::{AppState, MaintenanceFlag, RateCost};

impl AppState {
  /// In-memory thread-safe Per-IP Token Bucket Rate Limiter.
  /// Returns `true` if request is allowed, `false` if rate-limited.
  /// Enforces the serving token's optional rate limit and daily byte quota.
  /// Returns Err with a short reason when the request must be rejected with
  /// 429. Master-token traffic (token_id = None) is never limited.
  /// The error names *which* limit refused, so the refusal can name itself
  /// in a header rather than leaving the caller to parse a sentence.
  pub(crate) async fn check_token_limits(
    &self,
    token_id: Option<&str>,
  ) -> Result<(), crate::limits::Limit> {
    let Some(id) = token_id else {
      return Ok(());
    };
    // Limits are read from the store per request so dashboard edits apply
    // live; the store is small (dozens of tokens at most).
    let (max_rps, daily_max_bytes) = {
      let store = self.token_store.lock().await;
      match store.list().iter().find(|t| t.id == id) {
        Some(t) => (t.max_rps, t.daily_max_bytes),
        // Token revoked while its tunnel stays up: no limits to apply.
        None => return Ok(()),
      }
    };

    if let Some(rps) = max_rps.filter(|v| *v > 0.0) {
      let mut buckets = self.token_rate.lock().await;
      let now = Instant::now();
      gc_token_rate(&mut buckets, now);
      let burst = rps.max(1.0);
      let bucket = buckets.entry(id.to_string()).or_insert(RateLimitState {
        tokens: burst,
        last_updated: now,
      });
      let elapsed = now.duration_since(bucket.last_updated).as_secs_f64();
      bucket.tokens = (bucket.tokens + elapsed * rps).min(burst);
      bucket.last_updated = now;
      if bucket.tokens < 1.0 {
        return Err(crate::limits::Limit::TokenRate);
      }
      bucket.tokens -= 1.0;
    }

    if let Some(quota) = daily_max_bytes.filter(|v| *v > 0) {
      let today = crate::store::stats::period_keys()[0].clone();
      let usage = self.token_daily_bytes.lock().await;
      if let Some((day, used)) = usage.get(id)
        && *day == today
        && *used >= quota
      {
        return Err(crate::limits::Limit::TokenQuota);
      }
    }
    Ok(())
  }

  /// Attributes payload bytes to the serving token's daily usage (feeds the
  /// `daily_max_bytes` quota). The counter rolls over at local midnight.
  pub(crate) async fn add_token_bytes(&self, token_id: Option<&str>, bytes: u64) {
    let Some(id) = token_id else {
      return;
    };
    if bytes == 0 {
      return;
    }
    let today = crate::store::stats::period_keys()[0].clone();
    let mut usage = self.token_daily_bytes.lock().await;
    gc_token_daily_bytes(&mut usage, &today);
    let entry = usage
      .entry(id.to_string())
      .or_insert_with(|| (today.clone(), 0));
    if entry.0 != today {
      *entry = (today, bytes);
    } else {
      entry.1 = entry.1.saturating_add(bytes);
    }
  }

  /// May this organization act on `target` (put it into maintenance, mint a
  /// share link for it)? This is the isolation fence for the hostname-scoped
  /// operations, so that one tenant cannot 503 or hand out access to
  /// another's site.
  ///
  /// `target` is an exact hostname or a subdomain wildcard (`*.acme.com`),
  /// the same two shapes an organization's allowlist is written in. A
  /// wildcard is a claim over a whole subtree, so it takes an entry that owns
  /// the subtree: `acme.com` does not authorize `*.acme.com`, it authorizes
  /// one name and the request covers all the others.
  ///
  /// The question it answers is "may this org *serve* those names", not "is
  /// one of its clients serving them right now". Those came apart in
  /// practice: an org fenced to `x.com` could not put `x.com` into
  /// maintenance until a client for it was connected, which is precisely the
  /// case where maintenance mode is wanted, and a share link had to wait for
  /// the client to come back.
  ///
  /// A fenced org is judged by its allowlist. An unfenced child org has no
  /// allowlist to judge, so it falls back to the older test, one of its own
  /// connected clients serving the hostname, which is the only isolation left
  /// when the operator never drew a boundary. That test cannot prove a
  /// subtree, so an unfenced org cannot claim one.
  ///
  /// Master is fenced by the other organizations and by nothing else:
  /// everything no tenant claims is master's, which is what lets it act on a
  /// hostname with nothing connected, while a tenant's site stays the
  /// tenant's even from the super-admin's own screen. For a subtree that
  /// means no tenant may be anywhere inside it.
  pub(crate) async fn org_may_claim_hostname(&self, org: Option<&str>, target: &str) -> bool {
    use crate::store::orgs::{pattern_covers_pattern, patterns_overlap};

    let fences: Vec<(String, Vec<String>)> = {
      let store = self.org_store.lock().await;
      store
        .list()
        .iter()
        .map(|o| (o.id.clone(), o.hostnames.clone()))
        .collect()
    };
    // Hostnames of the clients of the organizations `org_matches` selects.
    let served = |org_matches: &dyn Fn(Option<&str>) -> bool,
                  clients: &std::collections::HashMap<String, ClientHandle>| {
      clients
        .values()
        .filter(|c| org_matches(c.perms.org_id.as_deref()))
        .flat_map(|c| {
          c.effective_hostnames()
            .into_iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
        })
        .collect::<Vec<String>>()
    };

    match org {
      Some(id) => {
        let own = fences
          .iter()
          .find(|(oid, _)| oid == id)
          .map(|(_, list)| list.clone())
          .unwrap_or_default();
        if !own.is_empty() {
          return own
            .iter()
            .any(|entry| pattern_covers_pattern(entry, target));
        }
        // No fence: the only claim left is a client of this org serving the
        // name, which says nothing about the rest of a subtree.
        if target.starts_with("*.") {
          return false;
        }
        let clients = self.clients.read().await;
        served(&|owner| owner == Some(id), &clients)
          .iter()
          .any(|h| h == target)
      }
      None => {
        if fences
          .iter()
          .flat_map(|(_, list)| list)
          .any(|entry| patterns_overlap(entry, target))
        {
          return false;
        }
        let clients = self.clients.read().await;
        !served(&|owner| owner.is_some(), &clients)
          .iter()
          .any(|h| pattern_covers_pattern(target, h))
      }
    }
  }

  /// True when `org` may *act* on `target`: not only whether its fence
  /// covers the name, but whether the name is somebody else's right now.
  ///
  /// [`Self::org_may_claim_hostname`] answers the first question, which is
  /// the right one for "may this name be bound" and for a read-only report:
  /// an organization has to be able to name a hostname before any of its
  /// clients has connected. It is not enough for an action. The master token
  /// is never fenced, so a master client can be serving a name inside an
  /// organization's fence, and coverage alone let that organization 503 it or
  /// mint a share link into it, which is the thing the fence exists to stop.
  ///
  /// So this adds the second half: nothing may be done to a hostname a client
  /// of a *different* organization is currently serving. A name nobody serves
  /// is still actionable, which is what keeps a fence usable before the first
  /// client connects.
  pub(crate) async fn org_may_act_on_hostname(&self, org: Option<&str>, target: &str) -> bool {
    if !self.org_may_claim_hostname(org, target).await {
      return false;
    }
    let clients = self.clients.read().await;
    !clients.values().any(|c| {
      c.perms.org_id.as_deref() != org
        && c
          .effective_hostnames()
          .into_iter()
          .any(|h| crate::store::orgs::pattern_covers_pattern(target, h))
    })
  }

  /// One beat of the background garbage collector: sweeps the per-IP and
  /// per-route rate buckets and the expired sessions.
  ///
  /// These retains used to run inline, under their lock, on the back of
  /// whichever request happened to draw the five-minute tick; a big map made
  /// that request (and everyone queued behind the lock) pay for the sweep.
  /// The size failsafes at the call sites stay, they are what bounds the maps
  /// between beats; this is the routine sweep that keeps the failsafes from
  /// ever being the mechanism.
  pub(crate) async fn gc_tick_once(&self, now: Instant) {
    self
      .rate_limiter
      .lock()
      .await
      .retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    self
      .route_rate
      .lock()
      .await
      .retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    let now_secs = crate::store::sessions::now_secs();
    self
      .sessions
      .lock()
      .await
      .retain(|info| info.expires_at > now_secs);
  }

  /// The maintenance flag in force for `host`, if any: an exact entry, a
  /// `*.suffix` wildcard covering it, or the server-wide `*`.
  ///
  /// One place rather than two, because there were two and they disagreed:
  /// the proxy matched wildcards and the autoscaler still asked
  /// `contains_key`, so a `*.robogon.com` flag served the 503 page and woke a
  /// scaled-to-zero service behind it at the same time.
  ///
  /// An expired flag simply does not match. It is not swept here: this is the
  /// read path holding a lock every request shares, and the write paths
  /// (setting a flag, listing them, deleting an organization) drop them.
  pub(crate) async fn maintenance_for(&self, host: Option<&str>) -> Option<MaintenanceFlag> {
    let now = crate::store::tokens::now_secs();
    // A scheduled window from the config file, if one is running. Checked
    // first and without taking the flag lock, so the common case of a server
    // with windows and no ad-hoc flag costs one list scan. A runtime flag can
    // still be raised during a window; it simply says the same thing.
    let cfg = self.config();
    if !cfg.maintenance_windows.is_empty()
      && let Some((window, until)) = cfg.maintenance_windows.active_for(host, now)
    {
      return Some(MaintenanceFlag {
        org: None,
        reason: window.reason.clone(),
        until: Some(until),
        since: now,
        // Named so the dashboard and the 503 page can tell an operator's
        // switch from the schedule doing what it was told.
        actor: "schedule".to_string(),
      });
    }
    let set = self.maintenance.lock().await;
    if set.is_empty() {
      return None;
    }
    set
      .iter()
      .filter(|(_, flag)| !flag.expired(now))
      .find(|(pattern, _)| {
        if *pattern == "*" {
          return true;
        }
        host.is_some_and(|h| {
          // An exact flag first, since that is the common case and needs no
          // matching, then anything with a placeholder in it: `*.robogon.com`
          // is one switch for every service under a domain, and the partial
          // shape (`*-pi.robogon.com`) is accepted by the same normalizer the
          // set handler runs, so it has to match here too. This arm used to
          // take only the `*.` shape, which made a partial flag a stored 200
          // that never served a single 503.
          *pattern == h
            || (pattern.contains('*') && crate::store::orgs::pattern_matches_host(pattern, h))
        })
      })
      .map(|(_, flag)| flag.clone())
  }

  /// The quota record for a child org (None for master or an unknown id).
  pub(crate) async fn org_quota(
    &self,
    org: Option<&str>,
  ) -> Option<crate::store::orgs::Organization> {
    let id = org?;
    self.org_store.lock().await.find(id).cloned()
  }

  // The token and user org quotas are enforced atomically inside their create
  // handlers (api/tokens.rs, api/users.rs), the cap count and the insert run
  // under one held store lock so concurrent creates can't overshoot the cap.

  /// Enforces the org's `max_clients` quota against currently-connected
  /// clients. Err(msg) when at the cap.
  pub(crate) async fn check_org_client_quota(&self, org: Option<&str>) -> Result<(), String> {
    let Some(max) = self.org_quota(org).await.and_then(|q| q.max_clients) else {
      return Ok(());
    };
    let count = self
      .clients
      .read()
      .await
      .values()
      .filter(|c| c.perms.org_id.as_deref() == org)
      .count() as u64;
    if count >= max {
      Err(format!("organization client quota reached ({max})"))
    } else {
      Ok(())
    }
  }

  /// True when the org is over its `max_bytes_month` quota for the current
  /// calendar month (proxied bytes in + out). False when no quota / no org.
  pub(crate) async fn org_over_month_bytes(&self, org: Option<&str>) -> bool {
    let Some(max) = self.org_quota(org).await.and_then(|q| q.max_bytes_month) else {
      return false;
    };
    let month_key = crate::store::stats::period_keys()[2].clone();
    let used = {
      let stats = self.persistent_stats.lock().await;
      stats
        .snapshot_for_org(org)
        .periods
        .get(&month_key)
        .map(|p| p.bytes_sent + p.bytes_received)
        .unwrap_or(0)
    };
    used >= max
  }

  /// Enforces the per-route rate limit for a request. Returns true when the
  /// request may proceed, false when the matched route's shared token bucket
  /// is empty (the caller answers 429). No configured rule for the
  /// host+path+method = always allowed.
  ///
  /// Two sources feed this, and a route's own `rate_limit:` wins over a
  /// `rate_limits:` entry matching the same request: the inline one is written
  /// next to the route it governs, so an operator reading that entry has seen
  /// the whole story.
  pub(crate) async fn check_route_rate_limit(
    &self,
    host: Option<&str>,
    path: &str,
    method: &str,
  ) -> bool {
    let cfg = self.config();
    let inline = cfg.static_routes.policy_for(host, path).and_then(|rule| {
      let rl = rule.rate_limit.as_ref()?;
      crate::route_limits::method_matches(rl.methods.as_ref(), Some(method)).then(|| {
        (
          rl.rps,
          rl.burst.filter(|b| *b > 0.0).unwrap_or(rl.rps).max(1.0),
          rule.rate_key.clone(),
        )
      })
    });
    let matched = match inline {
      Some(found) => Some(found),
      None if cfg.route_limits.is_empty() => None,
      None => cfg
        .route_limits
        .matched(host, path, Some(method))
        .map(|r| (r.rps, r.burst, r.key.clone())),
    };
    let Some((rps, burst, key)) = matched else {
      return true;
    };
    let mut buckets = self.route_rate.lock().await;
    let now = Instant::now();
    if buckets.len() > TOKEN_MAP_GC_THRESHOLD {
      buckets.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    }
    let bucket = buckets.entry(key).or_insert(RateLimitState {
      tokens: burst,
      last_updated: now,
    });
    let elapsed = now.duration_since(bucket.last_updated).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rps).min(burst);
    bucket.last_updated = now;
    if bucket.tokens < 1.0 {
      return false;
    }
    bucket.tokens -= 1.0;
    true
  }

  pub(crate) async fn check_rate_limit(&self, ip: IpAddr) -> bool {
    self.check_rate_limit_cost(ip, RateCost::Cheap).await
  }

  /// The same bucket, charged by what the request costs to serve
  /// (planned_features #64).
  ///
  /// Every admin call used to take exactly one token, so a login attempt, a
  /// token creation and a full export were charged the same. Two things follow
  /// from that, and both are wrong in the same way. A brute-force attempt gets
  /// the whole budget of a bucket sized for ordinary reads, and an operator
  /// running one legitimate export spends the same as one page view while
  /// costing the server a thousand times more.
  ///
  /// One bucket, different prices, rather than a bucket per class: separate
  /// buckets would let an attacker spend a full allowance on *each*, and the
  /// thing being protected, this server's capacity, is shared anyway.
  pub(crate) async fn check_rate_limit_cost(&self, ip: IpAddr, cost: RateCost) -> bool {
    self.charge_rate_limit(ip, cost.tokens()).await
  }

  async fn charge_rate_limit(&self, ip: IpAddr, cost: f64) -> bool {
    let mut limit_map = self.rate_limiter.lock().await;
    let now = Instant::now();

    // Stale buckets are swept by the background gc beat (`gc_tick_once`), not
    // here: a retain over tens of thousands of entries used to run on the
    // back of whichever request drew the five-minute tick, with the lock
    // held, which is a tail-latency spike by design. Only the size failsafe
    // stays inline, since it is what bounds the map between beats.
    if limit_map.len() > 1000 {
      limit_map.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    }

    let max_tokens = self.config().ip_limit_max;
    let refill_rate = self.config().ip_limit_refill;

    let state = limit_map.entry(ip).or_insert_with(|| RateLimitState {
      tokens: max_tokens,
      last_updated: now,
    });

    let elapsed = now.duration_since(state.last_updated).as_secs_f64();
    state.tokens = (state.tokens + elapsed * refill_rate).min(max_tokens);
    state.last_updated = now;

    if state.tokens >= cost {
      state.tokens -= cost;
      true
    } else {
      false
    }
  }
}
