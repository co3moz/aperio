# Public Expose (experimental)

> **Concept:** [Emergency Tunnels](../../emergency-tunnels.md).


An `expose:` entry cuts the binder out of the emergency-tunnel picture: the server itself opens a raw public TCP port and relays every accepted connection to the declared tunnel of the client presenting the matching key, useful for exposing SSH or a game server without running `--bind-tunnels` anywhere.

The `key` is a shared secret between the two config files (minimum 8 characters; it is the only thing gating the port, so make it long and random). Deliberately limited while experimental: TCP only, the connection goes to the **first** healthy client declaring the key (no load balancing), and `encrypt: true` tunnels are excluded. The exposed port is **public**, keep the real authentication (SSH keys, database passwords) on the backend itself.

With this pair running, `ssh -p 2222 user@tunnel.example.com` lands on the declaring machine's local sshd.
