/**
 * The config builder against the *real* schemas, not a hand-written stand-in.
 *
 * The builder is schema-driven, so a new setting is supposed to appear in the
 * form for free. "Supposed to" is the problem: a shape the flattener cannot
 * read (a union, a map of unions) degrades to an `unsupported` row silently,
 * and nothing fails. These tests pin the settings that changed with named
 * tunnels to the kind of editor they must get.
 */
import { describe, expect, it } from 'vitest'
import schemas from './__schemas.json'
import { fieldsOf, type Field, type JsonSchema } from './configSchema'

const client = schemas.client as unknown as JsonSchema
const server = schemas.server as unknown as JsonSchema

const find = (fields: Field[], key: string) => fields.find((f) => f.key === key)
const clientFields = fieldsOf(client, client)
const serverFields = fieldsOf(server, server)

describe('the client schema through the builder', () => {
  it('edits services, tunnels and binds as maps keyed by name', () => {
    // A name is the key an entry is written under, so each of these is a map
    // of name → entry and is edited in a dialog, one entry per name.
    for (const key of ['services', 'tunnels']) {
      expect(find(clientFields, key)?.kind, key).toBe('objectMap')
    }
    const tunnel = find(clientFields, 'tunnels')?.children?.map((c) => c.key)
    expect(tunnel).toContain('target')
    expect(tunnel).toContain('protocol')
    expect(tunnel).not.toContain('name')
  })

  it('keeps binds editable now that an entry may also be a bare port', () => {
    // The value schema is `anyOf [integer, BindEntry]`. If the flattener took
    // the integer branch, or gave up, the section would render as an
    // uneditable row instead of a dialog.
    const bind = find(clientFields, 'binds')
    expect(bind?.kind).toBe('objectMap')
    const keys = bind?.children?.map((c) => c.key)
    expect(keys).toContain('port')
    expect(keys).toContain('address')
    expect(keys).toContain('token')
  })

  it('offers every service setting under defaults', () => {
    const defaults = find(clientFields, 'defaults')
    expect(defaults?.kind).toBe('object')
    const keys = defaults?.children?.map((c) => c.key) ?? []
    for (const key of ['access', 'timeout', 'health', 'limits', 'cache', 'inspector']) {
      expect(keys, key).toContain(key)
    }
    // Where a service's traffic comes from and goes to belongs to the one
    // service, never to a default.
    for (const key of ['target', 'hostname', 'path', 'depends_on']) {
      expect(keys, key).not.toContain(key)
    }
  })

  it('describes the combined transport as an option', () => {
    const protocol = find(clientFields, 'tunnels')?.children?.find(
      (c) => c.key === 'protocol',
    )
    expect(protocol?.example).toContain('tcp/udp')
  })
})

describe('the server settings through the builder', () => {
  it('reads every section as a block of its own', () => {
    // The settings document is sections all the way down; a section that
    // came out as a single field would put every one of its settings out of
    // the form's reach.
    for (const section of ['limits', 'gateway', 'routing', 'proxy', 'cache', 'oidc']) {
      expect(find(serverFields, section)?.kind, section).toBe('object')
    }
    expect(find(serverFields, 'sites')?.kind).toBe('objectList')
  })
})

describe('every setting reaches an editor', () => {
  // A settings builder that silently cannot edit a setting is worse than one
  // that never offered it: the file it exports looks complete. The form
  // degrades such a key to an uneditable row, which is honest but easy to add
  // by accident, so the ones that exist are listed here. A new entry in this
  // list is a new hole and has to be a deliberate decision, not a diff nobody
  // read.
  const unsupported = (fields: Field[], trail: string[] = []): string[] =>
    fields.flatMap((f) => [
      ...(f.kind === 'unsupported' ? [[...trail, f.key].join('.')] : []),
      ...unsupported(f.children ?? [], [...trail, f.key]),
    ])

  it('has only the known map-of-scalar holes', () => {
    // Every one is `map of name → scalar`, which has no editor yet; the map
    // of name → *object* does, which is how `services` itself is edited.
    // The list grows when a setting of that shape is added: `metrics_labels`
    // and `subscribe.env` joined it one cycle, `auth.claims` the next, and
    // both went unnoticed until the snapshot was regenerated. That was the
    // failure mode this test had, and it is closed: aperio-config now asserts
    // the snapshot is current, so a stale one fails in Rust rather than
    // hiding a new hole here.
    expect(unsupported(clientFields).sort()).toEqual([
      'defaults.access.claims',
      'defaults.headers.request',
      'defaults.headers.response',
      'defaults.metrics_labels',
      'services.access.claims',
      'services.headers.request',
      'services.headers.response',
      'services.metrics_labels',
      'subscribe.env',
    ])
  })

  it('has only the same holes in the server schema', () => {
    // `sites.headers.*` is the header shape reached through a site,
    // `sites.waf.header` is a block two lists deep, which the builder leaves
    // to the yaml view on purpose, `access.gate.claims` is the claim map a
    // `jwt` method requires, and `otel.headers` is a map of header names to
    // values.
    expect(unsupported(serverFields).sort()).toEqual([
      'access.gate.claims',
      'otel.headers',
      'sites.headers.request',
      'sites.headers.response',
      'sites.waf.header',
    ])
  })
})
