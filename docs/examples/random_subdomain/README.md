# Random Subdomains (preview environments)

> **Concept:** [Routing & Load Balancing](../../routing-and-load-balancing.md).


With `random_subdomain` set, each client gets a generated hostname such as `a1b2c3d4e5.example.com` in addition to its other binds. Set up wildcard DNS and a matching proxy route and certificate first. See [Random Subdomains](../../routing-and-load-balancing.md#random-subdomains).

The value is a pattern: the `*` in the leftmost label is replaced with a random label. `example.com` is shorthand for `*.example.com`; `*-preview.example.com` yields `<random>-preview.example.com`, same subdomain level, so one wildcard TLS certificate covers all generated hostnames. `preview_noindex: true` keeps the previews out of search engines (`X-Robots-Tag: noindex, nofollow` plus a disallow-all `/robots.txt` on the random hostname).

The client needs nothing special, the file below is a plain client; its assigned URL appears in the client log and the dashboard.
