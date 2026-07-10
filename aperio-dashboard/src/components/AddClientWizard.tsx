import { PlusIcon } from 'lucide-react'
import { useState } from 'react'
import { CopyButton, PreBlock } from './shared'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Spinner } from '@/components/ui/spinner'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { api, ApiError } from '@/lib/api'

const CLIENT_IMAGE = 'ghcr.io/co3moz/aperio-client:latest'
const TOKEN_PLACEHOLDER = '<YOUR_APERIO_TOKEN>'

interface WizardState {
  target: string
  hostname: string
  path: string
  tokenName: string
}

/** Extracts the port when the target is a plain http://localhost:<port>. */
function localPort(target: string): string | null {
  const m = target.trim().match(/^http:\/\/(?:localhost|127\.0\.0\.1):(\d+)\/?$/)
  return m ? m[1] : null
}

function dockerCommand(serverUrl: string, token: string, s: WizardState): string {
  const lines = [
    'docker run -d --name aperio-client --restart unless-stopped --network host \\',
    `  -e APERIO_SERVER_URL=${serverUrl} \\`,
    `  -e APERIO_SERVER_TOKEN=${token} \\`,
    `  -e APERIO_CLIENT_TARGET=${s.target} \\`,
  ]
  if (s.hostname) lines.push(`  -e APERIO_HOSTNAME_BIND=${s.hostname} \\`)
  if (s.path) lines.push(`  -e APERIO_PATH_BIND=${s.path} \\`)
  lines.push(`  ${CLIENT_IMAGE}`)
  return lines.join('\n')
}

function cliCommand(serverUrl: string, token: string, s: WizardState): string {
  const port = localPort(s.target)
  // A local port or any URL works as the positional target.
  const target = port ?? s.target
  // Multi-line with continuations: long single lines wrap unpredictably
  // inside the <pre> block.
  const lines = [
    `aperio-client ${target} \\`,
    `  --server-url ${serverUrl} \\`,
    `  --server-token ${token}`,
  ]
  if (s.hostname) {
    lines[lines.length - 1] += ' \\'
    lines.push(`  --hostname ${s.hostname}`)
  }
  if (s.path) {
    lines[lines.length - 1] += ' \\'
    lines.push(`  --path ${s.path}`)
  }
  return lines.join('\n')
}

function yamlConfig(serverUrl: string, token: string, s: WizardState): string {
  const lines = [
    '# aperio.yaml',
    'server:',
    `  url: ${serverUrl}`,
    `  token: ${token}`,
    `target: ${s.target}`,
  ]
  if (s.hostname) lines.push(`hostname: ${s.hostname}`)
  if (s.path) lines.push(`path: ${s.path}`)
  lines.push('', '# then run:', '#   aperio-client')
  return lines.join('\n')
}

function CommandBlock({ content }: { content: string }) {
  // The copy button floats over the top-right corner of the code block so it
  // never stacks with the dialog's own footer buttons.
  return (
    <div className="relative mt-3">
      <PreBlock className="max-h-80 pr-24">{content}</PreBlock>
      <CopyButton value={content} className="absolute right-2 top-2" />
    </div>
  )
}

/**
 * Copy-paste onboarding wizard: pick a token strategy, describe the local
 * service, and get ready-to-run docker / CLI / aperio.yaml snippets.
 */
export function AddClientWizard() {
  const [open, setOpen] = useState(false)
  const [state, setState] = useState<WizardState>({
    target: 'http://localhost:3000',
    hostname: '',
    path: '',
    tokenName: '',
  })
  const [tokenMode, setTokenMode] = useState<'existing' | 'mint'>('existing')
  const [mintedSecret, setMintedSecret] = useState<string | null>(null)
  const [minting, setMinting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const serverUrl = window.location.origin

  // A minted token is scoped to the binds entered at mint time; invalidate
  // the embedded secret when the scope changes afterwards.
  const set = (key: keyof WizardState) => (e: React.ChangeEvent<HTMLInputElement>) => {
    setState((s) => ({ ...s, [key]: e.target.value }))
    if (key === 'hostname' || key === 'path') setMintedSecret(null)
  }

  const openDialog = (next: boolean) => {
    if (next) {
      setMintedSecret(null)
      setError(null)
    }
    setOpen(next)
  }

  const mint = async () => {
    setMinting(true)
    setError(null)
    try {
      const created = await api.createToken({
        name: state.tokenName.trim() || 'client',
        hostnames: state.hostname ? [state.hostname.trim()] : ['*'],
        paths: state.path ? [state.path.trim()] : ['*'],
        allowed_ips: [],
      })
      setMintedSecret(created.token)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setMinting(false)
    }
  }

  const token = tokenMode === 'mint' && mintedSecret ? mintedSecret : TOKEN_PLACEHOLDER

  const field = (label: string, key: keyof WizardState, placeholder: string) => (
    <div className="grid flex-1 gap-2">
      <Label htmlFor={`wiz-${key}`}>{label}</Label>
      <Input id={`wiz-${key}`} value={state[key]} onChange={set(key)} placeholder={placeholder} />
    </div>
  )

  return (
    <Dialog open={open} onOpenChange={openDialog}>
      <DialogTrigger render={<Button size="sm" variant="outline" />}>
        <PlusIcon /> Connect a new client
      </DialogTrigger>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Add a tunnel client</DialogTitle>
          <DialogDescription>
            Describe the local service; copy a ready-to-run command below.
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          {field('Local target (where your service listens)', 'target', 'http://localhost:3000')}
          <div className="flex gap-3">
            {field('Hostname bind (optional)', 'hostname', 'app.example.com')}
            {field('Path bind (optional)', 'path', '/api')}
          </div>

          <div className="flex flex-col gap-2">
            <Label>Token</Label>
            <ToggleGroup
              variant="outline"
              spacing={0}
              value={[tokenMode]}
              multiple={false}
              onValueChange={(v: string[]) => {
                const next = v[0]
                if (next === 'existing' || next === 'mint') setTokenMode(next)
              }}
            >
              <ToggleGroupItem value="existing">I have a token</ToggleGroupItem>
              <ToggleGroupItem value="mint">Mint a scoped token now</ToggleGroupItem>
            </ToggleGroup>
            {tokenMode === 'existing' ? (
              <p className="text-xs text-muted-foreground">
                The commands below use a <code>{TOKEN_PLACEHOLDER}</code> placeholder — replace it
                with your master token or an existing dynamic token.
              </p>
            ) : mintedSecret ? (
              <p className="rounded-2xl border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-700 dark:text-emerald-400">
                Token minted and embedded below. It is scoped to{' '}
                {state.hostname || state.path
                  ? `${state.hostname || 'any hostname'} / ${state.path || 'any path'}`
                  : 'all binds'}{' '}
                and will not be shown again after this dialog closes.
              </p>
            ) : (
              <div className="flex items-center gap-2">
                <Input
                  value={state.tokenName}
                  onChange={set('tokenName')}
                  placeholder="token name (e.g. staging-box)"
                  className="flex-1"
                />
                <Button onClick={mint} disabled={minting}>
                  {minting && <Spinner />} Mint token
                </Button>
              </div>
            )}
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>

          <Tabs defaultValue="docker">
            <TabsList>
              <TabsTrigger value="docker">Docker</TabsTrigger>
              <TabsTrigger value="cli">CLI</TabsTrigger>
              <TabsTrigger value="yaml">aperio.yaml</TabsTrigger>
            </TabsList>
            <TabsContent value="docker">
              <CommandBlock content={dockerCommand(serverUrl, token, state)} />
            </TabsContent>
            <TabsContent value="cli">
              <CommandBlock content={cliCommand(serverUrl, token, state)} />
            </TabsContent>
            <TabsContent value="yaml">
              <CommandBlock content={yamlConfig(serverUrl, token, state)} />
            </TabsContent>
          </Tabs>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            Close
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
