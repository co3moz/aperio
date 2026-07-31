import { CircleCheckIcon, CircleSlashIcon, MinusIcon, SearchIcon } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Spinner } from '@/components/ui/spinner'
import { api, ApiError, type Explanation } from '@/lib/api'
import { useI18n } from '@/i18n'
import { cn } from '@/lib/utils'

/** One icon per verdict, so the deciding step is findable without reading. */
const MARK = {
  decides: { icon: CircleSlashIcon, className: 'text-amber-600 dark:text-amber-400' },
  passes: { icon: CircleCheckIcon, className: 'text-emerald-600 dark:text-emerald-400' },
  skipped: { icon: MinusIcon, className: 'text-muted-foreground' },
} as const

/**
 * Ask what would happen to a request, without sending one.
 *
 * The proxy makes a dozen decisions before a byte reaches a backend, and when
 * the answer surprises someone the dashboard could show the result but never
 * the reason: a hostname answering 503 with a maintenance flag set by an
 * organization you are not viewing looked exactly like a broken client. This
 * walks the same decisions in the same order and marks the one that decides.
 */
export function ExplainSection() {
  const { t } = useI18n()
  const [target, setTarget] = useState('')
  const [result, setResult] = useState<Explanation | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const ask = async (e: FormEvent) => {
    e.preventDefault()
    const value = target.trim()
    if (!value) return
    setBusy(true)
    setError(null)
    try {
      setResult(await api.explain(value))
    } catch (err) {
      setResult(null)
      setError(err instanceof ApiError ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader
        title={t('Explain a request')}
        description={t('What would answer a request to this hostname and path, and why. Nothing is sent: no rate limit is spent, no round-robin cursor moves, and a scaled-to-zero service is not woken.')}
      />
      <Card className="py-5">
        <CardContent className="flex flex-col gap-4 px-5">
          <form onSubmit={ask} className="flex flex-wrap items-center gap-2">
            <Input
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              placeholder={t('app.example.com/api, or a full URL')}
              className="max-w-md"
            />
            <Button type="submit" variant="outline" disabled={busy}>
              {busy ? <Spinner /> : <SearchIcon />} {t('Explain')}
            </Button>
          </form>
          {error && <p className="text-sm text-destructive">{error}</p>}
          {result && (
            <>
              <p className="rounded-3xl border bg-muted/40 px-4 py-3 text-sm">
                <span className="font-mono text-xs text-muted-foreground">
                  {result.method} {result.hostname}
                  {result.path}
                </span>
                <br />
                {result.summary}
              </p>
              <ol className="flex flex-col gap-1.5">
                {result.steps.map((step) => {
                  const mark = MARK[step.verdict]
                  const Icon = mark.icon
                  return (
                    <li
                      key={step.stage}
                      className={cn(
                        'flex items-start gap-2.5 rounded-3xl border px-4 py-2.5 text-sm',
                        step.verdict === 'decides' && 'border-amber-500/40 bg-amber-500/10',
                      )}
                    >
                      <Icon className={cn('mt-0.5 size-4 shrink-0', mark.className)} />
                      <span className="flex flex-col gap-0.5">
                        <span className="font-mono text-xs text-muted-foreground">
                          {step.stage}
                          {step.setting && ` · ${step.setting}`}
                        </span>
                        <span>{step.detail}</span>
                      </span>
                    </li>
                  )
                })}
              </ol>
            </>
          )}
        </CardContent>
      </Card>
    </section>
  )
}
