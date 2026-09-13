# Public Expose

> **Concept:** [Emergency Tunnels](../../emergency-tunnels.md).


An `expose:` entry cuts the binder out of the tunnel picture: the server itself opens a raw public TCP port and relays every accepted connection into a declared tunnel, useful for exposing SSH or a game server without running `--bind-tunnels` anywhere.

The entry names the tunnel and the organization whose client may claim it (`org:`, or the `<org>@<name>` spelling; neither written means master), so the claim is settled by identity rather than a secret copied into two files. Revoking the tokens of that organization closes the port's source. (The older `token:` form still works but is superseded: a token name is not unique, so a rule naming one can match a client of another organization.) Deliberately limited: TCP only, since a public UDP port is an amplification surface; the connection goes to the **first** healthy client that matches, with no load balancing; and `encrypt: true` tunnels are excluded, because a raw public socket cannot run the client-side handshake. The exposed port is **public**, so keep the real authentication (SSH keys, database passwords) on the backend itself.

With this pair running, `ssh -p 2222 user@tunnel.example.com` lands on the declaring machine's local sshd.
