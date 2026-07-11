import { CopyIcon, ShieldCheckIcon } from 'lucide-react'
import QRCode from 'qrcode'
import { useEffect, useState } from 'react'
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
import { api, ApiError } from '@/lib/api'
import { useSession } from '@/lib/session'
import { useI18n } from '@/i18n'

type Step = 'status' | 'enroll' | 'recovery'

/** Self-service TOTP management for the signed-in dashboard user. */
export function TotpDialog({
  open,
  onOpenChange,
  enabled,
  onChanged,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  enabled: boolean
  onChanged: () => void
}) {
  const { t } = useI18n()
  const { username } = useSession()
  const isBuiltIn = username === 'aperio'
  const [step, setStep] = useState<Step>('status')
  const [secret, setSecret] = useState('')
  const [qr, setQr] = useState<string | null>(null)
  const [code, setCode] = useState('')
  const [recoveryCodes, setRecoveryCodes] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (open) {
      setStep('status')
      setCode('')
      setError(null)
      setQr(null)
      setRecoveryCodes([])
    }
  }, [open])

  const begin = async () => {
    setBusy(true)
    setError(null)
    try {
      const res = await api.totpSetup()
      setSecret(res.secret)
      setQr(await QRCode.toDataURL(res.otpauth_url, { margin: 1, width: 192 }))
      setStep('enroll')
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const enable = async () => {
    setBusy(true)
    setError(null)
    try {
      const res = await api.totpEnable(code.trim())
      setRecoveryCodes(res.recovery_codes)
      setStep('recovery')
      onChanged()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const disable = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.totpDisable(code.trim())
      toast.info(t('Two-factor authentication disabled'))
      onChanged()
      onOpenChange(false)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const copyRecovery = () => {
    void navigator.clipboard.writeText(recoveryCodes.join('\n'))
    toast.success(t('Recovery codes copied'))
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <ShieldCheckIcon className="size-4" /> {t('Two-factor authentication')}
          </DialogTitle>
          <DialogDescription>
            {t('Protect your dashboard sign-in with one-time codes from an authenticator app (Google Authenticator, Authy, 1Password, …).')}
          </DialogDescription>
        </DialogHeader>

        {isBuiltIn ? (
          <p className="text-sm text-muted-foreground">
            {t('The built-in "aperio" admin signs in with the master token or dashboard password and cannot enroll two-factor authentication. Create a named user instead.')}
          </p>
        ) : step === 'status' && !enabled ? (
          <div className="grid gap-4">
            <p className="text-sm text-muted-foreground">
              {t('Two-factor authentication is currently off for your account.')}
            </p>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <DialogFooter>
              <Button onClick={() => void begin()} disabled={busy}>
                {busy && <Spinner />} {t('Enable')}
              </Button>
            </DialogFooter>
          </div>
        ) : step === 'status' && enabled ? (
          <div className="grid gap-4">
            <p className="text-sm text-muted-foreground">
              {t('Two-factor authentication is on. Enter a current code (or a recovery code) to turn it off.')}
            </p>
            <div className="grid gap-2">
              <Label htmlFor="totp-off">{t('Authentication code')}</Label>
              <Input
                id="totp-off"
                inputMode="numeric"
                placeholder="123456"
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <DialogFooter>
              <Button variant="destructive" onClick={() => void disable()} disabled={busy || !code.trim()}>
                {busy && <Spinner />} {t('Disable')}
              </Button>
            </DialogFooter>
          </div>
        ) : step === 'enroll' ? (
          <div className="grid gap-4">
            <p className="text-sm text-muted-foreground">
              {t('Scan the QR code with your authenticator app, then enter the 6-digit code it shows to finish.')}
            </p>
            {qr && (
              <div className="flex justify-center">
                <img src={qr} alt="TOTP QR" className="rounded-lg border bg-white p-1" />
              </div>
            )}
            <p className="break-all text-center font-mono text-xs text-muted-foreground">
              {secret}
            </p>
            <div className="grid gap-2">
              <Label htmlFor="totp-code">{t('Authentication code')}</Label>
              <Input
                id="totp-code"
                inputMode="numeric"
                placeholder="123456"
                autoFocus
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <DialogFooter>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                {t('Cancel')}
              </Button>
              <Button onClick={() => void enable()} disabled={busy || !code.trim()}>
                {busy && <Spinner />} {t('Verify & enable')}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <div className="grid gap-4">
            <p className="text-sm text-muted-foreground">
              {t('Two-factor authentication is now on. Store these single-use recovery codes somewhere safe — they are shown only once and let you sign in if you lose your authenticator.')}
            </p>
            <div className="grid grid-cols-2 gap-1 rounded-lg border bg-muted/50 p-3 font-mono text-sm">
              {recoveryCodes.map((c) => (
                <span key={c}>{c}</span>
              ))}
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={copyRecovery}>
                <CopyIcon /> {t('Copy')}
              </Button>
              <Button onClick={() => onOpenChange(false)}>{t('Done')}</Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
