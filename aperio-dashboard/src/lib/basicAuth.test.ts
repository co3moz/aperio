import { describe, expect, it } from 'vitest'
import { basicAuthorization } from './basicAuth'

/** What the server does with the header: base64, then UTF-8. */
function decoded(header: string): string {
  const b64 = header.replace(/^Basic /, '')
  const binary = atob(b64)
  return new TextDecoder().decode(Uint8Array.from(binary, (ch) => ch.charCodeAt(0)))
}

describe('basicAuthorization', () => {
  it('is the plain Basic header for ASCII credentials', () => {
    expect(basicAuthorization('ada', 'pw')).toBe(`Basic ${btoa('ada:pw')}`)
  })

  it('carries a password outside Latin-1 as UTF-8 rather than throwing', () => {
    const password = 'şifre€🔑'
    expect(() => btoa(`ada:${password}`)).toThrow()
    const header = basicAuthorization('ada', password)
    expect(decoded(header)).toBe(`ada:${password}`)
  })

  it('keeps a colon in the password on the password side', () => {
    expect(decoded(basicAuthorization('ada', 'a:b'))).toBe('ada:a:b')
  })
})
