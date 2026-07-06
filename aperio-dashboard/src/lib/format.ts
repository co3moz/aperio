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

/** Splits a comma separated input field into trimmed, non-empty items. */
export function splitList(raw: string): string[] {
  return raw
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
}
