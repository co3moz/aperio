import {
  DownloadIcon,
  FileUpIcon,
  FilePlusIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { parse, stringify } from 'yaml'
import { SectionHeader } from './shared'
import { CopyButton } from './shared'
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from '@/components/ui/accordion'
import { Badge } from '@/components/ui/badge'
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
import {
  CLIENT_GROUPS,
  SERVER_GROUPS,
  essentialRank,
  isEssential,
  type GroupSpec,
} from '@/lib/configGroups'
import { useI18n } from '@/i18n'
import { toast } from 'sonner'

type Kind = 'client' | 'server'
type Doc = Record<string, unknown>

/** How many of a section's fields the document actually sets, so a collapsed
 *  section still says whether anything inside it is configured. */
function countSet(fields: Field[], doc: Doc): number {
  return fields.filter((f) => getAt(doc, f.path) !== undefined).length
}

/** One rendered section: the fields to decide first, and the rest. */
interface Section {
  spec: GroupSpec
  /** Shown inline when the section is open. */
  essential: Field[]
  /** Behind a nested accordion, so a long section stays readable. */
  rest: Field[]
  /** Both tiers, for counting. */
  all: Field[]
}

/** Orders fields so the ones you decide first lead. */
function orderFields(fields: Field[]): Field[] {
  return [...fields].sort((a, b) => essentialRank(a.key) - essentialRank(b.key))
}

/**
 * Arranges the schema's fields into the ordered sections, dropping the ones
 * this mode cannot use and the deprecated spellings the document does not
 * already contain. Keys no group claims land in a final catch-all, so a
 * setting added to the schema still reaches the form.
 */
function sectionsFor(
  fields: Field[],
  groups: GroupSpec[],
  hidden: Set<string>,
  doc: Doc,
): Section[] {
  const byKey = new Map(fields.map((f) => [f.key, f]))
  const claimed = new Set<string>()
  const usable = (f: Field) =>
    !hidden.has(f.key) &&
    // A deprecated spelling is only shown when the imported file uses it —
    // otherwise the form would invite writing the key we want retired.
    (!f.deprecated || getAt(doc, f.path) !== undefined)

  const split = (spec: GroupSpec, picked: Field[]): Section => ({
    spec,
    essential: picked.filter((f) => isEssential(f.key)),
    rest: picked.filter((f) => !isEssential(f.key)),
    all: picked,
  })

  const out: Section[] = []
  for (const spec of groups) {
    const picked: Field[] = []
    for (const key of spec.keys) {
      const field = byKey.get(key)
      if (!field) continue
      claimed.add(key)
      if (usable(field)) picked.push(field)
    }
    if (picked.length) out.push(split(spec, picked))
  }
  const leftovers = fields.filter((f) => !claimed.has(f.key) && usable(f))
  if (leftovers.length) {
    out.push(
      split(
        {
          title: 'Other settings',
          description: 'Everything not filed under a section above.',
          keys: [],
        },
        orderFields(leftovers),
      ),
    )
  }
  return out
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

  const sections = useMemo(() => {
    if (!schema) return []
    // Nothing is hidden by mode any more: a client file has one shape,
    // `services:`. The single-service keys are marked deprecated in the
    // schema, so they surface only for a file that already writes them —
    // which is exactly what someone migrating such a file needs.
    return sectionsFor(
      fieldsOf(schema, schema),
      kind === 'client' ? CLIENT_GROUPS : SERVER_GROUPS,
      new Set<string>(),
      doc,
    )
  }, [schema, kind, doc])

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
        <CardContent className="flex flex-wrap items-start gap-6">
          <div className="flex flex-col gap-2">
            <Label>{t('File')}</Label>
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
            {/* Keeps this column the same height as the one beside it, which
                carries a hint under its buttons. */}
            <p className="max-w-md text-xs text-muted-foreground">
              {kind === 'client'
                ? t('Configures a tunnel client and the backends it exposes.')
                : t('Configures the Aperio server itself.')}
            </p>
          </div>
        </CardContent>
      </Card>

      {error && <p className="text-sm text-destructive">{error}</p>}
      {!schema && !error && (
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner /> {t('Loading…')}
        </p>
      )}

      {schema && (
        <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,26rem)]">
          <div className="min-w-0">
            <Accordion className="w-full">
              {sections.map((section, i) => (
                <AccordionItem key={section.spec.title} value={String(i)}>
                  <AccordionTrigger>
                    <span className="flex flex-1 items-center justify-between gap-3 pr-2">
                      <span className="text-left">{t(section.spec.title)}</span>
                      <Badge variant="secondary">
                        {countSet(section.all, doc)}/{section.all.length}
                      </Badge>
                    </span>
                  </AccordionTrigger>
                  <AccordionContent>
                    <p className="mb-4 text-xs text-muted-foreground">
                      {t(section.spec.description)}
                    </p>
                    <div className="space-y-5">
                      {section.essential.map((field) => (
                        <FieldRow
                          key={field.path}
                          field={field}
                          doc={doc}
                          onChange={update}
                        />
                      ))}
                      {/* A section with no leading fields is not a short list
                          plus a long tail, it is just a list, and folding it
                          behind "More settings" makes an accordion whose only
                          child is another accordion. Show it. */}
                      {section.essential.length === 0 &&
                        section.rest.map((field) => (
                          <FieldRow
                            key={field.path}
                            field={field}
                            doc={doc}
                            onChange={update}
                          />
                        ))}
                      {section.essential.length > 0 && section.rest.length > 0 && (
                        // The long tail of a section, one level deeper: the
                        // keys you decide first stay visible, the rest are one
                        // click away instead of thirty rows of scrolling.
                        <Accordion className="w-full border-t pt-1">
                          <AccordionItem value="rest">
                            <AccordionTrigger>
                              <span className="flex flex-1 items-center justify-between gap-3 pr-2 text-sm font-normal">
                                <span>{t('More settings')}</span>
                                <Badge variant="outline">
                                  {countSet(section.rest, doc)}/{section.rest.length}
                                </Badge>
                              </span>
                            </AccordionTrigger>
                            <AccordionContent>
                              <div className="space-y-5">
                                {section.rest.map((field) => (
                                  <FieldRow
                                    key={field.path}
                                    field={field}
                                    doc={doc}
                                    onChange={update}
                                  />
                                ))}
                              </div>
                            </AccordionContent>
                          </AccordionItem>
                        </Accordion>
                      )}
                    </div>
                  </AccordionContent>
                </AccordionItem>
              ))}
            </Accordion>
          </div>

          {/* The document as it stands, beside the form rather than behind a
              button: seeing a key appear as it is filled in is what makes the
              mapping from field to file obvious. */}
          <div className="min-w-0">
            <Card className="lg:sticky lg:top-4">
              <CardHeader>
                <CardTitle className="font-mono text-sm">{filename}</CardTitle>
                <CardDescription>
                  {t(
                    'Empty fields are left out of the file entirely, so the server keeps its own default.',
                  )}
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-3">
                <pre className="max-h-[60vh] w-full overflow-auto rounded-md border bg-muted/40 p-3 text-xs leading-relaxed">
                  {yamlText || t('Nothing is set yet, so the file would be empty.')}
                </pre>
                <div className="flex flex-wrap justify-end gap-2">
                  <CopyButton value={yamlText} label={t('Copy')} />
                  <Button onClick={() => setExportOpen(true)} disabled={!yamlText}>
                    <DownloadIcon /> {t('Export YAML')}
                  </Button>
                </div>
              </CardContent>
            </Card>
          </div>
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
  if (field.kind === 'object') {
    return (
      <fieldset className="rounded-md border p-3">
        <legend className="px-1 text-sm font-medium">{field.key}</legend>
        {field.description && (
          <p className="mb-3 text-xs text-muted-foreground">{field.description}</p>
        )}
        <div className="space-y-4">
          {orderFields(field.children ?? []).map((child) => (
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

  if (field.kind === 'objectMap') {
    return <ObjectMapField field={field} doc={doc} onChange={onChange} />
  }

  if (field.kind === 'unsupported') {
    return <RawYamlField field={field} doc={doc} onChange={onChange} />
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
              {orderFields(field.children ?? []).map((child) => (
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

/**
 * The escape hatch for a shape the schema does not describe well enough to
 * build a form from — `headers.request`, say, whose value is an open map of
 * rules. Rather than telling the operator it cannot be edited here, the
 * subtree is opened as YAML in a dialog: still editable, still validated on
 * the way back in, and untouched if they change nothing.
 */
function RawYamlField({
  field,
  doc,
  onChange,
}: {
  field: Field
  doc: Doc
  onChange: (path: string, value: unknown) => void
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [text, setText] = useState('')
  const [problem, setProblem] = useState<string | null>(null)
  const value = getAt(doc, field.path)
  const isSet = value !== undefined

  const start = () => {
    setText(isSet ? stringify(value, { lineWidth: 0 }) : '')
    setProblem(null)
    setOpen(true)
  }

  const accept = () => {
    if (!text.trim()) {
      onChange(field.path, undefined)
      setOpen(false)
      return
    }
    try {
      const parsed: unknown = parse(text)
      onChange(field.path, parsed ?? undefined)
      setOpen(false)
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="grid gap-1.5">
      <Label className="font-mono text-xs">{field.key}</Label>
      <div className="flex items-center gap-2">
        <span className="text-sm text-muted-foreground">
          {isSet ? t('configured') : t('none configured')}
        </span>
        <Button variant="outline" size="sm" onClick={start}>
          <PencilIcon /> {t('Edit as YAML')}
        </Button>
      </div>
      {field.description && (
        <p className="text-xs text-muted-foreground">{field.description}</p>
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle className="font-mono">{field.key}</DialogTitle>
            <DialogDescription>
              {t(
                'This section has no fixed shape, so it is edited as YAML. Leave it empty to remove it.',
              )}
            </DialogDescription>
          </DialogHeader>
          <div className="grid min-w-0 gap-3">
            <Textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={12}
              spellCheck={false}
              className="font-mono text-xs"
            />
            {problem && <p className="text-sm text-destructive">{problem}</p>}
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setOpen(false)}>
              {t('Cancel')}
            </Button>
            <Button onClick={accept}>{t('OK')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

/**
 * A map of name → object, the shape `bind-tunnels:` uses. Editing it inline
 * would swamp the column, so the summary stays in the form and the entries are
 * edited in a dialog. Nothing here is left unconfigurable: the point of the
 * builder is that every key the schema knows can be reached.
 */
function ObjectMapField({
  field,
  doc,
  onChange,
}: {
  field: Field
  doc: Doc
  onChange: (path: string, value: unknown) => void
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [newKey, setNewKey] = useState('')
  const raw = getAt(doc, field.path)
  const entries: [string, Doc][] =
    raw && typeof raw === 'object' && !Array.isArray(raw)
      ? Object.entries(raw as Record<string, Doc>)
      : []

  const write = (next: [string, Doc][]) =>
    onChange(field.path, next.length ? Object.fromEntries(next) : undefined)

  return (
    <div className="grid gap-1.5">
      <Label className="font-mono text-xs">{field.key}</Label>
      <div className="flex items-center gap-2">
        <span className="text-sm text-muted-foreground">
          {entries.length
            ? t('{n} entry(s) configured', { n: entries.length })
            : t('none configured')}
        </span>
        <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
          <PencilIcon /> {t('Edit')}
        </Button>
      </div>
      {field.description && (
        <p className="text-xs text-muted-foreground">{field.description}</p>
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle className="font-mono">{field.key}</DialogTitle>
            <DialogDescription>{field.description}</DialogDescription>
          </DialogHeader>
          <div className="grid max-h-[60vh] min-w-0 gap-4 overflow-y-auto">
            {entries.map(([name, entry], i) => (
              <div key={name} className="rounded-md border bg-muted/30 p-3">
                <div className="mb-3 flex items-center justify-between gap-2">
                  <span className="font-mono text-xs font-medium">{name}</span>
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
                  {orderFields(field.children ?? []).map((child) => (
                    <FieldRow
                      key={child.path}
                      field={{ ...child, path: child.key }}
                      doc={entry}
                      onChange={(path, value) =>
                        write(
                          entries.map((e, j) =>
                            j === i ? [e[0], setAt(e[1], path, value)] : e,
                          ),
                        )
                      }
                    />
                  ))}
                </div>
              </div>
            ))}
            <div className="flex gap-2">
              <Input
                value={newKey}
                placeholder={t('New entry name')}
                onChange={(e) => setNewKey(e.target.value)}
              />
              <Button
                variant="outline"
                disabled={
                  !newKey.trim() || entries.some(([k]) => k === newKey.trim())
                }
                onClick={() => {
                  write([...entries, [newKey.trim(), {}]])
                  setNewKey('')
                }}
              >
                <PlusIcon /> {t('Add')}
              </Button>
            </div>
          </div>
          <DialogFooter>
            <Button onClick={() => setOpen(false)}>{t('Done')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
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
