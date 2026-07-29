/**
 * Reading the `aperio.yaml` / `aperio-server.yaml` JSON Schemas well enough to
 * build a form from them.
 *
 * The schemas come from the running server (`GET /aperio/api/config/schema/…`)
 * and are generated from the same Rust types that parse the files, so the
 * builder never drifts from what the binary accepts. What this module does is
 * flatten schemars' output — `$ref` indirection, the `anyOf` that a nullable
 * or untagged field produces — into a flat list of fields the UI can render
 * without knowing any of that.
 */

/** A JSON Schema document or subschema, as far as we look into one. */
export interface JsonSchema {
  type?: string | string[]
  description?: string
  properties?: Record<string, JsonSchema>
  items?: JsonSchema
  enum?: unknown[]
  examples?: unknown[]
  format?: string
  default?: unknown
  $ref?: string
  $defs?: Record<string, JsonSchema>
  anyOf?: JsonSchema[]
  oneOf?: JsonSchema[]
  required?: string[]
  additionalProperties?: JsonSchema | boolean
  /** Standard JSON Schema: this key still works but should not be written. */
  deprecated?: boolean
}

/** How a field is edited. */
export type FieldKind =
  | 'string'
  | 'number'
  | 'boolean'
  | 'bytes'
  | 'duration'
  | 'select'
  | 'stringList'
  | 'objectList'
  /** A map of name → object, e.g. `bind-tunnels:`. Edited in a dialog. */
  | 'objectMap'
  | 'object'
  | 'unsupported'

export interface Field {
  /** Key as written in the yaml. */
  key: string
  /** Dotted path from the document root, e.g. `health.interval`. */
  path: string
  kind: FieldKind
  description?: string
  /** Options for `select`. */
  options?: string[]
  /** Placeholder drawn from the schema's `examples`. */
  example?: string
  /** Nested fields for `object`, or the shape of one entry for `objectList`. */
  children?: Field[]
  /** Keys an entry of an `objectList` must carry. */
  required?: string[]
  /** True for a key on its way out — a superseded spelling, or one being
   *  withdrawn from the file format. Hidden unless an imported file actually
   *  uses it: offering it in a blank form would invite writing the very key
   *  we want people to stop writing. */
  deprecated?: boolean
}

/** Resolves `$ref` against the document's `$defs`, once. */
function deref(node: JsonSchema, root: JsonSchema): JsonSchema {
  if (!node.$ref) return node
  const name = node.$ref.split('/').pop() ?? ''
  return root.$defs?.[name] ?? node
}

/**
 * Collapses the `anyOf`/`oneOf` schemars emits for a nullable field
 * (`T | null`) or an untagged enum (`bool | {…}`) into the branch worth
 * editing: the object branch when there is one, otherwise the first
 * non-null branch. Untagged enums lose their scalar shorthand this way
 * (`cache: true` becomes `cache: {enabled: true}`), which is the spelling the
 * docs recommend anyway and which the server accepts identically.
 */
function collapse(node: JsonSchema, root: JsonSchema): JsonSchema {
  const resolved = deref(node, root)
  const branches = resolved.anyOf ?? resolved.oneOf
  if (!branches?.length) return resolved
  const real = branches
    .map((b) => deref(b, root))
    .filter((b) => b.type !== 'null')
  const object = real.find((b) => b.properties || b.$ref)
  return collapseIfNeeded(object ?? real[0] ?? resolved, root)
}

/** `collapse` again for a branch that is itself a `$ref` to a union. */
function collapseIfNeeded(node: JsonSchema, root: JsonSchema): JsonSchema {
  const resolved = deref(node, root)
  if (resolved.anyOf || resolved.oneOf) return collapse(resolved, root)
  return resolved
}

/** Keys whose value is a size in bytes, so the form can offer KB/MB units. */
const BYTE_KEYS =
  /(^|_)(max_body_size|max_request_body|max_response_body|max_message_size|max_bytes|backlog_limit|pause_bytes|resume_bytes|db_max_bytes|audit_max_size)$/

/** Keys holding a duration written the way the parser accepts (`5m`, `45s`). */
const DURATION_KEYS = /(^|_)(idle_timeout|cold_start|window|cooldown)$/

function scalarKind(key: string, node: JsonSchema): FieldKind {
  if (node.enum?.length) return 'select'
  const type = Array.isArray(node.type) ? node.type[0] : node.type
  if (type === 'boolean') return 'boolean'
  if (type === 'integer' || type === 'number') {
    return BYTE_KEYS.test(key) ? 'bytes' : 'number'
  }
  if (type === 'string') {
    // Only the string-valued durations; a numeric `window` stays a number.
    return DURATION_KEYS.test(key) ? 'duration' : 'string'
  }
  return 'string'
}

/** Builds the field list of one object schema. */
export function fieldsOf(
  node: JsonSchema,
  root: JsonSchema,
  prefix = '',
  depth = 0,
): Field[] {
  const props = collapse(node, root).properties
  if (!props) return []
  const out: Field[] = []
  for (const [key, raw] of Object.entries(props)) {
    const schema = collapse(raw, root)
    const path = prefix ? `${prefix}.${key}` : key
    // A doc comment and `examples` sit on the property itself, not on the
    // branch inside the nullable `anyOf` that wraps an Option field, so the
    // outer node is consulted first and the branch only fills in for a plain
    // (non-nullable) `$ref`.
    const outer = deref(raw, root)
    const description = outer.description ?? schema.description
    const examples = outer.examples ?? schema.examples
    // Every example, not just the first. A key whose examples enumerate the
    // accepted values (`tcp`, `udp`, `tcp/udp`) is documenting a choice, and
    // showing one of three hides the other two from the only place an
    // operator would look for them.
    const example = examples?.length
      ? examples.map((e) => String(e)).join(', ')
      : undefined
    const type = Array.isArray(schema.type) ? schema.type[0] : schema.type
    // Two ways a key can be on its way out. JSON Schema's own `deprecated`
    // keyword is the authoritative one and is what the Rust types emit for a
    // key being withdrawn outright; the phrase match stays for the superseded
    // spellings, documented as "Deprecated spelling of `x`" and never flagged.
    // Read from the property itself as well as the collapsed branch, for the
    // same reason the description is: on a key whose type is an `anyOf` (a
    // hostname is a string or a list of them), collapsing walks into one
    // branch and the flag stays behind on the outer node.
    const deprecated =
      outer.deprecated === true ||
      schema.deprecated === true ||
      /deprecated spelling of/i.test(description ?? '')

    if (type === 'array') {
      const item = collapse(schema.items ?? {}, root)
      const itemType = Array.isArray(item.type) ? item.type[0] : item.type
      if (item.properties) {
        out.push({
          key,
          path,
          kind: 'objectList',
          description,
          children: fieldsOf(item, root, '', depth + 1),
          required: item.required,
          deprecated,
        })
      } else if (itemType === 'object') {
        // A map-valued list we cannot describe; the raw yaml still round-trips.
        out.push({ key, path, kind: 'unsupported', description, deprecated })
      } else {
        out.push({ key, path, kind: 'stringList', description, example, deprecated })
      }
      continue
    }

    if (schema.properties) {
      // Nested blocks are worth one level of inlining; deeper ones (a map of
      // objects, say) are left to the yaml view rather than guessed at.
      if (depth >= 2) {
        out.push({ key, path, kind: 'unsupported', description, deprecated })
        continue
      }
      out.push({
        key,
        path,
        kind: 'object',
        description,
        children: fieldsOf(schema, root, path, depth + 1),
        deprecated,
      })
      continue
    }

    if (type === 'object') {
      // A map whose values are objects (`bind-tunnels:`): schemars describes
      // the value shape under additionalProperties, which is enough to edit
      // each entry in a dialog rather than declaring it off-limits.
      const values = collapse(
        (schema as { additionalProperties?: JsonSchema }).additionalProperties ?? {},
        root,
      )
      if (values.properties) {
        out.push({
          key,
          path,
          kind: 'objectMap',
          description,
          children: fieldsOf(values, root, '', depth + 1),
          deprecated,
        })
        continue
      }
      out.push({ key, path, kind: 'unsupported', description, deprecated })
      continue
    }

    out.push({
      key,
      path,
      kind: scalarKind(key, schema),
      description,
      example,
      options: schema.enum?.map(String),
      deprecated,
    })
  }
  return out
}

/** Reads a dotted path out of a plain object tree. */
export function getAt(doc: unknown, path: string): unknown {
  let node: unknown = doc
  for (const part of path.split('.')) {
    if (node === null || typeof node !== 'object') return undefined
    node = (node as Record<string, unknown>)[part]
  }
  return node
}

/**
 * Writes a dotted path into a plain object tree, creating the intermediate
 * objects, and prunes back up when the value is cleared so an emptied field
 * leaves no `health: {}` behind in the exported document.
 */
export function setAt<T extends Record<string, unknown>>(
  doc: T,
  path: string,
  value: unknown,
): T {
  const parts = path.split('.')
  const next = { ...doc } as Record<string, unknown>
  let node = next
  const chain: Record<string, unknown>[] = [node]
  for (const part of parts.slice(0, -1)) {
    const child = node[part]
    const copy =
      child !== null && typeof child === 'object' && !Array.isArray(child)
        ? { ...(child as Record<string, unknown>) }
        : {}
    node[part] = copy
    node = copy
    chain.push(copy)
  }
  const leaf = parts[parts.length - 1]
  if (value === undefined || value === '' || value === null) {
    delete node[leaf]
  } else {
    node[leaf] = value
  }
  // Drop objects the deletion emptied, innermost first.
  for (let i = chain.length - 1; i > 0; i--) {
    if (Object.keys(chain[i]).length === 0) delete chain[i - 1][parts[i - 1]]
  }
  return next as T
}

/** Byte units offered next to a size field, largest first. */
export const BYTE_UNITS: { label: string; factor: number }[] = [
  { label: 'GB', factor: 1024 * 1024 * 1024 },
  { label: 'MB', factor: 1024 * 1024 },
  { label: 'KB', factor: 1024 },
  { label: 'B', factor: 1 },
]

/** Splits a byte count into the largest unit that divides it exactly. */
export function splitBytes(value: number): { amount: number; unit: string } {
  for (const { label, factor } of BYTE_UNITS) {
    if (value >= factor && value % factor === 0) {
      return { amount: value / factor, unit: label }
    }
  }
  return { amount: value, unit: 'B' }
}

/** Multiplies an amount by its unit; NaN input yields undefined. */
export function toBytes(amount: number, unit: string): number | undefined {
  if (!Number.isFinite(amount)) return undefined
  const found = BYTE_UNITS.find((u) => u.label === unit)
  return found ? Math.round(amount * found.factor) : undefined
}
