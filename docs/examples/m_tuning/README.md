# Tuning (multi-service)

Capacity knobs per `services:` entry, with top-level values as the shared default: the busy API gets parallel tunnel connections and a higher concurrency cap, the report generator gets a long timeout and a big response budget, and the media service is bandwidth-paced so downloads never saturate the uplink. Anything unset falls back to the top-level values — write shared tuning once.

Server-side ceilings are global (see [s_tuning](../s_tuning/) for those).
