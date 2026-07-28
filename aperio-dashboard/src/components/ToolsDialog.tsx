import { BracesIcon, FileCog2Icon, ScrollTextIcon } from 'lucide-react'
import { ApiExplorerSection } from './ApiExplorerSection'
import { AuditSection } from './AuditSection'
import { ConfigBuilderSection } from './ConfigBuilderSection'
import { PaneDialog, type PaneSpec } from './PaneDialog'
import { useI18n } from '@/i18n'
import type { Role } from '@/lib/api'
import type { PaneFocus } from '@/lib/paneFocus'
import type { Page } from './AppSidebar'

/**
 * The pages you reach for when something needs working out, rather than to
 * run the tunnel day to day.
 *
 * None of them is where the work happens: you check the audit log because
 * something changed and you want to know who, you open the API explorer
 * because a call is not doing what you expected, you open the builder because
 * a config file needs writing. Instruments, in other words, which is why they
 * sit together behind one entry instead of taking four rows of the sidebar.
 */
export const TOOLS_PAGES = ['audit', 'api', 'config-builder'] as const

export type ToolsPage = (typeof TOOLS_PAGES)[number]

export function isToolsPage(page: Page): page is ToolsPage {
  return (TOOLS_PAGES as readonly string[]).includes(page)
}

export const TOOLS_PANES: PaneSpec<ToolsPage>[] = [
  { id: 'audit', label: 'Audit Log', icon: ScrollTextIcon, minRole: 'viewer' },
  { id: 'api', label: 'API Explorer', icon: BracesIcon, minRole: 'viewer' },
  { id: 'config-builder', label: 'Config Builder', icon: FileCog2Icon, minRole: 'viewer' },
]

export function ToolsDialog({
  page,
  role,
  masterAdmin,
  focus,
  onNavigate,
  onClose,
}: {
  page: ToolsPage
  role: Role
  masterAdmin: boolean
  focus?: PaneFocus | null
  onNavigate: (page: ToolsPage) => void
  onClose: () => void
}) {
  const { t } = useI18n()
  return (
    <PaneDialog
      page={page}
      panes={TOOLS_PANES}
      role={role}
      masterAdmin={masterAdmin}
      focus={focus}
      title={t('Tools')}
      description={t('Diagnose, inspect and generate — the things you reach for when something needs working out.')}
      // Wider than Settings, which holds forms: a request/response pair, a
      // signature table and a generated YAML file are all wide by nature, and
      // this is the width that stops them wrapping into uselessness.
      className="sm:max-w-6xl"
      onNavigate={onNavigate}
      onClose={onClose}
    >
      {(current) => (
        <>
          {current === 'audit' && <AuditSection />}
          {current === 'api' && <ApiExplorerSection />}
          {current === 'config-builder' && <ConfigBuilderSection />}
        </>
      )}
    </PaneDialog>
  )
}
