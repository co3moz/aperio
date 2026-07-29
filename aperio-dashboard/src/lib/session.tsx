import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import { useI18n } from '@/i18n'
import { api, type Role } from './api'

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
  /** The built-in super-admin, the only session allowed to list organizations. */
  masterAdmin: boolean
}

const SessionContext = createContext<SessionValue>({
  username: 'aperio',
  role: 'admin',
  selectedOrg: 'master',
  masterAdmin: false,
})

export function SessionProvider({
  username,
  role,
  selectedOrg,
  masterAdmin,
  children,
}: {
  username: string
  role: Role
  selectedOrg: string
  masterAdmin: boolean
  children: ReactNode
}) {
  return (
    <SessionContext.Provider value={{ username, role, selectedOrg, masterAdmin }}>
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
 * Listing organizations is the super-admin's privilege, so only they can be
 * given the name; everyone else gets the id they are scoped to, which is the
 * only other thing that identifies it. The implicit master org has no record
 * to look up either way.
 */
export function useOrgName(): string {
  const { t } = useI18n()
  const { selectedOrg, masterAdmin } = useSession()
  const [name, setName] = useState<string | null>(null)

  useEffect(() => {
    // Never issue the request as a non-super-admin: it is a guaranteed 403.
    // Fetched once rather than polled, an org is renamed about as often as it
    // is created, and switching into one reloads the dashboard anyway.
    if (!masterAdmin || selectedOrg === 'master') return
    let live = true
    api
      .orgs()
      .then((orgs) => {
        if (live) setName(orgs.find((o) => o.id === selectedOrg)?.name ?? null)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [masterAdmin, selectedOrg])

  if (selectedOrg === 'master') return t('master')
  return name ?? selectedOrg
}

/** True when the current session's role is at least `min`. */
export function useHasRole(min: Role): boolean {
  const { role } = useSession()
  return ORDER[role] >= ORDER[min]
}
