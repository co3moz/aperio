// Browser glue for the server's webauthn-rs endpoints: converts between the
// base64url strings the server speaks and the ArrayBuffers the WebAuthn
// browser API wants, for both registration and sign-in ceremonies.

function b64urlToBuf(s: string): ArrayBuffer {
  const pad = '='.repeat((4 - (s.length % 4)) % 4)
  const raw = atob(s.replace(/-/g, '+').replace(/_/g, '/') + pad)
  const buf = new Uint8Array(raw.length)
  for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i)
  return buf.buffer
}

function bufToB64url(b: ArrayBuffer): string {
  const bytes = new Uint8Array(b)
  let raw = ''
  for (const byte of bytes) raw += String.fromCharCode(byte)
  return btoa(raw).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

/** True when this page can run WebAuthn ceremonies at all. */
export function browserSupportsPasskeys(): boolean {
  return typeof navigator !== 'undefined' && 'credentials' in navigator && 'PublicKeyCredential' in window
}

/** True when the server has passkey sign-in configured. */
export async function serverSupportsPasskeys(): Promise<boolean> {
  try {
    const res = await fetch('/aperio/auth/passkey')
    if (!res.ok) return false
    const body = (await res.json()) as { available?: boolean }
    return body.available === true
  } catch {
    return false
  }
}

interface CreationChallenge {
  ceremony_id: string
  challenge: {
    publicKey: {
      challenge: string
      user: { id: string; name: string; displayName: string }
      excludeCredentials?: { id: string; type: string }[]
      [key: string]: unknown
    }
  }
}

/** Runs the registration ceremony against an already-fetched challenge.
 *  `usernameless` asks the authenticator for a discoverable (resident)
 *  credential, required for signing in without typing a username. */
export async function createPasskeyCredential(
  start: CreationChallenge,
  usernameless = false,
): Promise<unknown> {
  const pk = start.challenge.publicKey
  const options: CredentialCreationOptions = {
    publicKey: {
      ...(pk as unknown as PublicKeyCredentialCreationOptions),
      challenge: b64urlToBuf(pk.challenge),
      user: {
        ...(pk.user as unknown as PublicKeyCredentialUserEntity),
        id: b64urlToBuf(pk.user.id),
      },
      excludeCredentials: (pk.excludeCredentials ?? []).map((c) => ({
        type: 'public-key' as const,
        id: b64urlToBuf(c.id),
      })),
      ...(usernameless
        ? {
            authenticatorSelection: {
              ...((pk.authenticatorSelection as Record<string, unknown> | undefined) ?? {}),
              residentKey: 'required' as const,
              requireResidentKey: true,
            },
          }
        : {}),
    },
  }
  const cred = (await navigator.credentials.create(options)) as PublicKeyCredential | null
  if (!cred) throw new Error('registration was cancelled')
  const response = cred.response as AuthenticatorAttestationResponse
  return {
    id: cred.id,
    rawId: bufToB64url(cred.rawId),
    type: cred.type,
    extensions: {},
    response: {
      attestationObject: bufToB64url(response.attestationObject),
      clientDataJSON: bufToB64url(response.clientDataJSON),
    },
  }
}

interface RequestChallenge {
  ceremony_id: string
  challenge: {
    publicKey: {
      challenge: string
      allowCredentials?: { id: string; type: string }[]
      [key: string]: unknown
    }
  }
}

/** Runs the sign-in ceremony against an already-fetched challenge. */
export async function getPasskeyAssertion(start: RequestChallenge): Promise<unknown> {
  const pk = start.challenge.publicKey
  const options: CredentialRequestOptions = {
    publicKey: {
      ...(pk as unknown as PublicKeyCredentialRequestOptions),
      challenge: b64urlToBuf(pk.challenge),
      allowCredentials: (pk.allowCredentials ?? []).map((c) => ({
        type: 'public-key' as const,
        id: b64urlToBuf(c.id),
      })),
    },
  }
  const cred = (await navigator.credentials.get(options)) as PublicKeyCredential | null
  if (!cred) throw new Error('sign-in was cancelled')
  const response = cred.response as AuthenticatorAssertionResponse
  return {
    id: cred.id,
    rawId: bufToB64url(cred.rawId),
    type: cred.type,
    extensions: {},
    response: {
      authenticatorData: bufToB64url(response.authenticatorData),
      clientDataJSON: bufToB64url(response.clientDataJSON),
      signature: bufToB64url(response.signature),
      userHandle: response.userHandle ? bufToB64url(response.userHandle) : null,
    },
  }
}

/** Full passkey sign-in flow from the login page. Throws on failure. */
export async function passkeySignIn(username: string): Promise<void> {
  const startRes = await fetch('/aperio/auth/passkey/start', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username }),
  })
  if (!startRes.ok) throw new Error(await startRes.text())
  const start = (await startRes.json()) as RequestChallenge
  const credential = await getPasskeyAssertion(start)
  const finishRes = await fetch('/aperio/auth/passkey/finish', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ ceremony_id: start.ceremony_id, credential }),
  })
  if (!finishRes.ok) throw new Error(await finishRes.text())
}

/** Usernameless sign-in: the authenticator's account picker chooses the
 *  identity. Only passkeys registered with the usernameless opt-in are
 *  accepted by the server. Throws on failure. */
export async function passkeySignInDiscoverable(): Promise<void> {
  const startRes = await fetch('/aperio/auth/passkey/discoverable/start', { method: 'POST' })
  if (!startRes.ok) throw new Error(await startRes.text())
  const start = (await startRes.json()) as RequestChallenge
  // The server hints at conditional mediation; we run a plain modal ceremony
  // (the user explicitly pressed the passkey button), so drop the hint.
  delete (start.challenge as Record<string, unknown>).mediation
  const credential = await getPasskeyAssertion(start)
  const finishRes = await fetch('/aperio/auth/passkey/discoverable/finish', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ ceremony_id: start.ceremony_id, credential }),
  })
  if (!finishRes.ok) throw new Error(await finishRes.text())
}
