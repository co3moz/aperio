import '@/index.css'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { AuthApp } from './AuthApp'
import { AppProviders } from './theme'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AppProviders>
      <AuthApp />
    </AppProviders>
  </StrictMode>,
)
