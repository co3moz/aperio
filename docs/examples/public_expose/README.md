# Public listener

The server opens port 2222 and relays it into the `ssh` tunnel the client declares, so `ssh -p 2222 user@tunnel.example.com` reaches the client's sshd. Create the listener from the dashboard's **Tunnels** page or with `aperio-client api expose create --file expose.json`, and allow the port through the server's firewall. See [Public listeners](../../tunnels.md#public-listeners).
