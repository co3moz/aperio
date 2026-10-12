import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import { useI18n } from '@/i18n'
import { api, type ReachableOrg, type Role } from './api'

const ORDER: Record<Role, number> = { viewer: 0, operator: 1, admin: 2 }

interface SessionValue {
  username: string
  role: Role
  /**
   * The organization this session currently views (`master` or a child id).
   *
   * Every org-scoped list the server returns is filtered by it, so a section
   * showing one needs it to say *whose* users or tokens these are.
   */
  selectedOrg: string
  /** The server admin (`*` Admin): lists organizations, runs the
   *  server-global screens, and may grant `*` to somebody else. */
  serverAdmin: boolean
  /** Every organization a grant reaches, with the role held there. */
  orgs: ReachableOrg[]
}

const SessionContext = createContext<SessionValue>({
  username: 'aperio',
  role: 'admin',
  selectedOrg: 'master',
  serverAdmin: false,
  orgs: [],
})

export function SessionProvider({
  username,
  role,
  selectedOrg,
  serverAdmin,
  orgs,
  children,
}: {
  username: string
  role: Role
  selectedOrg: string
  serverAdmin: boolean
  orgs: ReachableOrg[]
  children: ReactNode
}) {
  return (
    <SessionContext.Provider value={{ username, role, selectedOrg, serverAdmin, orgs }}>
      {children}
    </SessionContext.Provider>
  )
}

export function useSession(): SessionValue {
  return useContext(SessionContext)
}

/**
 * A readable name for the organization this session is looking at.
 *
 * The session carries the name of every organization a grant reaches, so
 * that is read first; the organization listing is asked only by the server
 * admin, for whom it is not a guaranteed 403, and only for a name the
 * session did not carry. The implicit master org has no record either way.
 */
export function useOrgName(): string {
  const { t } = useI18n()
  const { selectedOrg, serverAdmin, orgs } = useSession()
  const [name, setName] = useState<string | null>(null)
  const known = orgs.find((o) => o.id === selectedOrg)

  useEffect(() => {
    // Never issue the request as anyone but the server admin: it is a
    // guaranteed 403.
    // Fetched once rather than polled, an org is renamed about as often as it
    // is created, and switching into one reloads the dashboard anyway.
    if (!serverAdmin || selectedOrg === 'master' || known) return
    let live = true
    api
      .orgs()
      .then((list) => {
        if (live) setName(list.find((o) => o.id === selectedOrg)?.name ?? null)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [serverAdmin, selectedOrg, known])

  if (selectedOrg === 'master') return t('master')
  return known?.custom_name || known?.name || name || selectedOrg
}

/** A readable name for a grant target: `*`, `master`, or an organization
 *  the session knows, else the id. */
export function useOrgLabel(): (org: string) => string {
  const { t } = useI18n()
  const { orgs } = useSession()
  return (org: string) => {
    if (org === '*') return t('all organizations')
    if (org === 'master') return t('master')
    const known = orgs.find((o) => o.id === org)
    return known?.custom_name || known?.name || org
  }
}

/** True when the current session's role is at least `min`. */
export function useHasRole(min: Role): boolean {
  const { role } = useSession()
  return ORDER[role] >= ORDER[min]
}
