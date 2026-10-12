# Serving from the cache while offline

`cache: {enabled: true, offline: true}` keeps a service's cached pages answering while no client of it is connected; the API beside it fails instead of serving stale data. See [Response cache](../../traffic.md#response-cache).
