# Headers (multi-service)

Header rules per `services:` entry: an entry's `headers:` section **replaces** the top-level one entirely (no merging), so each service controls its own edits, here the web app gets the shared defaults, while the API strips different headers and tags its responses.

The server-side `headers:` still applies on top of everything, across all services (see [s_headers](../s_headers/)).
