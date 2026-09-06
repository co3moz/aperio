import { BookOpenIcon, KeyRoundIcon, ServerIcon, TerminalIcon } from 'lucide-react'
import type { Page } from './AppSidebar'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { useHasRole } from '@/lib/session'
import { useI18n } from '@/i18n'

/** Where the articles live; the page header links here too. */
export const DOCS_BASE = 'https://github.com/co3moz/aperio/blob/master/docs/'

/**
 * What an empty server tells a newcomer (planned_features #160). Six zero
 * tiles and a flat chart ask for nothing; this says what makes a tunnel, in
 * the order it happens, with each step one click from the page that does
 * it. It leaves the moment a client connects, since it is a door and not a
 * banner.
 */
export function GetStartedCard({ onNavigate }: { onNavigate: (page: Page) => void }) {
  const { t } = useI18n()
  const canMint = useHasRole('operator')
  // The server this page was served by is the server the client dials.
  const origin = typeof window === 'undefined' ? 'https://tunnel.example.com' : window.location.origin
  const steps: { icon: typeof KeyRoundIcon; title: string; body: string; action?: { label: string; page: Page } }[] = [
    {
      icon: KeyRoundIcon,
      title: t('1. Mint a token'),
      body: canMint
        ? t('A token is what a client connects with. Scope it to the hostname the service will answer on.')
        : t('A token is what a client connects with. An operator mints one on the API Tokens page.'),
      action: { label: t('API Tokens'), page: 'tokens' },
    },
    {
      icon: TerminalIcon,
      title: t('2. Run the client next to your service'),
      body: t('On the machine that can reach the service, with the token from step 1. The Clients page has a wizard that writes this for you.'),
      action: { label: t('Connect a new client'), page: 'clients' },
    },
    {
      icon: ServerIcon,
      title: t('3. Watch it appear here'),
      body: t('The client dials out to this server and the tunnel is up; this card leaves the moment it connects.'),
    },
  ]
  return (
    <Card>
      <CardHeader>
        <CardTitle className="font-heading">{t('No client is connected yet')}</CardTitle>
        <CardDescription>
          {t('Three steps put a service on this server. Nothing on your side accepts inbound connections; the client dials out.')}
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4 md:grid-cols-3">
        {steps.map((s) => (
          <div key={s.title} className="flex flex-col gap-2 rounded-2xl border p-4">
            <div className="flex items-center gap-2 text-sm font-semibold">
              <s.icon className="size-4 text-primary" />
              {s.title}
            </div>
            <p className="text-sm text-muted-foreground">{s.body}</p>
            {s.action && (
              <Button size="sm" variant="outline" className="mt-auto w-fit" onClick={() => onNavigate(s.action!.page)}>
                {s.action.label}
              </Button>
            )}
          </div>
        ))}
        <pre className="col-span-full overflow-x-auto rounded-2xl bg-muted p-4 font-mono text-xs">
          {`aperio-client 3000 --server-url ${origin} --server-token apr_...`}
        </pre>
        <a
          className="col-span-full flex w-fit items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
          href={`${DOCS_BASE}getting-started.md`}
          target="_blank"
          rel="noreferrer"
        >
          <BookOpenIcon className="size-4" /> {t('Getting started, the full walkthrough')}
        </a>
      </CardContent>
    </Card>
  )
}
