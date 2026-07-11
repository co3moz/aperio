import { FingerprintIcon, PlusIcon, Trash2Icon } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Spinner } from '@/components/ui/spinner'
import { api, ApiError, type PasskeyInfo } from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'
import { useSession } from '@/lib/session'
import { useI18n } from '@/i18n'
import {
  browserSupportsPasskeys,
  createPasskeyCredential,
  serverSupportsPasskeys,
} from '@/lib/webauthn'

/** Self-service passkey management for the signed-in dashboard user. */
export function PasskeysDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useI18n()
  const { username } = useSession()
  const isBuiltIn = username === 'aperio'
  const [available, setAvailable] = useState<boolean | null>(null)
  const [passkeys, setPasskeys] = useState<PasskeyInfo[] | null>(null)
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const refresh = useCallback(async () => {
    try {
      setPasskeys(await api.passkeys())
    } catch {
      setPasskeys([])
    }
  }, [])

  useEffect(() => {
    if (!open) return
    setError(null)
    setName('')
    void serverSupportsPasskeys().then(setAvailable)
    if (!isBuiltIn) void refresh()
  }, [open, isBuiltIn, refresh])

  const register = async () => {
    setBusy(true)
    setError(null)
    try {
      const start = await api.passkeyRegisterStart()
      const credential = await createPasskeyCredential(start)
      await api.passkeyRegisterFinish({
        ceremony_id: start.ceremony_id,
        name: name.trim() || undefined,
        credential,
      })
      toast.success(t('Passkey registered'))
      setName('')
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const remove = async (p: PasskeyInfo) => {
    try {
      await api.passkeyDelete(p.id)
      toast.info(t('Passkey "{name}" deleted', { name: p.name }))
    } finally {
      void refresh()
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <FingerprintIcon className="size-4" /> {t('Passkeys')}
          </DialogTitle>
          <DialogDescription>
            {t('Sign in without a password using YubiKeys, Touch ID / Face ID, or your password manager.')}
          </DialogDescription>
        </DialogHeader>

        {isBuiltIn ? (
          <p className="text-sm text-muted-foreground">
            {t('The built-in "aperio" admin signs in with the master token or dashboard password and cannot register passkeys. Create a named user instead.')}
          </p>
        ) : available === false ? (
          <p className="text-sm text-muted-foreground">
            {t('Passkey sign-in is not configured on this server. Set APERIO_WEBAUTHN_ORIGIN to the dashboard’s public URL to enable it.')}
          </p>
        ) : !browserSupportsPasskeys() ? (
          <p className="text-sm text-muted-foreground">
            {t('This browser does not support WebAuthn.')}
          </p>
        ) : (
          <div className="grid gap-4">
            {passkeys === null ? (
              <Spinner />
            ) : passkeys.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('No passkeys registered yet.')}</p>
            ) : (
              <ul className="grid gap-2">
                {passkeys.map((p) => (
                  <li
                    key={p.id}
                    className="flex items-center justify-between rounded-lg border px-3 py-2"
                  >
                    <div className="flex flex-col">
                      <span className="text-sm font-medium">{p.name}</span>
                      <span className="text-xs text-muted-foreground">
                        {formatRelativeTime(p.created_at)}
                      </span>
                    </div>
                    <Button size="xs" variant="destructive" onClick={() => void remove(p)}>
                      <Trash2Icon /> {t('Delete')}
                    </Button>
                  </li>
                ))}
              </ul>
            )}
            <div className="grid gap-2">
              <Label htmlFor="pk-name">{t('Name for the new passkey (optional)')}</Label>
              <Input
                id="pk-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={t('e.g. YubiKey 5, MacBook Touch ID')}
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <DialogFooter>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                {t('Close')}
              </Button>
              <Button onClick={() => void register()} disabled={busy}>
                {busy ? <Spinner /> : <PlusIcon />} {t('Register passkey')}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
