# Installing: packages, formulae and service units

Every release attaches a `.deb` and an `.rpm` for both binaries, on `amd64` and
`arm64`. They exist so that installing on an ordinary Linux box is a package
manager and a `systemctl enable`, rather than `install.sh` followed by writing
your own unit file, which is where hardening quietly does not happen.

```bash
# Debian, Ubuntu
sudo dpkg -i aperio-server_0.9.0_amd64.deb

# Fedora, RHEL, openSUSE
sudo rpm -i aperio-server-0.9.0-1.x86_64.rpm
```

Both packages can be installed on the same machine; they share no files.

## macOS and Windows

Every release also carries a Homebrew formula and a Scoop manifest for each
binary, rendered from the release's own checksums. Neither needs a tap or a
bucket to exist, because the file itself is a release asset:

```bash
# macOS or Linux, Apple Silicon or Intel
brew install --formula https://github.com/co3moz/aperio/releases/latest/download/aperio-client.rb
```

```powershell
# Windows
scoop install https://github.com/co3moz/aperio/releases/latest/download/aperio-client.json
```

A tap is nicer when it exists (`brew install co3moz/aperio/aperio-client`, and
`brew upgrade` keeps working afterwards), and the release workflow pushes the
formulae to one when it is configured. See *Maintainers* at the end of this
page.

## What a package puts where

| Path | What it is |
| --- | --- |
| `/usr/bin/aperio-server`, `/usr/bin/aperio-client` | the binaries |
| `/usr/lib/systemd/system/aperio-server.service` | the server unit |
| `/usr/lib/systemd/system/aperio-client@.service` | the client unit, one instance per config file |
| `/etc/aperio/aperio-server.yaml` | the server's config, `0640`, never overwritten by an upgrade |
| `/etc/aperio/aperio-client.yaml.example` | the template an instance is copied from |
| `/var/lib/aperio` | the server's SQLite store |
| `/var/lib/aperio-client/<instance>` | a client instance's state (its persistent client id) |

Installing does **not** start anything. The shipped server config carries a
placeholder master token, and a tunnel server that comes up on `apt install`
with a token that was published in a package is worse than one that does not
come up at all. Set a real token first:

```bash
sudoedit /etc/aperio/aperio-server.yaml     # change server.token
sudo systemctl enable --now aperio-server
```

The server listens on `127.0.0.1:8080` and does not terminate TLS. Put a
reverse proxy in front of it: see [edge-proxy.md](edge-proxy.md).

## One client instance per thing you expose

`aperio-client@.service` is a template, and the name after the `@` is the
config file it reads. A machine usually fronts more than one thing, and the
alternative to a template is copies of a unit file that drift apart.

```bash
sudo cp /etc/aperio/aperio-client.yaml.example /etc/aperio/myapp.yaml
sudoedit /etc/aperio/myapp.yaml              # server url, token, target, hostname
sudo systemctl enable --now aperio-client@myapp

systemctl status aperio-client@myapp
journalctl -u aperio-client@myapp -f
```

## What the units refuse

Both units run as the `aperio` system account, created by `systemd-sysusers`
when the package is installed, and both are sandboxed the same way. The list is
not tuning, it is the set of things you would otherwise have to trust the
process not to do:

- `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`
- `PrivateTmp`, `PrivateDevices`, `ProtectProc=invisible`
- `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectKernelLogs`,
  `ProtectControlGroups`, `ProtectClock`, `ProtectHostname`
- `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`,
  `LockPersonality`, `MemoryDenyWriteExecute`
- `SystemCallFilter=@system-service`, `SystemCallArchitectures=native`
- `RestrictAddressFamilies`: IP only for the server, IP and unix sockets for
  the client, which may be pointed at a backend over a socket

`ProtectSystem=strict` makes the whole filesystem read-only except for what is
named, which is why moving the data directory takes two edits rather than one:

```bash
sudo systemctl edit aperio-server
```
```ini
[Service]
Environment=APERIO_DATA_DIR=/srv/aperio
ReadWritePaths=/srv/aperio
```

The same override is how a client that serves a directory it must also *write*
to gets permission for exactly that path.

**Why a named account and not `DynamicUser`.** A transient account is otherwise
the better answer, and it cannot work here: the config file holds a token and
so is not world-readable, and a file that is not world-readable has to be
readable by someone with a name. A dynamic user has a different uid on every
start, so no mode on that file admits it and nothing else.

## Secrets from somewhere other than the file

Every key in the config is also an `APERIO_*` environment variable, which is
what to reach for when a value comes from a secret store:

```bash
sudo systemctl edit aperio-server
```
```ini
[Service]
Environment=APERIO_SERVER_TOKEN=...
```

An override wins over the file, and it keeps the token out of a file that
backups and configuration management copy around.

## Upgrades

`/etc/aperio/aperio-server.yaml` is marked as a config file in both formats, so
an upgrade never overwrites your edits; a changed default arrives beside it as
`.dpkg-dist` or `.rpmnew` for you to diff. The package restarts what was
already running and starts nothing that was not.

The `version:` key in the shipped config is stamped with the version that built
the package. Keep it current when you upgrade: it is what lets the binary tell
you which configuration-format changes landed in between, and refuse to start
when one of them has security consequences. See
[upgrade-guide.md](upgrade-guide.md).

## Maintainers

The Homebrew formulae and Scoop manifests are generated by
`packaging/render-manifests.sh`, which reads the `<file>.sha256` written beside
every release asset rather than recomputing anything, so a formula and the
signed checksum manifest cannot disagree: they are the same number, read once.
To see what a release would produce, point it at that release's assets:

```bash
gh release download v0.9.0 --pattern '*.sha256' --dir /tmp/assets
packaging/render-manifests.sh 0.9.0 /tmp/assets /tmp/manifests
brew style /tmp/manifests/aperio-client.rb
```

Pushing to a tap needs a second repository, so it is the one part that is not
self-contained, and it is **optional**: with nothing configured the release
still attaches the formulae and logs a notice. To turn it on, create a
`homebrew-<name>` repository, then set on this repository:

- the variable `HOMEBREW_TAP_REPO`, e.g. `co3moz/homebrew-aperio`
- the secret `HOMEBREW_TAP_TOKEN`, a token that may push to it

A release must not fail because an optional distribution channel is not set up,
so a missing token skips the step rather than breaking the build.
