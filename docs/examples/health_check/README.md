# Health checks

Each service probes its own backend; a failing one leaves routing without dropping the tunnel, and the others keep serving. See [A backend fails behind the client](../../traffic.md#a-backend-fails-behind-the-client).
