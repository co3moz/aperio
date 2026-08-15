// k6 soak / load test for a running Aperio server.
//
// Run against a deployment or a local stack to check sustained throughput and
// latency:
//
//   k6 run -e BASE_URL=http://localhost:8080 -e HOST=app.example.com tests/soak/k6.js
//
// It ramps virtual users up, holds a soak plateau, and ramps down, sending
// GETs through a proxied hostname. Thresholds fail the run if the error rate or
// p95 latency degrades past the configured budget.
//
// `run.mjs` also drives this file: it brings a whole stack up, hands the
// durations in as `RAMP`/`PLATEAU`/`VUS`, and watches RSS while this holds the
// load. That is the scheduled half; this file on its own is still the manual
// one.

import http from 'k6/http'
import { check, sleep } from 'k6'

const BASE_URL = __ENV.BASE_URL || 'http://localhost:8080'
const HOST = __ENV.HOST || 'localhost'
const PATH = __ENV.PATH || '/'
// Overridable so the scheduled run can pick its own shape without a second
// copy of this profile drifting away from it.
const VUS = Number(__ENV.VUS || 20)
const RAMP = __ENV.RAMP || '30s'
const PLATEAU = __ENV.PLATEAU || '3m'

export const options = {
  scenarios: {
    soak: {
      executor: 'ramping-vus',
      startVUs: 0,
      stages: [
        { duration: RAMP, target: VUS }, // ramp up
        { duration: PLATEAU, target: VUS }, // soak plateau
        { duration: '30s', target: 0 }, // ramp down
      ],
      gracefulRampDown: '10s',
    },
  },
  thresholds: {
    http_req_failed: ['rate<0.01'], // < 1% errors
    http_req_duration: ['p(95)<500'], // p95 under 500ms
  },
}

export default function () {
  const res = http.get(`${BASE_URL}${PATH}`, { headers: { Host: HOST } })
  check(res, {
    'status is 2xx/3xx': (r) => r.status >= 200 && r.status < 400,
  })
  sleep(1)
}
