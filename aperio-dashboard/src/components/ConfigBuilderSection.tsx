import {
  DownloadIcon,
  FileUpIcon,
  FilePlusIcon,
  PlusIcon,
  Trash2Icon,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { parse, stringify } from 'yaml'
import { SectionHeader } from './shared'
import { CopyButton } from './shared'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { api, ApiError } from '@/lib/api'
import {
  BYTE_UNITS,
  fieldsOf,
  getAt,
  setAt,
  splitBytes,
  toBytes,
  type Field,
  type JsonSchema,
} from '@/lib/configSchema'
import { useI18n } from '@/i18n'
import { toast } from 'sonner'

type Kind = 'client' | 'server'
type Doc = Record<string, unknown>

/** Fields worth showing first; everything else keeps schema order below them. */
const LEAD_KEYS: Record<Kind, string[]> = {
  client: ['server', 'target', 'serve', 'hostname', 'path'],
  server: ['server', 'host', 'port', 'data_dir', 'log_level'],
}

/**
 * Builds an `aperio.yaml` or `aperio-server.yaml` from a form, or edits one
 * that already exists.
 *
 * The form is generated from the JSON Schema the server serves, which is
 * derived from the very Rust types that parse these files — so every setting
 * the running binary understands appears here, and nothing that does not.
 * Anything the form cannot render (a map of maps, say) is preserved verbatim
 * through import and export rather than dropped, so the builder is safe to
 * open an existing file in.
 */
export function ConfigBuilderSection() {
  const { t } = useI18n()
  const [kind, setKind] = useState<Kind>('client')
  const [schemas, setSchemas] = useState<Partial<Record<Kind, JsonSchema>>>({})
  const [docs, setDocs] = useState<Record<Kind, Doc>>({ client: {}, server: {} })
  const [error, setError] = useState<string | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)

  const schema = schemas[kind]
  const doc = docs[kind]

  useEffect(() => {
    if (schemas[kind]) return
    let cancelled = false
    api
      .configSchema(kind)
      .then((s) => {
        if (!cancelled) setSchemas((prev) => ({ ...prev, [kind]: s }))
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : String(e))
      })
    return () => {
      cancelled = true
    }
  }, [kind, schemas])

  const fields = useMemo(() => {
    if (!schema) return []
    const all = fieldsOf(schema, schema)
    const lead = LEAD_KEYS[kind]
    return [...all].sort((a, b) => {
      const ai = lead.indexOf(a.key)
      const bi = lead.indexOf(b.key)
      if (ai === -1 && bi === -1) return 0
      if (ai === -1) return 1
      if (bi === -1) return -1
      return ai - bi
    })
  }, [schema, kind])

  const update = useCallback(
    (path: string, value: unknown) =>
      setDocs((prev) => ({ ...prev, [kind]: setAt(prev[kind], path, value) })),
    [kind],
  )

  const filename = kind === 'client' ? 'aperio.yaml' : 'aperio-server.yaml'
  const yamlText = useMemo(() => {
    if (Object.keys(doc).length === 0) return ''
    try {
      return stringify(doc, { lineWidth: 0 })
    } catch {
      return ''
    }
  }, [doc])

  return (
    <div className="space-y-6">
      <SectionHeader
        title={t('Config Builder')}
        description={t(
          'Assemble an aperio.yaml or aperio-server.yaml from the settings this server understands, or open an existing one and edit it.',
        )}
      >
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="outline" onClick={() => setImportOpen(true)}>
            <FileUpIcon /> {t('Import YAML')}
          </Button>
          <Button
            variant="outline"
            onClick={() => {
              setDocs((prev) => ({ ...prev, [kind]: {} }))
              toast.success(t('Started a new {file}', { file: filename }))
            }}
          >
            <FilePlusIcon /> {t('New')}
          </Button>
        </div>
      </SectionHeader>

      <Card>
        <CardHeader>
          <CardTitle>{t('Which file?')}</CardTitle>
          <CardDescription>
            {t(
              'The client file configures a tunnel client and what it exposes; the server file configures the Aperio server itself.',
            )}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <ToggleGroup
            variant="outline"
            spacing={0}
            value={[kind]}
            multiple={false}
            onValueChange={(v: string[]) => {
              const next = v[0]
              if (next === 'client' || next === 'server') setKind(next)
            }}
          >
            <ToggleGroupItem value="client">aperio.yaml</ToggleGroupItem>
            <ToggleGroupItem value="server">aperio-server.yaml</ToggleGroupItem>
          </ToggleGroup>
        </CardContent>
      </Card>

      {error && <p className="text-sm text-destructive">{error}</p>}
      {!schema && !error && (
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner /> {t('Loading…')}
        </p>
      )}

      {schema && (
        <Card>
          <CardHeader>
            <CardTitle>{filename}</CardTitle>
            <CardDescription>
              {t(
                'Empty fields are left out of the file entirely, so the server keeps its own default.',
              )}
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-5">
            {fields.map((field) => (
              <FieldRow
                key={field.path}
                field={field}
                doc={doc}
                onChange={update}
              />
            ))}
          </CardContent>
        </Card>
      )}

      {schema && (
        <div className="flex justify-end">
          <Button onClick={() => setExportOpen(true)}>
            <DownloadIcon /> {t('Export YAML')}
          </Button>
        </div>
      )}

      <ImportDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        filename={filename}
        onImport={(parsed) => setDocs((prev) => ({ ...prev, [kind]: parsed }))}
      />
      <ExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        filename={filename}
        text={yamlText}
      />
    </div>
  )
}

/** One setting: a label, its schema description, and the right editor. */
function FieldRow({
  field,
  doc,
  onChange,
}: {
  field: Field
  doc: Doc
  onChange: (path: string, value: unknown) => void
}) {
  const { t } = useI18n()

  if (field.kind === 'object') {
    return (
      <fieldset className="rounded-md border p-3">
        <legend className="px-1 text-sm font-medium">{field.key}</legend>
        {field.description && (
          <p className="mb-3 text-xs text-muted-foreground">{field.description}</p>
        )}
        <div className="space-y-4">
          {field.children?.map((child) => (
            <FieldRow
              key={child.path}
              field={child}
              doc={doc}
              onChange={onChange}
            />
          ))}
        </div>
      </fieldset>
    )
  }

  if (field.kind === 'objectList') {
    return <ObjectListField field={field} doc={doc} onChange={onChange} />
  }

  if (field.kind === 'unsupported') {
    return (
      <div className="rounded-md border border-dashed p-3 text-xs text-muted-foreground">
        <span className="font-mono">{field.key}</span>{' '}
        {t(
          'is too structured to edit here; it is preserved exactly as imported.',
        )}
      </div>
    )
  }

  return (
    <div className="grid gap-1.5">
      <Label htmlFor={field.path} className="font-mono text-xs">
        {field.key}
      </Label>
      <ScalarEditor field={field} doc={doc} onChange={onChange} />
      {field.description && (
        <p className="text-xs text-muted-foreground">{field.description}</p>
      )}
    </div>
  )
}

/** The editors for the leaf kinds. */
function ScalarEditor({
  field,
  doc,
  onChange,
}: {
  field: Field
  doc: Doc
  onChange: (path: string, value: unknown) => void
}) {
  const { t } = useI18n()
  const value = getAt(doc, field.path)

  if (field.kind === 'boolean') {
    return (
      <div className="flex items-center gap-2">
        <Switch
          id={field.path}
          checked={value === true}
          onCheckedChange={(on) => onChange(field.path, on ? true : undefined)}
        />
        <span className="text-xs text-muted-foreground">
          {value === true ? t('on') : t('unset (server default)')}
        </span>
      </div>
    )
  }

  if (field.kind === 'select') {
    return (
      <Select
        value={typeof value === 'string' ? value : ''}
        onValueChange={(v) => onChange(field.path, v === '__unset' ? undefined : v)}
      >
        <SelectTrigger id={field.path}>
          <SelectValue placeholder={t('unset (server default)')} />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="__unset">{t('unset (server default)')}</SelectItem>
          {field.options?.map((o) => (
            <SelectItem key={o} value={o}>
              {o}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    )
  }

  if (field.kind === 'bytes') {
    return <BytesEditor field={field} value={value} onChange={onChange} />
  }

  if (field.kind === 'stringList') {
    const list = Array.isArray(value) ? value : []
    return (
      <Input
        id={field.path}
        value={list.join(', ')}
        placeholder={field.example ?? t('comma-separated')}
        onChange={(e) => {
          const items = e.target.value
            .split(',')
            .map((s) => s.trim())
            .filter(Boolean)
          onChange(field.path, items.length ? items : undefined)
        }}
      />
    )
  }

  if (field.kind === 'number') {
    return (
      <Input
        id={field.path}
        type="number"
        value={typeof value === 'number' ? String(value) : ''}
        placeholder={field.example}
        onChange={(e) => {
          const raw = e.target.value
          if (raw === '') return onChange(field.path, undefined)
          const n = Number(raw)
          onChange(field.path, Number.isFinite(n) ? n : undefined)
        }}
      />
    )
  }

  return (
    <Input
      id={field.path}
      value={typeof value === 'string' ? value : ''}
      placeholder={
        field.example ?? (field.kind === 'duration' ? '30s' : undefined)
      }
      onChange={(e) => onChange(field.path, e.target.value || undefined)}
    />
  )
}

/** A byte size, entered as an amount plus a unit rather than raw bytes. */
function BytesEditor({
  field,
  value,
  onChange,
}: {
  field: Field
  value: unknown
  onChange: (path: string, value: unknown) => void
}) {
  const { t } = useI18n()
  const current = typeof value === 'number' ? splitBytes(value) : null
  const [unit, setUnit] = useState(current?.unit ?? 'MB')
  const shown = current ? String(current.amount) : ''
  // A value typed while the unit is MB must not be re-split into GB under the
  // cursor, so the unit follows the stored value only when it changes shape.
  const lastValue = useRef(value)
  useEffect(() => {
    if (value !== lastValue.current) {
      lastValue.current = value
      if (typeof value === 'number') setUnit(splitBytes(value).unit)
    }
  }, [value])

  return (
    <div className="flex gap-2">
      <Input
        id={field.path}
        type="number"
        className="flex-1"
        value={shown}
        placeholder={field.example}
        onChange={(e) => {
          const raw = e.target.value
          if (raw === '') return onChange(field.path, undefined)
          onChange(field.path, toBytes(Number(raw), unit))
        }}
      />
      <Select
        value={unit}
        onValueChange={(u) => {
          if (u === null) return
          setUnit(u)
          if (shown !== '') onChange(field.path, toBytes(Number(shown), u))
        }}
      >
        <SelectTrigger className="w-24" aria-label={t('unit')}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {BYTE_UNITS.map((u) => (
            <SelectItem key={u.label} value={u.label}>
              {u.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}

/** A list of objects: `services:`, `tunnels:`, `routes:`, and friends. */
function ObjectListField({
  field,
  doc,
  onChange,
}: {
  field: Field
  doc: Doc
  onChange: (path: string, value: unknown) => void
}) {
  const { t } = useI18n()
  const raw = getAt(doc, field.path)
  const entries: Doc[] = Array.isArray(raw) ? (raw as Doc[]) : []

  const write = (next: Doc[]) =>
    onChange(field.path, next.length ? next : undefined)

  return (
    <fieldset className="rounded-md border p-3">
      <legend className="px-1 text-sm font-medium">{field.key}</legend>
      {field.description && (
        <p className="mb-3 text-xs text-muted-foreground">{field.description}</p>
      )}
      <div className="space-y-4">
        {entries.map((entry, i) => (
          <div key={i} className="rounded-md border bg-muted/30 p-3">
            <div className="mb-3 flex items-center justify-between">
              <span className="text-xs font-medium text-muted-foreground">
                {t('Entry {n}', { n: i + 1 })}
              </span>
              <Button
                variant="ghost"
                size="sm"
                aria-label={t('Remove entry')}
                onClick={() => write(entries.filter((_, j) => j !== i))}
              >
                <Trash2Icon />
              </Button>
            </div>
            <div className="space-y-4">
              {field.children?.map((child) => (
                <FieldRow
                  key={child.path}
                  field={{ ...child, path: child.key }}
                  doc={entry}
                  onChange={(path, value) =>
                    write(
                      entries.map((e, j) =>
                        j === i ? setAt(e, path, value) : e,
                      ),
                    )
                  }
                />
              ))}
            </div>
          </div>
        ))}
        <Button
          variant="outline"
          size="sm"
          onClick={() => write([...entries, {}])}
        >
          <PlusIcon /> {t('Add entry')}
        </Button>
      </div>
    </fieldset>
  )
}

/** Paste a document or pick a file; either replaces what the form holds. */
function ImportDialog({
  open,
  onOpenChange,
  filename,
  onImport,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  filename: string
  onImport: (doc: Doc) => void
}) {
  const { t } = useI18n()
  const [text, setText] = useState('')
  const [problem, setProblem] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (open) {
      setText('')
      setProblem(null)
    }
  }, [open])

  const accept = () => {
    let parsed: unknown
    try {
      parsed = parse(text)
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e))
      return
    }
    if (parsed === null || parsed === undefined) {
      onImport({})
    } else if (typeof parsed !== 'object' || Array.isArray(parsed)) {
      setProblem(t('A configuration file must be a mapping of settings.'))
      return
    } else {
      onImport(parsed as Doc)
    }
    onOpenChange(false)
    toast.success(t('Imported {file}', { file: filename }))
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t('Import {file}', { file: filename })}</DialogTitle>
          <DialogDescription>
            {t(
              'Paste the document, or open a file. Settings the form cannot edit are kept as they are.',
            )}
          </DialogDescription>
        </DialogHeader>
        <div className="grid min-w-0 gap-3">
          <Textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={14}
            spellCheck={false}
            className="font-mono text-xs"
            placeholder={'server:\n  url: https://tunnel.example.com\n'}
          />
          {problem && <p className="text-sm text-destructive">{problem}</p>}
          <div>
            <input
              ref={fileInput}
              type="file"
              accept=".yaml,.yml,text/yaml"
              className="hidden"
              onChange={async (e) => {
                const file = e.target.files?.[0]
                if (!file) return
                const contents = await file.text()
                setText(contents)
                setProblem(null)
                e.target.value = ''
              }}
            />
            <Button variant="outline" onClick={() => fileInput.current?.click()}>
              <FileUpIcon /> {t('Open file…')}
            </Button>
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={accept} disabled={!text.trim()}>
            {t('OK')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** The finished document, to copy or save. */
function ExportDialog({
  open,
  onOpenChange,
  filename,
  text,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  filename: string
  text: string
}) {
  const { t } = useI18n()

  const save = () => {
    const blob = new Blob([text], { type: 'text/yaml;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = filename
    a.click()
    URL.revokeObjectURL(url)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{filename}</DialogTitle>
          <DialogDescription>
            {t('Copy it, or save it next to the binary that reads it.')}
          </DialogDescription>
        </DialogHeader>
        <div className="grid min-w-0 gap-3">
          {text ? (
            <Textarea
              value={text}
              readOnly
              rows={16}
              spellCheck={false}
              className="font-mono text-xs"
            />
          ) : (
            <p className="text-sm text-muted-foreground">
              {t('Nothing is set yet, so the file would be empty.')}
            </p>
          )}
        </div>
        <DialogFooter>
          <CopyButton value={text} label={t('Copy')} />
          <Button onClick={save} disabled={!text}>
            <DownloadIcon /> {t('Save as file')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
