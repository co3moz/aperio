/**
 * The catalogue against the server that fills it.
 *
 * The explain endpoint sends a message *code* and the dashboard looks it up
 * here. A code with no entry falls back to the server's English sentence, so
 * the failure is silent: the screen still reads, in the wrong language, and
 * only someone running the dashboard in Turkish would notice. These read the
 * codes out of the Rust source and demand an entry for each, so adding a
 * stage to the chain fails here rather than on someone's screen.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { EXPLAIN_INELIGIBLE, EXPLAIN_MESSAGES, EXPLAIN_SETTINGS } from './explainMessages'

const SOURCE = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  '..',
  'aperio-server',
  'src',
  'api',
  'explain.rs',
)
const rust = readFileSync(SOURCE, 'utf8')

/** Every `"a.b"` string literal in the handler, which is how a code is written. */
const codes = new Set(
  [...rust.matchAll(/"([a-z_]+\.[a-z_0-9]+)"/g)]
    .map((m) => m[1])
    // The `#[path = "..."]` attribute wiring in the test module.
    .filter((c) => !c.endsWith('.rs')),
)

describe('the explain message catalogue', () => {
  it('has an entry for every code the server can send', () => {
    const known = new Set([
      ...Object.keys(EXPLAIN_MESSAGES),
      ...Object.keys(EXPLAIN_INELIGIBLE),
      ...Object.keys(EXPLAIN_SETTINGS),
    ])
    expect([...codes].filter((c) => !known.has(c))).toEqual([])
  })

  it('has no entry the server never sends', () => {
    // The other direction: a code left behind by a stage that was removed is
    // a translation seven people maintain for a sentence nobody sees.
    const mine = [
      ...Object.keys(EXPLAIN_MESSAGES),
      ...Object.keys(EXPLAIN_INELIGIBLE),
      ...Object.keys(EXPLAIN_SETTINGS),
    ]
    expect(mine.filter((c) => !codes.has(c))).toEqual([])
  })

  it('names every placeholder the sentence interpolates', () => {
    // `{clients}` in the template and `clients` in `params` is the whole
    // contract; a typo renders the brace literally.
    for (const [code, template] of Object.entries(EXPLAIN_MESSAGES)) {
      for (const [, name] of template.matchAll(/\{(\w+)\}/g)) {
        expect(rust.includes(`"${name}"`), `${code} wants {${name}}`).toBe(true)
      }
    }
  })
})
