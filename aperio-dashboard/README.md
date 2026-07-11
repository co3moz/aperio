# aperio-dashboard

The admin dashboard: a React 19 + TypeScript + Vite SPA served by
`aperio-server` at `/aperio`. Built on shadcn/ui (Base UI primitives),
Tailwind CSS v4, Recharts, and cmdk; internationalized in 8 languages.

In release builds the compiled `dist/` is embedded into the server binary by
`aperio-server/build.rs` (rust-embed). Debug builds of the server read
`dist/` from disk at runtime, so an `npm run build` here is picked up without
recompiling the server.

## Development

```bash
npm install
npm run dev      # hot reload; proxies API calls to a local server on :8080
npm run build    # production bundle into dist/
npm run lint     # oxlint
```

Start a local `aperio-server` (debug build, port 8080) next to `npm run dev`
so API calls have something to talk to. See
[docs/development.md](../docs/development.md).

## Source map

| Path | Purpose |
| --- | --- |
| `src/main.tsx`, `src/App.tsx` | Dashboard entry: router, sidebar layout, page shell |
| `src/auth.tsx`, `src/AuthApp.tsx` | Login page (`auth.html` is its separate Vite entry) |
| `src/components/` | One file per dashboard section (see below) plus shared pieces |
| `src/components/ui/` | Generated shadcn/ui primitives — customize via the shadcn workflow, avoid hand-editing |
| `src/hooks/` | `useLiveData` (SSE `/api/stream`), `usePoll`, `use-mobile` |
| `src/lib/api.ts` | Typed fetch wrapper for the server's REST API |
| `src/lib/session.tsx` | Session context: username, role, sign-out |
| `src/lib/format.ts`, `url.ts`, `utils.ts` | Formatting and URL-state helpers |
| `src/i18n/` | Translations (English source strings live inline; `de`, `es`, `fr`, `ja`, `ru`, `tr`, `zh` override) |
| `src/theme.tsx` | Hand-rolled light/dark theme toggle (no inline scripts — CSP is `self`-only) |

Sections in `src/components/`: `ClientsSection`, `TrafficSection`,
`TrafficBreakdownSection`, `ActivityChart`, `StatsCards`, `TokensSection`,
`ShareLinksSection`, `MaintenanceSection`, `SettingsSection`,
`WebhooksSection`, `UsersSection`, `AuditSection`, `InspectorDialog`,
`AddClientWizard`, `CommandPalette`, `AppSidebar`.

## Conventions

- Every user-facing string goes through the i18n layer; add the English
  source string and translations for all 8 languages.
- Data flows: live data via the SSE stream (`useLiveData`), the rest via
  `lib/api.ts`; filter state belongs in the URL (`lib/url.ts`).
- The dashboard ships self-hosted fonts and no third-party requests — keep
  the CSP `self`-only (no CDN imports, no inline scripts).
