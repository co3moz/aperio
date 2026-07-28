// Refreshes src/lib/__schemas.json, the fixture configSchema.live.test.ts
// runs the config builder against.
//
// The builder is driven by the JSON Schemas aperio-config emits, so the only
// way to know a new setting actually reaches the form is to feed it the real
// thing. That means a snapshot: this script regenerates it. Run it after
// changing aperio-config, or the test keeps answering for the old schema.
import { execFileSync } from 'node:child_process'
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const dump = (...args) =>
  JSON.parse(
    execFileSync('cargo', ['run', '-q', '-p', 'aperio-config', '--bin', 'aperio-config', ...args], {
      cwd: `${root}..`,
      encoding: 'utf8',
      maxBuffer: 32 * 1024 * 1024,
    }),
  )

const schemas = { client: dump(), server: dump('--', '--server') }
writeFileSync(`${root}src/lib/__schemas.json`, `${JSON.stringify(schemas, null, 2)}\n`)
console.log(
  `schemas written: ${Object.keys(schemas.client.properties).length} client keys, ` +
    `${Object.keys(schemas.server.properties).length} server keys`,
)
