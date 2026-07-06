import '@radix-ui/themes/styles.css'
import './global.css'
import { Theme } from '@radix-ui/themes'
import type { ReactNode } from 'react'

export function AppTheme({ children }: { children: ReactNode }) {
  return (
    <Theme
      appearance="dark"
      accentColor="indigo"
      grayColor="slate"
      radius="large"
      panelBackground="translucent"
    >
      {children}
    </Theme>
  )
}
