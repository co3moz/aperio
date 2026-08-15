# Conformance

External test suites, run against Aperio rather than written for it.

Everything under [`e2e/`](../e2e/) asserts what we believe the product should
do, which is exactly its limit: a suite written by the same people who wrote
the implementation shares the implementation's misunderstandings. These runs
are the other kind of evidence. They are also the only kind a prospective
user can weigh without reading our code, which for a proxy is the whole
question.

Not part of `npm --prefix tests/e2e test`: they take minutes, and the Autobahn
run needs Docker.

## Autobahn (WebSocket)

```bash
npm --prefix tests/conformance install     # once
npm --prefix tests/conformance test        # the whole suite
node tests/conformance/autobahn.mjs --cases '1.*,7.*'   # one group
node tests/conformance/autobahn.mjs --keep              # keep the data dir
```

The [Autobahn test suite](https://github.com/crossbario/autobahn-testsuite)
is the standard conformance suite for RFC 6455: several hundred cases over
framing, fragmentation, close codes, UTF-8 validity in text frames, ping/pong
and payloads up to 16 MB. `autobahn.mjs` brings up the whole path and points
the suite at the far end of it:

```
[ autobahn fuzzingclient ] --ws--> [ aperio-server ] ==tunnel==> [ aperio-client ] --ws--> [ ws echo ]
```

So every frame crosses the relay twice, out and back. The backend is
[`ws`](https://github.com/websockets/ws), which passes the suite cleanly on
its own, and that is what makes a failure here a statement about the relay
rather than about the echo server.

**What counts as a failure.** Autobahn grades each case `OK`, `NON-STRICT`,
`INFORMATIONAL`, `UNIMPLEMENTED` or `FAILED`, twice: once for the frames
(`behavior`) and once for the close handshake (`behaviorClose`). Only `FAILED`
(and its `WRONG CODE` / `UNCLEAN` variants) fails the run. `NON-STRICT` means
a permitted-but-less-strict choice, which for a relay is frequently the
honest answer, so it is printed and counted rather than treated as a bug. The
close grade matters as much as the frame grade: a relay that carries the data
correctly and mangles the close code is precisely the bug this exists to
catch.

**What is not run.** The `12.*` and `13.*` groups test
`permessage-deflate`, which the tunnel deliberately does not negotiate (see
`planned_features.md` `#73`, withdrawn). They are excluded by name rather
than tolerated in the results, so the report says what was actually run.

**Docker.** The suite is Python 2 and is only sanely available as the
`crossbario/autobahn-testsuite` image. On Linux the container shares the
host's network namespace and dials `127.0.0.1`; on Docker Desktop it reaches
the host as `host.docker.internal`.

That difference is why the tunnel is bound on a **path** here rather than a
hostname. Autobahn dials a URL, and the authority of that URL is the `Host`
header the server routes on, so a hostname bind would mean teaching the
container to resolve a name that exists nowhere and getting the answer right
on two platforms. Bound on `/`, what the machine is called stops mattering.

The HTML report lands in `reports/` and is published as a CI artifact.

## In CI

[`conformance.yml`](../../.github/workflows/conformance.yml) runs this
weekly and on demand, not per push: it is minutes long and the thing it
checks changes rarely. A failure is a bug in the relay, not a flaky test.

## h2spec (HTTP/2)

```bash
node tests/conformance/h2spec.mjs          # both runs and the comparison
node tests/conformance/h2spec.mjs --keep   # keep the processes and data dir
```

[h2spec](https://github.com/summerwind/h2spec) is the conformance suite for
RFC 9113, 146 cases over frames, flow control, header compression and stream
states. It is a conformance *client*, so it tests servers, and there is a
server here to test: `axum::serve` accepts **h2c with prior knowledge**, so a
visitor can speak HTTP/2 to Aperio directly. The binary is a single Go
executable, downloaded into `.h2spec/` on first run.

It runs twice against one server:

```
[ h2spec ] --h2c--> [ aperio-server ]                          (baseline)
[ h2spec ] --h2c--> [ aperio-server ] ==tunnel==> [ backend ]  (proxied)
```

**The gate is the difference between them**, and that is the point rather
than a detail. Nearly every case exercises frame and connection handling that
belongs to hyper, so an absolute score describes the stack and not this
project. A case that passes when the server answers for itself and fails when
the same server is proxying describes the relay, and only the relay. It is
the same reasoning that made the Autobahn run use a backend that passes the
suite on its own.

A difference is **re-run on both sides before it fails the build**, because
the GOAWAY cases turned out to be timing-sensitive here: across four
measured runs, "Sends a GOAWAY frame" failed three times and "GOAWAY with
unknown error code" once, on paths that were otherwise identical. Two of
three trial runs of this harness saw a case differ and none of them survived
the confirmation. A gate that flakes is worse than no gate.

At the time of writing the stack fails two cases on both paths, so neither
gates: `http2/3.5 Sends invalid connection preface` and `generic/3.8 Sends a
GOAWAY frame`. `reports/h2spec-summary.json` records the run.

**What this does not cover:** the `h2://` *backend* transport, where Aperio is
the HTTP/2 client. h2spec tests servers; testing a client needs a
deliberately non-conformant server, which is a different tool and a different
entry.
