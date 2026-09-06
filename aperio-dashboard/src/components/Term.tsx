import { BookOpenIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import { DOCS_BASE } from './GetStartedCard'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useI18n } from '@/i18n'

/**
 * The six words the product uses that nobody outside it knows
 * (planned_features #163), each defined in one sentence and pointed at the
 * article that explains it. The definitions go through `t`, so the seven
 * languages get them.
 */
export const GLOSSARY = {
  bind: {
    definition: 'A bind is the hostname or path prefix a client claims; a request matching it is routed to that client.',
    docs: 'routing-and-load-balancing.md#hostname-binds',
  },
  draining: {
    definition: 'A draining client has announced it is shutting down: it gets no new requests and finishes the ones in flight.',
    docs: 'client-resilience.md#graceful-shutdown',
  },
  ejected: {
    definition: 'An ejected service failed too often under real traffic and is out of rotation for a while, even if its health probe is green.',
    docs: 'routing-and-load-balancing.md#passive-outlier-ejection',
  },
  canary: {
    definition: 'A canary token is a decoy: it serves nothing, and any attempt to use it raises an alert, so a leaked copy surfaces itself.',
    docs: 'production-hardening.md#credentials--authentication',
  },
  expose: {
    definition: 'An expose entry opens a raw public port on the server that relays into a tunnel a client declared, with no second client needed.',
    docs: 'emergency-tunnels.md#public-expose',
  },
  override: {
    definition: 'A hostname override redirects a live client to other hostnames from the dashboard; it lives in memory and a reconnect or restart reverts it.',
    docs: 'dashboard.md#clients-table',
  },
} as const

export type TermKey = keyof typeof GLOSSARY

/** Wraps a word in its definition: a dotted underline, and the sentence plus
 *  a docs link on hover or focus. */
export function Term({ k, children }: { k: TermKey; children: ReactNode }) {
  const { t } = useI18n()
  const entry = GLOSSARY[k]
  return (
    <Tooltip>
      <TooltipTrigger
        render={<span className="cursor-help underline decoration-dotted underline-offset-4" tabIndex={0} />}
      >
        {children}
      </TooltipTrigger>
      <TooltipContent className="max-w-xs">
        <p>{t(entry.definition)}</p>
        <a
          className="mt-1 inline-flex items-center gap-1 text-xs underline"
          href={`${DOCS_BASE}${entry.docs}`}
          target="_blank"
          rel="noreferrer"
        >
          <BookOpenIcon className="size-3" /> {t('Read more')}
        </a>
      </TooltipContent>
    </Tooltip>
  )
}
