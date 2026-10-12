# Failover

A request whose client dies before answering is re-dispatched to another client, or waits for one (`retry-wait`). Only idempotent methods fail over. See [A client dies mid-request](../../traffic.md#a-client-dies-mid-request).
