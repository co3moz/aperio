// What the explain endpoint's message codes say, in English.
//
// The server answers twice over: `detail` is a finished English sentence, for
// a CLI or an API caller, and `code` + `params` are the same sentence as a
// name and its values. The dashboard renders the second, because the first is
// English and this screen is not.
//
// The English here is the translation key, same as everywhere else in the
// dashboard, so `t(EXPLAIN_MESSAGES[code], step.params)` is an ordinary
// lookup. A code with no entry falls back to `detail`: a server newer than
// the dashboard says something in English rather than nothing.
//
// One code per *sentence shape*, not per stage. A maintenance flag with a
// reason and a lifting time is a different entry from one with neither, so a
// translator is handed a whole sentence instead of three fragments to glue in
// an order their language may not use.

/** `code` → the English sentence, with `{placeholders}` for `params`. */
export const EXPLAIN_MESSAGES: Record<string, string> = {
  'maintenance.flagged':
    '503: a maintenance flag set by {actor} covers this hostname, until someone turns it off',
  'maintenance.flagged_reason':
    '503: a maintenance flag set by {actor} covers this hostname ({reason}), until someone turns it off',
  'maintenance.flagged_until':
    '503: a maintenance flag set by {actor} covers this hostname, lifting at unix {until}',
  'maintenance.flagged_reason_until':
    '503: a maintenance flag set by {actor} covers this hostname ({reason}), lifting at unix {until}',
  'maintenance.none': 'no maintenance flag covers this hostname',

  'static_route.none_configured': 'no routes: rules configured',
  'static_route.answers': '{status}: a routes: rule answers this path',
  'static_route.answers_location': '{status}: a routes: rule answers this path, to {location}',
  'static_route.no_match': 'no routes: rule matches this hostname and path',

  'preview_noindex.robots':
    '200: preview_noindex answers /robots.txt itself, so this path never reaches a client',

  'waf.none_configured': 'no waf: rules configured',
  'waf.denied': '403: blocked by a waf: rule ({reason})',
  'waf.no_match':
    'no waf: rule matches this method and path (header and body rules need a real request)',

  'route_rate_limit.covered':
    'a rate_limits: rule covers this path ({rps} rps, burst {burst}); this dry run does not spend from it',
  'route_rate_limit.covered_methods':
    'a rate_limits: rule covers this path ({rps} rps, burst {burst}, {methods} only); this dry run does not spend from it',
  'route_rate_limit.none': 'no rate_limits: rule covers this path',

  'visitor_gate.client_password':
    "visitors must sign in (or carry a share link) before this reaches a client: the serving client declared a visitor password for this route, which supersedes the server's own gate",
  'visitor_gate.server_password_and_oidc':
    "visitors must sign in (or carry a share link) before this reaches a client: the server's visitor gate is on, and OIDC is configured",
  'visitor_gate.server_password':
    "visitors must sign in (or carry a share link) before this reaches a client: the server's visitor password is set",
  'visitor_gate.oidc':
    'visitors must sign in (or carry a share link) before this reaches a client: OIDC is configured for visitors',
  'visitor_gate.open': 'this hostname is served without a visitor gate',
  'visitor_gate.undeclared':
    'nothing declares this route reachable and `default_access` is `deny`, so it is not published: a visitor carrying no Aperio session gets the same answer an unclaimed hostname gives, and a signed-in Aperio user still reaches it',

  'routing.candidates': '{count} client(s) would take it: {clients}',
  'routing.none': 'no connected client serves this hostname and path',
  'routing.none_ineligible':
    'no connected client serves this hostname and path, though {count} could: {ineligible}',

  'cold_start.armed':
    'an autoscaling record is armed for this bind, so the request would be held while capacity is asked for, rather than answered at once',

  'fallback.redirect':
    '{status}: the fallbacks: rule for this hostname redirects to {url} instead of a 504',
  'no_client.504': '504: nothing serves this route, and no fallbacks: rule covers it',

  'client.reached': 'the request reaches a tunnel client ({clients})',

  // The limits, in the order the proxy applies them. Several of these report
  // a rule rather than a verdict, because a request nobody sends has no
  // visitor IP, no token and no body, and saying "passes" about a check that
  // was never run is the confident wrong answer this screen exists to avoid.
  'denied_ips.none': 'no source-IP deny list is configured',
  'denied_ips.configured':
    'a source-IP deny list is in force ({count} entries); a request from a listed address is refused before anything below runs, whatever this report says',

  'rate_limit_ip.configured':
    'every visitor IP gets a bucket of {max} with {refill}/s refill; this dry run does not spend from it',

  'server_concurrency.ceiling':
    'the server admits {max} requests at once across every service; one arriving with no slot free is refused rather than queued',
  'server_concurrency.headroom':
    '{in_flight} of {max} server-wide request slots are in use right now; a request arriving with none free is refused rather than queued',

  'allowed_ips.restricted':
    'the serving service declares allowed_ips, so a visitor outside that list is routed nowhere and gets the 504 below rather than a refusal naming the list',

  'body_limit.effective':
    'a request body over {effective} bytes is refused with 413; the service may tighten the server\'s limit but never widen it',

  'client_concurrency.declared':
    'the serving client admits {max} at a time; past that a request waits for a slot and is refused if none frees before the gateway timeout',
  'client_concurrency.unlimited': 'the serving client declares no concurrency limit of its own',

  'cache.eligible':
    'this route is cacheable, so a fresh entry would answer here without the request reaching a client at all',
  'cache.off': 'response caching is off server-wide',
  'cache.method': 'caching applies to GET; a {method} always reaches a client',
  'cache.not_opted_in':
    'the serving service does not opt into caching, so every request goes to the backend',

  'token_quota.depends_on_token':
    "a dynamic token carries its own requests-per-second limit and daily byte quota; which token a request arrives with is not a property of this route, so neither is checked here",
  'token_quota.daily': "the same token's daily byte quota is likewise a property of the credential",

  'org_quota.within': 'this organization is within its monthly byte quota',
  'org_quota.exhausted':
    '429: this organization is over its monthly byte quota, so every request it serves is refused until the month turns',
  'org_quota.master': 'the master organization has no monthly byte quota',

  'streams_per_ip.unlimited': 'no ceiling on concurrently open streamed responses per visitor',
  'streams_per_ip.capped':
    'one visitor IP may hold {max} streamed responses open at once; a streamed answer past that is refused',

  // Not a step: what stands in a client list for a client the reader's
  // organization is not entitled to be told about. It is still counted, so
  // "something else serves this" stays answerable.
  'client.other_org': "another organization's client",
}

/** Why a connected client was passed over, one reason per code. */
export const EXPLAIN_INELIGIBLE: Record<string, string> = {
  'ineligible.disabled': 'disabled from the dashboard',
  'ineligible.draining': 'draining',
  'ineligible.backend_unhealthy': 'its backend health probe is failing',
  'ineligible.missed_heartbeats': 'missed heartbeats',
  'ineligible.path_mismatch': 'its path bind does not match',
}

/** The stage names, which are identifiers on the wire and prose on screen. */
export const EXPLAIN_STAGES: Record<string, string> = {
  maintenance: 'Maintenance',
  static_route: 'Static route',
  preview_noindex: 'Preview noindex',
  waf: 'WAF',
  route_rate_limit: 'Rate limit',
  visitor_gate: 'Visitor gate',
  routing: 'Routing',
  cold_start: 'Cold start',
  fallback: 'Fallback',
  no_client: 'No client',
  denied_ips: 'Deny list',
  rate_limit_ip: 'Visitor rate limit',
  server_concurrency: 'Server concurrency',
  allowed_ips: 'Allowed IPs',
  body_limit: 'Body limit',
  client_concurrency: 'Client concurrency',
  cache: 'Cache',
  token_quota: 'Token quota',
  org_quota: 'Organization quota',
  streams_per_ip: 'Streams per visitor',
}

// Where a decision comes from. Most are a config key, `routes:` or `waf:`,
// the same word in every language and left alone; these are the few that were
// written as prose and so have to be said in the reader's.
export const EXPLAIN_SETTINGS: Record<string, string> = {
  'setting.default_access': 'default_access',
  'setting.maintenance_mode': 'maintenance mode',
  'setting.service_auth': 'auth: on the service',
  'setting.server_auth_oidc': 'server_auth / OIDC',
  'setting.server_auth': 'server_auth',
  'setting.oidc': 'OIDC',
  'setting.host_path_binds': 'hostname/path binds',
}
