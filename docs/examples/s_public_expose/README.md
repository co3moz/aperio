# Public Expose

> **Concept:** [Emergency Tunnels](../../emergency-tunnels.md).


An `expose:` entry cuts the binder out of the tunnel picture: the server itself opens a raw public TCP port and relays every accepted connection into a declared tunnel, useful for exposing SSH or a game server without running `--bind-tunnels` anywhere.

The entry names the tunnel and the token whose client may claim it, so the port has an owner: revoking that token closes its source, and another client in the same organization cannot take the name and receive the traffic. (The older `key:` shared-secret form still works, but it names no owner and cannot be revoked.) Deliberately limited: TCP only, since a public UDP port is an amplification surface; the connection goes to the **first** healthy client that matches, with no load balancing; and `encrypt: true` tunnels are excluded, because a raw public socket cannot run the client-side handshake. The exposed port is **public**, so keep the real authentication (SSH keys, database passwords) on the backend itself.

With this pair running, `ssh -p 2222 user@tunnel.example.com` lands on the declaring machine's local sshd.
