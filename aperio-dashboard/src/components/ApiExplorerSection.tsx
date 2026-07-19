import { ChevronRightIcon, PlayIcon, SearchIcon } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { SectionHeader } from './shared'
import { MethodBadge, StatusBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { useI18n } from '@/i18n'

/* Minimal OpenAPI 3 shapes — only what the explorer renders. */
interface OpenApiParameter {
  name: string
  in: 'path' | 'query' | 'header' | 'cookie'
  description?: string
  required?: boolean
}
interface OpenApiOperation {
  tags?: string[]
  description?: string
  summary?: string
  parameters?: OpenApiParameter[]
  requestBody?: { content?: Record<string, { schema?: unknown }> }
  responses?: Record<string, { description?: string }>
}
interface OpenApiDoc {
  info?: { title?: string; description?: string; version?: string }
  paths: Record<string, Record<string, OpenApiOperation>>
  tags?: { name: string; description?: string }[]
}

interface Operation {
  method: string
  path: string
  op: OpenApiOperation
}

const METHOD_ORDER = ['get', 'post', 'put', 'patch', 'delete']
const BODY_METHODS = new Set(['POST', 'PUT', 'PATCH'])

/** One expandable operation row with an inline try-it form. */
function OperationRow({ method, path, op }: Operation) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [pathValues, setPathValues] = useState<Record<string, string>>({})
  const [query, setQuery] = useState('')
  const [body, setBody] = useState('')
  const [running, setRunning] = useState(false)
  const [result, setResult] = useState<{ status: number; body: string } | null>(null)

  const pathParams = (op.parameters ?? []).filter((p) => p.in === 'path')
  const queryParams = (op.parameters ?? []).filter((p) => p.in === 'query')
  const upper = method.toUpperCase()

  const execute = async () => {
    let target = path
    for (const p of pathParams) {
      target = target.replace(`{${p.name}}`, encodeURIComponent(pathValues[p.name] ?? ''))
    }
    if (query.trim()) target += (target.includes('?') ? '&' : '?') + query.trim()
    setRunning(true)
    setResult(null)
    try {
      const init: RequestInit = { method: upper }
      if (BODY_METHODS.has(upper) && body.trim()) {
        init.headers = { 'Content-Type': 'application/json' }
        init.body = body
      }
      const res = await fetch(target, init)
      const text = await res.text()
      let pretty = text
      try {
        pretty = JSON.stringify(JSON.parse(text), null, 2)
      } catch {
        /* not JSON — show as-is */
      }
      setResult({ status: res.status, body: pretty.slice(0, 20_000) })
    } catch (e) {
      setResult({ status: 0, body: String(e) })
    } finally {
      setRunning(false)
    }
  }

  return (
    <div className="border-b last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-3 px-4 py-2.5 text-left hover:bg-muted/50"
      >
        <ChevronRightIcon
          className={cn('size-4 shrink-0 text-muted-foreground transition-transform', open && 'rotate-90')}
        />
        <MethodBadge method={upper} />
        <code className="font-mono text-sm">{path}</code>
        <span className="ml-auto hidden truncate pl-4 text-xs text-muted-foreground sm:inline">
          {op.summary ?? op.description ?? ''}
        </span>
      </button>
      {open && (
        <div className="space-y-3 border-t bg-muted/30 px-11 py-3">
          {op.description && <p className="text-sm text-muted-foreground">{op.description}</p>}
          {pathParams.map((p) => (
            <div key={p.name} className="flex items-center gap-2">
              <code className="w-40 shrink-0 font-mono text-xs">{`{${p.name}}`}</code>
              <Input
                placeholder={p.description ?? p.name}
                value={pathValues[p.name] ?? ''}
                onChange={(e) => setPathValues((v) => ({ ...v, [p.name]: e.target.value }))}
                className="h-8 max-w-md font-mono text-xs"
              />
            </div>
          ))}
          {queryParams.length > 0 && (
            <div className="flex items-center gap-2">
              <span className="w-40 shrink-0 text-xs text-muted-foreground">{t('Query string')}</span>
              <Input
                placeholder={queryParams.map((p) => `${p.name}=…`).join('&')}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                className="h-8 max-w-md font-mono text-xs"
              />
            </div>
          )}
          {BODY_METHODS.has(upper) && (
            <Textarea
              placeholder={t('JSON request body (optional)')}
              value={body}
              onChange={(e) => setBody(e.target.value)}
              className="max-w-2xl font-mono text-xs"
              rows={4}
            />
          )}
          <div className="flex items-center gap-3">
            <Button size="sm" onClick={() => void execute()} disabled={running}>
              <PlayIcon /> {running ? t('Sending...') : t('Send request')}
            </Button>
            {result && <StatusBadge status={result.status || null} error={result.status ? null : result.body} />}
            <span className="text-xs text-muted-foreground">
              {t('Runs against this server with your current session.')}
            </span>
          </div>
          {result && (
            <pre className="max-h-80 overflow-auto rounded-md bg-zinc-950 p-3 font-mono text-xs text-zinc-100">
              {result.body || t('(empty response body)')}
            </pre>
          )}
        </div>
      )}
    </div>
  )
}

/**
 * Embedded API explorer over the server's own `/aperio/api/openapi.json`:
 * operations grouped by tag, expandable into a description and an inline
 * try-it form that runs against this server with the dashboard session.
 * Fully self-contained — no external Swagger assets.
 */
export function ApiExplorerSection() {
  const { t } = useI18n()
  const [doc, setDoc] = useState<OpenApiDoc | null>(null)
  const [error, setError] = useState(false)
  const [filter, setFilter] = useState('')

  useEffect(() => {
    fetch('/aperio/api/openapi.json')
      .then((r) => r.json())
      .then((d: OpenApiDoc) => setDoc(d))
      .catch(() => setError(true))
  }, [])

  const groups = useMemo(() => {
    if (!doc) return []
    const byTag = new Map<string, Operation[]>()
    for (const [path, methods] of Object.entries(doc.paths)) {
      for (const method of METHOD_ORDER) {
        const op = methods[method]
        if (!op) continue
        const tag = op.tags?.[0] ?? 'other'
        if (!byTag.has(tag)) byTag.set(tag, [])
        byTag.get(tag)!.push({ method, path, op })
      }
    }
    const needle = filter.toLowerCase()
    const order = (doc.tags ?? []).map((t2) => t2.name)
    return [...byTag.entries()]
      .sort((a, b) => {
        const ia = order.indexOf(a[0])
        const ib = order.indexOf(b[0])
        return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib)
      })
      .map(([tag, ops]) => ({
        tag,
        description: doc.tags?.find((t2) => t2.name === tag)?.description,
        ops: ops
          .filter(
            (o) =>
              !needle ||
              o.path.toLowerCase().includes(needle) ||
              o.method.includes(needle) ||
              (o.op.description ?? '').toLowerCase().includes(needle),
          )
          .sort((a, b) => a.path.localeCompare(b.path)),
      }))
      .filter((g) => g.ops.length > 0)
  }, [doc, filter])

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('API Explorer')}>
        <Button
          size="sm"
          variant="outline"
          onClick={() => window.open('/aperio/api/openapi.json', '_blank')}
        >
          openapi.json
        </Button>
        <div className="relative">
          <SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder={t('Filter endpoints...')}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-64 pl-8"
          />
        </div>
      </SectionHeader>

      {doc?.info?.description && (
        <p className="max-w-3xl text-sm text-muted-foreground">{doc.info.description}</p>
      )}

      {error ? (
        <Card>
          <CardContent className="py-8 text-center text-sm text-muted-foreground">
            {t('Could not load the OpenAPI document.')}
          </CardContent>
        </Card>
      ) : doc === null ? (
        <Card>
          <CardContent className="py-8 text-center text-sm text-muted-foreground">
            {t('Loading the API document...')}
          </CardContent>
        </Card>
      ) : (
        groups.map((group) => (
          <div key={group.tag} className="flex flex-col gap-1.5">
            <h3 className="mt-2 text-sm font-semibold capitalize">{group.tag}</h3>
            {group.description && (
              <p className="text-xs text-muted-foreground">{group.description}</p>
            )}
            <Card className="overflow-hidden py-0">
              {group.ops.map((o) => (
                <OperationRow key={`${o.method} ${o.path}`} {...o} />
              ))}
            </Card>
          </div>
        ))
      )}
    </section>
  )
}
