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
  it('offers a name on a declared tunnel', () => {
    const tunnels = find(clientFields, 'tunnels')
    expect(tunnels?.kind).toBe('objectList')
    const keys = tunnels?.children?.map((c) => c.key)
    expect(keys).toContain('name')
    expect(keys).toContain('target')
    expect(keys).toContain('protocol')
  })

  it('keeps bind-tunnels editable now that an entry may also be a bare port', () => {
    // The value schema became `anyOf [integer, BindTunnelEntry]`. If the
    // flattener took the integer branch, or gave up, the section would render
    // as an uneditable row instead of a dialog.
    const bind = find(clientFields, 'bind-tunnels')
    expect(bind?.kind).toBe('objectMap')
    const keys = bind?.children?.map((c) => c.key)
    expect(keys).toContain('port')
    expect(keys).toContain('address')
    expect(keys).toContain('token')
  })

  it('describes the combined transport as an option', () => {
    const protocol = find(clientFields, 'tunnels')?.children?.find(
      (c) => c.key === 'protocol',
    )
    expect(protocol?.example).toContain('tcp/udp')
  })
})

describe('the server schema through the builder', () => {
  it('offers the identity form of an expose entry', () => {
    const expose = find(serverFields, 'expose')
    expect(expose?.kind).toBe('objectList')
    const keys = expose?.children?.map((c) => c.key)
    expect(keys).toEqual(expect.arrayContaining(['port', 'tunnel', 'token', 'key']))
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
    // All five are `map of name → scalar`, which has no editor yet; the map
    // of name → *object* does, which is how `bind-tunnels` itself is edited.
    expect(unsupported(clientFields).sort()).toEqual([
      'bind-tunnels.override',
      'headers.request.add',
      'headers.response.add',
      'services.headers.request',
      'services.headers.response',
    ])
  })

  it('has only the same two holes in the server schema', () => {
    expect(unsupported(serverFields).sort()).toEqual([
      'headers.request.add',
      'headers.response.add',
    ])
  })
})
