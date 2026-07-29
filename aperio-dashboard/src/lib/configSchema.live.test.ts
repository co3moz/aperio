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

describe('the single-service keys on their way out', () => {
  // They only ever worked in a file with no `services:` list, and a file is
  // where a deployment is written down: two shapes for "what this client
  // exposes" is a question nobody should have to answer. The shorthand stays
  // on the CLI and in the environment, where a one-liner is the point.
  const SINGLE = ['target', 'serve', 'hostname', 'path', 'tcp_target', 'target_health']

  it('is marked deprecated in the schema the form reads', () => {
    for (const key of SINGLE) {
      expect(find(clientFields, key)?.deprecated, key).toBe(true)
    }
  })

  it('leaves the keys that are genuinely per-entry fallbacks alone', () => {
    // These stay top-level in the multi-service shape: the client falls back
    // to them per entry, so flagging them would tell people to remove a key
    // that still does its job.
    for (const key of ['trim_bind', 'pass_hostname', 'serve_spa', 'serve_404', 'services']) {
      expect(find(clientFields, key)?.deprecated, key).toBeFalsy()
    }
  })

  it('flags the block spelling of the probe path, but not its siblings', () => {
    // `health.interval` and friends are genuine per-entry defaults; only the
    // endpoint is read by nothing once a services: list exists.
    const health = find(clientFields, 'health')?.children ?? []
    expect(find(health, 'endpoint')?.deprecated).toBe(true)
    for (const key of ['interval', 'timeout', 'threshold', 'wait_for_backend']) {
      expect(find(health, key)?.deprecated, key).toBeFalsy()
    }
  })

  it('still names the same key on a services entry, undeprecated', () => {
    // The replacement has to be reachable, or the advice is empty.
    const entry = find(clientFields, 'services')?.children ?? []
    // `target_health` is excluded: on an entry it is still flagged, as the old
    // flat spelling of `health.endpoint`. That is a different complaint with a
    // different answer, and the answer, the block, is checked right below.
    for (const key of SINGLE.filter((k) => k !== 'target_health')) {
      expect(find(entry, key)?.deprecated, key).toBeFalsy()
    }
    const entryHealth = find(entry, 'health')?.children ?? []
    expect(find(entryHealth, 'endpoint')?.deprecated).toBeFalsy()
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
