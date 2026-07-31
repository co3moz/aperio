# Static File Serving

Publishing a directory does not need a backend. `--serve ./dist` starts a tiny HTTP server inside the client, rooted at that directory, and exposes *it* through the tunnel:

```bash
aperio-client --serve ./dist --server-url https://tunnel.example.com --server-token apr_xxx
```

That is the whole setup for putting a built site online. The tunnel sees an ordinary HTTP target, so every regular feature applies unchanged: hostname and path binds, visitor passwords, the response cache, header rules, the request inspector, failover between two clients serving the same directory.

The file server listens on `127.0.0.1:0`, a random loopback port. Nothing else on the machine can reach it, and nothing needs to: the only way in is the tunnel you already authenticated.

## Where it can be configured

| Surface | How |
| --- | --- |
| CLI | `aperio-client --serve ./dist` |
| Environment | `APERIO_SERVE=./dist` |
| yaml | `serve:` on a `services:` entry |

A top-level `serve:` (with no `services:` list) still works and is the same thing, but it is **deprecated and removed in 0.7.0**, see [Configuration](configuration.md#aperioyaml--aperioyaml).

`serve:` takes the place of `target:` and the two are mutually exclusive, per entry. One client can therefore serve several directories on different binds, and mix static sites with proxied backends in the same process:

```yaml
services:
  - name: site
    serve: ./sites/marketing
    hostname: www.example.com

  - name: docs
    serve: ./sites/docs
    hostname: docs.example.com

  - name: api
    target: http://localhost:4000
    hostname: api.example.com
```

Serving survives [config hot-reload](client-resilience.md): a directory that was already being served keeps its listener across a reload, a newly added one gets a fresh listener, and a reload that fails validation leaves the running listeners alone rather than tearing down serving for a configuration it refused to apply.

## What it answers

`GET` and `HEAD`, nothing else, anything else gets `405`. A request path maps onto a file under the root; a directory resolves to its `index.html`, and a directory without one is a miss. The content type comes from the file extension, falling back to `application/octet-stream` for anything unrecognised.

Files are **streamed from disk** rather than read into memory, so serving a 2 GB video to ten visitors costs ten buffers, not 20 GB. A `HEAD` reads only the file's metadata and reports the `Content-Length` a `GET` would have returned, so it stays cheap no matter how large the file is. The path resolution around it is asynchronous too, so a slow filesystem (a network mount, a cold disk) delays the request that touched it rather than every other request the same worker thread was carrying.

### Range requests

Responses advertise `Accept-Ranges: bytes`, and a single-range `Range` header is answered with `206 Partial Content` and a `Content-Range`. This is what makes video scrubbing and resumable downloads work straight out of a served directory:

```
Range: bytes=2-5      → 206, Content-Range: bytes 2-5/10
Range: bytes=7-       → 206, the tail from offset 7
Range: bytes=-2       → 206, the final two bytes
Range: bytes=8-99     → 206, the end is clamped to the file
Range: bytes=99-      → 416, Content-Range: bytes */10
```

Three cases are deliberately served as a full `200` instead, which the HTTP spec permits for any `Range` a server chooses not to honor: **multi-range** requests (`bytes=0-1,4-5`), malformed values, and any request carrying **`If-Range`**. The last one is a correctness decision rather than a limitation: this server emits no `ETag` or `Last-Modified`, so it has no validator to compare an `If-Range` against, and guessing would risk splicing together a file that changed mid-download.

If a [cached](caching.md) entry covers the URL, the server answers the range at the edge from the stored body instead, without traversing the tunnel at all. That path *does* honor `If-Range`, because a cached entry carries a validator.

## Missing files

Two options refine what a miss looks like. Both are **process-wide** rather than per service: a client serving several directories applies the same SPA and 404 behavior to all of them. If one site is a router SPA and another must not be, run them from separate clients.

- **`serve_spa: true`** (env `APERIO_SERVE_SPA=1`) answers a *navigation* that resolves to no file with the root `index.html` and status `200`, which is what a client-side router (React Router, Vue Router) needs to own its routes. The index is streamed from disk on each navigation like any other file, so it is read fresh after a redeploy without the process being restarted.
- **`serve_404: ./dist/404.html`** (env `APERIO_SERVE_404`) serves a custom page with status `404` for whatever the SPA fallback does not cover. The file is read once at startup; an unreadable path logs a warning and is ignored rather than being fatal.

Without either, a miss is a plain-text `404`.

The SPA fallback deliberately triggers only when the request's `Accept` header explicitly prefers `text/html`. A generic `*/*`, which is what scripts, stylesheets, fonts and `fetch()` send, is excluded, so a missing hashed asset still `404`s instead of receiving `index.html` with a `200` and failing later as a syntax error in the console. That distinction is the difference between a debuggable deployment and a confusing one.

## Path safety

The root is canonicalised when the client starts, and every request path is checked twice against it.

First, before touching the filesystem: the path is percent-decoded, then any segment that is `..`, or contains a backslash or a colon, is rejected outright. Second, after resolving: the candidate is canonicalised and must still live under the root. The second check is what catches a **symlink** inside the served directory pointing somewhere else, which the first check cannot see.

The practical consequence is that a served directory exposes exactly the files under it and nothing above it, even if it contains symlinks you forgot about. It does not, however, filter *within* the root: a `.env` or a `.git/` directory sitting in the folder you published is published too. Serve a build output directory, not a working tree.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`static_site`](examples/static_site/): serve local directories, one or several
