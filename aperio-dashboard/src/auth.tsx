import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { AuthApp } from './AuthApp'
import { AppTheme } from './theme'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AppTheme>
      <AuthApp />
    </AppTheme>
  </StrictMode>,
)
