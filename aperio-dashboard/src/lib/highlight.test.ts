import { describe, expect, it } from 'vitest'
import { MAX_HIGHLIGHT_CHARS, detectLanguage, highlight, prettyJson } from './highlight'

/** The text a set of tokens renders as. Highlighting that changes the body is
 *  worse than no highlighting, so every test checks this. */
const rendered = (body: string) =>
  highlight(body)
    .tokens.map((t) => t.text)
    .join('')

describe('detectLanguage', () => {
  it('reads the body rather than trusting a shape', () => {
    expect(detectLanguage('{"a": 1}')).toBe('json')
    expect(detectLanguage('[1, 2]')).toBe('json')
    // Looks like JSON and is not: a truncated capture is the common case, and
    // tokenizing it as JSON would mislabel the remains.
    expect(detectLanguage('{"a": 1, "b"')).toBe('text')
    expect(detectLanguage('<html><body>hi</body></html>')).toBe('markup')
    expect(detectLanguage('plain words')).toBe('text')
  })
})

describe('highlight', () => {
  it('never changes the text it is given', () => {
    for (const body of [
      '{"name": "ada", "n": 42, "ok": true, "gone": null}',
      '<a href="https://example.com" disabled>link</a>',
      '<!-- a comment --><p>text</p>',
      'not markup or json at all',
      '{"nested": {"deep": [1, -2.5, 1e3]}}',
    ]) {
      const expected = detectLanguage(body) === 'json' ? prettyJson(body) : body
      expect(rendered(body)).toBe(expected)
    }
  })

  it('tells a JSON key from a JSON string value', () => {
    const { tokens } = highlight('{"name": "ada"}')
    const key = tokens.find((t) => t.text === '"name"')
    const value = tokens.find((t) => t.text === '"ada"')
    expect(key?.kind).toBe('key')
    expect(value?.kind).toBe('string')
  })

  it('does not read a colon inside a string as a key marker', () => {
    // The value here ends with a colon; a naive "string followed by colon"
    // rule would colour the *value* as a key.
    const { tokens } = highlight('{"a": "10:30"}')
    expect(tokens.find((t) => t.text === '"10:30"')?.kind).toBe('string')
  })

  it('handles an escaped quote inside a string', () => {
    const body = '{"say": "he said \\"hi\\""}'
    expect(rendered(body)).toBe(prettyJson(body))
  })

  it('marks tags, attributes and attribute values', () => {
    const { tokens } = highlight('<a href="https://example.com">x</a>')
    expect(tokens.find((t) => t.text === '<a')?.kind).toBe('tag')
    expect(tokens.find((t) => t.text === 'href')?.kind).toBe('attr')
    expect(tokens.find((t) => t.text === '"https://example.com"')?.kind).toBe('string')
    // Text between tags is left alone: it is usually the part being read.
    expect(tokens.find((t) => t.text === 'x')?.kind).toBeUndefined()
  })

  it('leaves an unterminated tag intact', () => {
    // A truncated capture ends mid-tag more often than not.
    expect(rendered('<div class="a"')).toBe('<div class="a"')
  })

  it('gives up on a body large enough that tokenizing would be felt', () => {
    const big = `{"a": "${'x'.repeat(MAX_HIGHLIGHT_CHARS)}"}`
    const result = highlight(big)
    expect(result.language).toBe('text')
    expect(result.tokens).toHaveLength(1)
  })
})

describe('prettyJson', () => {
  it('re-indents a minified body and leaves an unparseable one alone', () => {
    expect(prettyJson('{"a":1}')).toBe('{\n  "a": 1\n}')
    expect(prettyJson('{"a":')).toBe('{"a":')
  })
})
