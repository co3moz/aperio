# Tunnels

`aperio.yaml` declares MongoDB, SSH and DNS as tunnels next to them; `aperio-binder.yaml` binds them on another machine (`aperio-client --config aperio-binder.yaml bind`). The binder needs the declaring client's token, or one in its organization with `allow_bind`. See [Tunnels](../../tunnels.md).
