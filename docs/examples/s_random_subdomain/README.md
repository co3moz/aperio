# Random Subdomains (preview environments)

With `random_subdomain` set (and a wildcard DNS/proxy route in front), every connecting client is automatically assigned a hostname like `a1b2c3d4e5.example.com`, per connection, additive to its other binds. Perfect for per-branch preview deployments: every `aperio-client 3000` gets its own URL with zero configuration.

The value is a pattern: the `*` in the leftmost label is replaced with a random label. `example.com` is shorthand for `*.example.com`; `*-preview.example.com` yields `<random>-preview.example.com`, same subdomain level, so one wildcard TLS certificate covers all generated hostnames. `preview_noindex: true` keeps the previews out of search engines (`X-Robots-Tag: noindex, nofollow` plus a disallow-all `/robots.txt` on the random hostname).

The client needs nothing special, the file below is a plain client; its assigned URL appears in the client log and the dashboard.
