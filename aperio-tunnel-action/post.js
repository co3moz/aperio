// Cleanup for the Aperio tunnel action: stops the client container and
// revokes the ephemeral token. Best-effort — the token has a TTL anyway.
'use strict'

const { execFileSync } = require('node:child_process')

function input(name, fallback) {
  const value = process.env[`INPUT_${name.toUpperCase()}`]
  return value === undefined || value === '' ? fallback : value
}

async function main() {
  const container = process.env.STATE_container
  if (container) {
    try {
      execFileSync('docker', ['rm', '--force', container], { encoding: 'utf8' })
      console.log(`Removed tunnel client container ${container}`)
    } catch (e) {
      console.warn(`Could not remove container ${container}: ${e.message}`)
    }
  }

  const tunnelId = process.env.STATE_tunnelId
  const serverUrl = input('server-url', '').replace(/\/+$/, '')
  const serverToken = input('server-token', '')
  if (!tunnelId || !serverUrl || !serverToken) return

  try {
    const res = await fetch(`${serverUrl}/aperio/api/tunnels/${encodeURIComponent(tunnelId)}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${serverToken}` },
    })
    if (res.ok) {
      console.log(`Tunnel ${tunnelId} revoked`)
    } else {
      console.warn(`Tunnel revocation returned HTTP ${res.status} (token expires by TTL anyway)`)
    }
  } catch (e) {
    console.warn(`Tunnel revocation failed: ${e.message} (token expires by TTL anyway)`)
  }
}

main()
