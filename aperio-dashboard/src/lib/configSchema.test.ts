import { describe, expect, it } from 'vitest'
import {
  fieldsOf,
  getAt,
  setAt,
  splitBytes,
  toBytes,
  type JsonSchema,
} from './configSchema'

/** A schema shaped like the ones schemars emits: refs, nullable anyOf, and
 *  an untagged enum for a block that also accepts a bare scalar. */
const schema: JsonSchema = {
  type: 'object',
  properties: {
    target: {
      anyOf: [{ type: 'string' }, { type: 'null' }],
      description: 'Backend URL',
      examples: ['http://localhost:3000'],
    },
    max_body_size: { anyOf: [{ type: 'integer' }, { type: 'null' }] },
    ip_family: { type: 'string', enum: ['auto', 'ipv4', 'ipv6'] },
    idle_timeout: { type: 'string' },
    allowed_ips: { type: 'array', items: { type: 'string' } },
    health: { anyOf: [{ $ref: '#/$defs/HealthGroup' }, { type: 'null' }] },
    services: { type: 'array', items: { $ref: '#/$defs/ServiceEntry' } },
    cache: { anyOf: [{ type: 'boolean' }, { $ref: '#/$defs/CacheGroup' }] },
  },
  $defs: {
    HealthGroup: {
      type: 'object',
      properties: {
        endpoint: { type: 'string' },
        interval: { type: 'integer' },
      },
    },
    ServiceEntry: {
      type: 'object',
      required: ['target'],
      properties: {
        name: { type: 'string' },
        target: { type: 'string' },
      },
    },
    CacheGroup: {
      type: 'object',
      properties: { max_bytes: { type: 'integer' } },
    },
  },
}

const byKey = (key: string) =>
  fieldsOf(schema, schema).find((f) => f.key === key)

describe('fieldsOf', () => {
  it('unwraps the nullable anyOf schemars emits for an Option field', () => {
    const target = byKey('target')
    expect(target?.kind).toBe('string')
    expect(target?.description).toBe('Backend URL')
    expect(target?.example).toBe('http://localhost:3000')
  })

  it('recognises the editor each type needs', () => {
    expect(byKey('ip_family')?.kind).toBe('select')
    expect(byKey('ip_family')?.options).toEqual(['auto', 'ipv4', 'ipv6'])
    // A byte size gets the unit editor rather than a raw number box.
    expect(byKey('max_body_size')?.kind).toBe('bytes')
    // A duration is a string the parser reads as `5m`, not a number.
    expect(byKey('idle_timeout')?.kind).toBe('duration')
    expect(byKey('allowed_ips')?.kind).toBe('stringList')
  })

  it('follows $ref into nested blocks and paths their children', () => {
    const health = byKey('health')
    expect(health?.kind).toBe('object')
    expect(health?.children?.map((c) => c.path)).toEqual([
      'health.endpoint',
      'health.interval',
    ])
  })

  it('describes a list of objects by the shape of one entry', () => {
    const services = byKey('services')
    expect(services?.kind).toBe('objectList')
    expect(services?.children?.map((c) => c.key)).toEqual(['name', 'target'])
    expect(services?.required).toEqual(['target'])
  })

  it('picks the object branch of an untagged scalar-or-block enum', () => {
    // `cache: true` and `cache: {max_bytes: …}` are both valid; the form
    // offers the block, which the server accepts identically.
    const cache = byKey('cache')
    expect(cache?.kind).toBe('object')
    expect(cache?.children?.[0]?.path).toBe('cache.max_bytes')
  })
})

describe('setAt / getAt', () => {
  it('writes through a dotted path, creating the intermediate objects', () => {
    const doc = setAt({}, 'health.interval', 7)
    expect(doc).toEqual({ health: { interval: 7 } })
    expect(getAt(doc, 'health.interval')).toBe(7)
    expect(getAt(doc, 'health.missing')).toBeUndefined()
    expect(getAt(doc, 'nothing.at.all')).toBeUndefined()
  })

  it('does not mutate the document it was given', () => {
    const before = { health: { interval: 7 } }
    const after = setAt(before, 'health.endpoint', '/healthz')
    expect(before).toEqual({ health: { interval: 7 } })
    expect(after).toEqual({ health: { interval: 7, endpoint: '/healthz' } })
  })

  it('prunes the block when clearing its last field', () => {
    // Otherwise an emptied field would leave `health: {}` in the export, which
    // reads as a deliberate empty block rather than an absent one.
    let doc: Record<string, unknown> = setAt({}, 'health.interval', 7)
    doc = setAt(doc, 'health.interval', '')
    expect(doc).toEqual({})
  })

  it('keeps the block when other fields remain', () => {
    let doc: Record<string, unknown> = setAt({}, 'health.interval', 7)
    doc = setAt(doc, 'health.endpoint', '/healthz')
    doc = setAt(doc, 'health.interval', undefined)
    expect(doc).toEqual({ health: { endpoint: '/healthz' } })
  })
})

describe('byte units', () => {
  it('shows a size in the largest unit that divides it exactly', () => {
    expect(splitBytes(10 * 1024 * 1024)).toEqual({ amount: 10, unit: 'MB' })
    expect(splitBytes(2 * 1024 * 1024 * 1024)).toEqual({ amount: 2, unit: 'GB' })
    expect(splitBytes(1536)).toEqual({ amount: 1536, unit: 'B' })
    expect(splitBytes(0)).toEqual({ amount: 0, unit: 'B' })
  })

  it('converts back, and refuses a value that is not a number', () => {
    expect(toBytes(10, 'MB')).toBe(10 * 1024 * 1024)
    expect(toBytes(1.5, 'KB')).toBe(1536)
    expect(toBytes(Number.NaN, 'MB')).toBeUndefined()
    expect(toBytes(1, 'furlongs')).toBeUndefined()
  })
})

describe('deprecated keys', () => {
  const withDeprecated: JsonSchema = {
    type: 'object',
    properties: {
      cache_max_bytes: {
        anyOf: [{ type: 'integer' }, { type: 'null' }],
        description: 'Deprecated spelling of `cache.max_bytes` (env: APERIO_CACHE_MAX_BYTES).',
      },
      target: { type: 'string', description: 'Backend URL' },
    },
  }

  it('marks a key the schema documents as a deprecated spelling', () => {
    const fields = fieldsOf(withDeprecated, withDeprecated)
    expect(fields.find((f) => f.key === 'cache_max_bytes')?.deprecated).toBe(true)
    expect(fields.find((f) => f.key === 'target')?.deprecated).toBe(false)
  })
})

describe('map-valued fields', () => {
  const withMap: JsonSchema = {
    type: 'object',
    properties: {
      'bind-tunnels': {
        type: ['object', 'null'],
        additionalProperties: { $ref: '#/$defs/BindTunnelEntry' },
        description: 'Peer clients whose tunnels this process binds.',
      },
      opaque: { type: 'object' },
    },
    $defs: {
      BindTunnelEntry: {
        type: 'object',
        properties: { token: { type: 'string' }, psk: { type: 'string' } },
      },
    },
  }

  it('describes a name → object map by the shape of one entry', () => {
    // Without this the field would be "unsupported" and bind-tunnels would be
    // the one thing the builder could not configure.
    const field = fieldsOf(withMap, withMap).find((f) => f.key === 'bind-tunnels')
    expect(field?.kind).toBe('objectMap')
    expect(field?.children?.map((c) => c.key)).toEqual(['token', 'psk'])
  })

  it('still gives up on an object with no described shape', () => {
    const field = fieldsOf(withMap, withMap).find((f) => f.key === 'opaque')
    expect(field?.kind).toBe('unsupported')
  })
})

describe('ordering and deprecation in practice', () => {
  it('detects a deprecated note that follows a sentence of its own', () => {
    // `target_health` documents what it does first and only then says it is a
    // deprecated spelling, so anchoring the match to the start missed it and
    // the retired key kept appearing in blank files.
    const s: JsonSchema = {
      type: 'object',
      properties: {
        target_health: {
          type: 'string',
          description:
            'Backend health endpoint to probe; a failing backend leaves rotation without dropping the tunnel. Deprecated spelling of `health.endpoint`.',
        },
      },
    }
    expect(fieldsOf(s, s)[0].deprecated).toBe(true)
  })
})
