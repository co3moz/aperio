export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1)
  return `${parseFloat((bytes / Math.pow(k, i)).toFixed(2))} ${sizes[i]}`
}

/** Formats a bytes/second capacity as a bit-rate (e.g. 1000000 → "8 Mbit/s"). */
export function formatBandwidth(bytesPerSec: number): string {
  const bits = bytesPerSec * 8
  if (bits >= 1e9) return `${parseFloat((bits / 1e9).toFixed(1))} Gbit/s`
  if (bits >= 1e6) return `${parseFloat((bits / 1e6).toFixed(1))} Mbit/s`
  if (bits >= 1e3) return `${parseFloat((bits / 1e3).toFixed(1))} kbit/s`
  return `${bits} bit/s`
}

export function formatUptime(seconds: number): string {
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const s = Math.floor(seconds % 60)
  const parts: string[] = []
  if (h > 0) parts.push(`${h}h`)
  if (m > 0) parts.push(`${m}m`)
  parts.push(`${s}s`)
  return parts.join(' ')
}

/**
 * Renders an absolute time as a compact "12s ago" style label. Accepts either
 * unix seconds (audit `ts`) or the server's local `%Y-%m-%d %H:%M:%S` string;
 * returns the raw input unchanged when it cannot be parsed.
 */
export function formatRelativeTime(input: string | number): string {
  const ms = typeof input === 'number' ? input * 1000 : Date.parse(input.replace(' ', 'T'))
  if (Number.isNaN(ms)) return String(input)
  const diff = Date.now() - ms
  if (diff < 1000) return 'just now'
  const s = Math.floor(diff / 1000)
  if (s < 60) return `${s}s ago`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ago`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ago`
  const d = Math.floor(h / 24)
  return `${d}d ago`
}

export function formatLastPing(secondsAgo: number | null): string {
  if (secondsAgo === null || secondsAgo === undefined) return '—'
  if (secondsAgo < 1) return 'now'
  return `${secondsAgo}s ago`
}

export function formatExpiry(expiresAt: number | null, expired: boolean): string {
  if (!expiresAt) return 'never'
  const date = new Date(expiresAt * 1000).toLocaleString()
  return expired ? `expired ${date}` : date
}

/** Decodes a captured base64 body into a printable preview. */
export function decodeBodyPreview(
  b64: string | null,
  truncated: boolean,
  streamed: boolean,
): string {
  if (streamed) return '(streamed body — not captured)'
  if (!b64) return '(empty)'
  try {
    const bin = atob(b64)
    if (bin.length === 0) return '(empty)'
    // Show as text when mostly printable; otherwise note binary.
    let printable = 0
    for (let i = 0; i < bin.length; i++) {
      const c = bin.charCodeAt(i)
      if (c === 9 || c === 10 || c === 13 || (c >= 32 && c < 127) || c > 127) printable++
    }
    if (printable / bin.length > 0.9) {
      const bytes = Uint8Array.from(bin, (ch) => ch.charCodeAt(0))
      const suffix = truncated ? '\n… (truncated at 64 KB)' : ''
      return new TextDecoder().decode(bytes) + suffix
    }
    return `(binary body, ${bin.length} bytes${truncated ? ', truncated' : ''})`
  } catch {
    return '(unable to decode body)'
  }
}

export function formatHeaders(headers: [string, string][]): string {
  return headers.map(([k, v]) => `${k}: ${v}`).join('\n') || '(none)'
}

/** Single-quotes a string for safe inclusion in a POSIX shell command. */
function shellQuote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`
}

/**
 * Reconstructs an equivalent `curl` command for a captured request. The URL is
 * rebuilt from the request's Host header (falling back to the dashboard origin)
 * and path; a decodable, non-truncated body is included with `--data-binary`.
 */
export function buildCurl(
  method: string,
  uri: string,
  headers: [string, string][],
  body: string | null,
  bodyTruncated: boolean,
): string {
  const host = headers.find(([k]) => k.toLowerCase() === 'host')?.[1] ?? window.location.host
  const scheme = window.location.protocol.replace(':', '') || 'https'
  const url = `${scheme}://${host}${uri}`

  const parts = [`curl -X ${method} ${shellQuote(url)}`]
  for (const [k, v] of headers) {
    // Host is expressed by the URL; hop-by-hop/length headers are set by curl.
    const lower = k.toLowerCase()
    if (lower === 'host' || lower === 'content-length') continue
    parts.push(`-H ${shellQuote(`${k}: ${v}`)}`)
  }
  if (body && !bodyTruncated) {
    try {
      const decoded = new TextDecoder().decode(Uint8Array.from(atob(body), (c) => c.charCodeAt(0)))
      if (decoded) parts.push(`--data-binary ${shellQuote(decoded)}`)
    } catch {
      // Binary or undecodable body: omit it rather than emit garbage.
    }
  }
  return parts.join(' \\\n  ')
}

/** Splits a comma separated input field into trimmed, non-empty items. */
export function splitList(raw: string): string[] {
  return raw
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
}
