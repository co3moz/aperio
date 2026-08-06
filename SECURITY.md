# Security Policy

Aperio puts a private service on the public internet. That is the whole point
of it, and it is also why a bug here is worth more to an attacker than the same
bug somewhere else: the server terminates visitor traffic, holds tunnel
credentials, and speaks for every backend behind it. Reports are welcome and
taken seriously.

## Reporting a vulnerability

**Do not open a public issue for a security bug.** Use GitHub's private
reporting instead:

> [Report a vulnerability](https://github.com/co3moz/aperio/security/advisories/new)
> (repository → Security → Advisories → Report a vulnerability)

That opens a private advisory only the maintainers can read, with a place to
discuss and a way to credit you when it is published.

**What to include**, in whatever detail you have:

- What an attacker gains: reading another organization's data, bypassing a
  token's fence, reaching a backend that should not be reachable, denial of
  service, and so on. The impact is what decides the severity.
- The version (`aperio-server --version`) or commit.
- Enough to reproduce it: a configuration fragment, the requests, and what you
  saw. A `.yaml` reproducing it is worth more than a paragraph describing it.
- Whether it needs a valid tunnel token, a dashboard session, or neither. An
  unauthenticated path and an authenticated one are different bugs.

**What to expect:** an acknowledgement within a few days, an assessment of
whether it reproduces and how severe it is, and a fix released with a
`### Security` entry in [CHANGELOG.md](CHANGELOG.md) describing what was wrong
and what changed. Please give us a chance to release before publishing.

## Supported versions

Fixes land on the latest release. There is no long-term-support branch: this is
a two-binary project where upgrading is replacing a file, and pretending to
backport indefinitely would be a promise nobody could keep. Run the latest
release, and read [the upgrade guide](docs/upgrade-guide.md) before a jump.

## What is in scope

Anything that breaks a boundary the product claims to hold:

- **Organization isolation.** One organization reading, routing, or acting on
  another's clients, hostnames, tokens, captures, or events.
- **Token permissions.** A tunnel token binding a hostname it was not granted,
  exceeding its fence (IP allowlist, device pin, expiry), or escalating to a
  capability it does not carry.
- **The dashboard and admin API.** Authentication bypass, session fixation or
  theft, privilege escalation between the viewer/operator/admin roles, CSRF on
  a state-changing endpoint, stored or reflected XSS.
- **The tunnel protocol.** A malicious client reaching another client's
  traffic, a malformed frame crashing or hanging the server, resource
  exhaustion that is not bounded by a documented limit.
- **The proxy path.** Request smuggling, header injection, cache poisoning,
  reaching an internal address a route did not permit, or leaking one visitor's
  response to another.
- **Secrets in the wrong place.** A credential written to a log, an error page,
  an export, a capture, or a backup that was not supposed to carry it.

[docs/threat-model.md](docs/threat-model.md) states the boundaries in detail
and is the reference for what the product claims. A gap between that document
and the code is itself a report worth filing, whichever side turns out to be
wrong.

## What is out of scope

- Anything requiring the master token, the data directory, or shell access on
  the server: those are the keys to the whole system by design.
- A deployment choosing to expose something (`public: true`, a route pointing
  at an internal address on purpose, the dashboard published without TLS).
  Configuration that does what it says is not a vulnerability.
- Missing hardening headers on a page that has no session, output from
  automated scanners with no demonstrated impact, and volumetric denial of
  service from raw traffic volume, which is what the rate limits and the
  fronting proxy are for.
- Vulnerabilities in a dependency with no path to reach them from Aperio. We
  still want to hear about them; they are handled as ordinary upgrades rather
  than advisories.

## Verifying a release

Every release is signed and carries its own provenance, so "I downloaded a
binary from the internet" can be checked rather than trusted.

Each asset has a `.sha256` next to it, and the release carries a `SHA256SUMS`
file covering all of them, signed with [Sigstore](https://www.sigstore.dev/)
(keyless, so there is no long-lived private key to steal). With
[cosign](https://docs.sigstore.dev/cosign/system_config/installation/)
installed:

```bash
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp '^https://github.com/co3moz/aperio/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
sha256sum --check --ignore-missing SHA256SUMS
```

The release assets and the container images also carry **build provenance
attestations**, which say which workflow, which commit and which runner
produced them:

```bash
gh attestation verify aperio-server-x86_64-unknown-linux-musl.tar.gz --repo co3moz/aperio
gh attestation verify oci://ghcr.io/co3moz/aperio-server:latest --repo co3moz/aperio
```

An **SBOM** (SPDX, `aperio.spdx.json`) is attached to every release, listing
the dependency tree the binaries were built from, for anyone who has to answer
"are you affected by X" without reading our lockfile.
