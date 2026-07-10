import { createContext, useContext, type ReactNode } from 'react'
import type { Role } from './api'

const ORDER: Record<Role, number> = { viewer: 0, operator: 1, admin: 2 }

interface SessionValue {
  username: string
  role: Role
}

const SessionContext = createContext<SessionValue>({ username: 'aperio', role: 'admin' })

export function SessionProvider({
  username,
  role,
  children,
}: {
  username: string
  role: Role
  children: ReactNode
}) {
  return (
    <SessionContext.Provider value={{ username, role }}>{children}</SessionContext.Provider>
  )
}

export function useSession(): SessionValue {
  return useContext(SessionContext)
}

/** True when the current session's role is at least `min`. */
export function useHasRole(min: Role): boolean {
  const { role } = useSession()
  return ORDER[role] >= ORDER[min]
}
