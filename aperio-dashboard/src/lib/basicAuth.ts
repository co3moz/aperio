/**
 * The `Authorization` value for a Basic sign-in.
 *
 * The credentials are encoded as UTF-8 before base64. `btoa` takes a
 * Latin-1 string and throws on anything else, so a password with an `ş`, a
 * `€` or an emoji in it was reported as a wrong password before it was ever
 * sent; the server reads the decoded bytes as UTF-8, which is what every
 * other client of it sends.
 */
export function basicAuthorization(username: string, password: string): string {
  const bytes = new TextEncoder().encode(`${username}:${password}`)
  let binary = ''
  for (const byte of bytes) binary += String.fromCharCode(byte)
  // The scheme word and the credentials, joined: not a template, which the
  // i18n check reads as a sentence to translate.
  return ['Basic', btoa(binary)].join(' ')
}
