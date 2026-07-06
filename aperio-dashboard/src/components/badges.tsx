import { Badge } from '@radix-ui/themes'

type BadgeColor = 'indigo' | 'green' | 'amber' | 'red' | 'gray'

const METHOD_COLORS: Record<string, BadgeColor> = {
  GET: 'indigo',
  POST: 'green',
  PUT: 'amber',
  PATCH: 'amber',
  DELETE: 'red',
}

export function MethodBadge({ method }: { method: string }) {
  const m = method.toUpperCase()
  return <Badge color={METHOD_COLORS[m] ?? 'gray'}>{m}</Badge>
}

export function StatusBadge({ status, error }: { status: number | null; error?: string | null }) {
  if (error) return <Badge color="red">ERR</Badge>
  if (!status) return <Badge color="gray">-</Badge>
  const color: BadgeColor =
    status < 300 ? 'green' : status < 400 ? 'indigo' : status < 500 ? 'amber' : 'red'
  return <Badge color={color}>{status}</Badge>
}
