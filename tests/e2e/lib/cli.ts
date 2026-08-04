import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { CLIENT_BIN } from './env.js'

const run = promisify(execFile)

export interface CliResult {
  ok: boolean
  stdout: string
  stderr: string
}

/**
 * Runs `aperio-client api …` against a server with an admin key.
 *
 * The bash phase defines a one-line `api()` shell function for this; the
 * difference is that a failure here is a value, so a test can assert that a
 * command *fails* without `if … then fail` around it.
 */
export async function api(
  serverUrl: string,
  apiKey: string,
  args: string[],
): Promise<CliResult> {
  try {
    const { stdout, stderr } = await run(CLIENT_BIN, ['api', ...args], {
      env: { ...process.env, APERIO_SERVER_URL: serverUrl, APERIO_API_KEY: apiKey },
      maxBuffer: 16 * 1024 * 1024,
    })
    return { ok: true, stdout, stderr }
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string }
    return { ok: false, stdout: err.stdout ?? '', stderr: err.stderr ?? '' }
  }
}

/** The same, without a credential, for the calls that must be refused. */
export async function apiWithoutKey(serverUrl: string, args: string[]): Promise<CliResult> {
  try {
    const { stdout, stderr } = await run(CLIENT_BIN, ['api', ...args], {
      env: { ...process.env, APERIO_SERVER_URL: serverUrl, APERIO_API_KEY: '' },
    })
    return { ok: true, stdout, stderr }
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string }
    return { ok: false, stdout: err.stdout ?? '', stderr: err.stderr ?? '' }
  }
}

/** Runs a bare client subcommand (`check`, …) and returns what it printed. */
export async function client(
  args: string[],
  env: Record<string, string> = {},
): Promise<CliResult> {
  try {
    const { stdout, stderr } = await run(CLIENT_BIN, args, {
      env: { ...process.env, ...env },
      maxBuffer: 16 * 1024 * 1024,
    })
    return { ok: true, stdout, stderr }
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string }
    return { ok: false, stdout: err.stdout ?? '', stderr: err.stderr ?? '' }
  }
}
