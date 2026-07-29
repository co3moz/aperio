/**
 * The naming rule, mirrored from `aperio_config::validate_name` / `slug`.
 *
 * A handle is an identifier: `a-z`, `0-9` and `_`. Everything else is
 * reserved, which is what keeps `-`, `.`, `*` and `@` available as syntax
 * *around* a name (`payments@postgres`, and one day `acme.*@postgres`).
 * Anything a person should read goes in a display name instead.
 *
 * The dashboard checks the same rule the server enforces, so a form says what
 * is wrong before a round trip does, and says it the same way.
 */
export const NAME_PATTERN = /^[a-z0-9_]+$/
export const MAX_NAME_LEN = 64

/** Why this handle is not one, or null when it is. */
export function nameError(kind: string, raw: string): string | null {
  const name = raw.trim()
  if (!name) return `a ${kind} name cannot be empty`
  if (name.length > MAX_NAME_LEN) return `${kind} name is longer than ${MAX_NAME_LEN} characters`
  if (!NAME_PATTERN.test(name)) {
    return `${kind} name '${name}' may only contain a-z, 0-9 and _ (write it as '${slug(name)}')`
  }
  return null
}

/**
 * The ASCII letter a Latin one stands on. Only letters that are one letter
 * wearing a mark, plus `ı` and `ß`, which have one obvious reading; anything
 * else becomes a separator, since a suggestion should be recognizable rather
 * than a guess at a script it cannot read.
 */
const FOLD: Record<string, string> = {
  á: 'a', à: 'a', â: 'a', ä: 'a', ã: 'a', å: 'a', ā: 'a',
  ç: 'c', ć: 'c', č: 'c',
  é: 'e', è: 'e', ê: 'e', ë: 'e', ē: 'e',
  ğ: 'g', ĝ: 'g',
  í: 'i', ì: 'i', î: 'i', ï: 'i', ī: 'i', ı: 'i',
  ñ: 'n', ń: 'n',
  ó: 'o', ò: 'o', ô: 'o', ö: 'o', õ: 'o', ø: 'o', ō: 'o',
  ś: 's', š: 's', ş: 's',
  ú: 'u', ù: 'u', û: 'u', ü: 'u', ū: 'u',
  ý: 'y', ÿ: 'y',
  ź: 'z', ż: 'z', ž: 'z',
  ß: 'ss', æ: 'ae', œ: 'oe',
}

/**
 * Turns anything into a valid handle. Used to propose one from a display name,
 * never to silently replace what someone typed.
 */
export function slug(raw: string): string {
  let out = ''
  for (const ch of raw.trim()) {
    const lower = ch.toLowerCase()
    if (/[a-z0-9]/.test(lower)) out += lower
    else if (FOLD[lower]) out += FOLD[lower]
    else if (!out.endsWith('_')) out += '_'
  }
  out = out.replace(/^_+|_+$/g, '')
  return (out || 'unnamed').slice(0, MAX_NAME_LEN)
}
