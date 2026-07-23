# Emergency Tunnels, end-to-end encrypted

By default the server decodes and re-encodes tunnel frames, so a compromised server could read relayed bytes. A TCP tunnel declared with `encrypt: true` closes that hole: the two **clients** run an ephemeral X25519 key exchange and seal everything with ChaCha20-Poly1305, the server relays only ciphertext.

The optional `psk` protects against an *actively* hostile server (a man-in-the-middle of the key exchange): it is mixed into the key derivation on both ends and never transmitted, so a MITM without it derives mismatched keys and the stream dies instead of leaking data. Coordinate the PSK out-of-band and set the same value on both sides. `encrypt` is TCP-only.

Files: `aperio.yaml` (declaring side), `aperio-binder.yaml` (binder side), `aperio-server.yaml` (shared server).
