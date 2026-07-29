import { RadioTowerIcon, SendIcon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { RecordEmpty, RecordFact, RecordList, RecordRow, RecordSkeleton, SectionHeader } from './shared'
import { TintBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { usePoll } from '@/hooks/usePoll'
import { useI18n } from '@/i18n'
import { api, ApiError } from '@/lib/api'

/**
 * Who is listening, and a way to send them something.
 *
 * The listening half is the part that cannot be worked out from outside: a
 * publish that reached nobody looks exactly like one that reached everybody,
 * and the difference is almost always a filter that does not match or a token
 * without the topic. Both are visible here side by side.
 */
export function MessagesSection() {
  const { t } = useI18n()
  const { data: subscribers, refresh } = usePoll(api.subscribers, 5_000)
  const [topic, setTopic] = useState('')
  const [payload, setPayload] = useState('')
  const [atLeastOnce, setAtLeastOnce] = useState(false)
  const [busy, setBusy] = useState(false)

  const send = async () => {
    if (!topic.trim()) return
    setBusy(true)
    try {
      const out = await api.publish({
        topic: topic.trim(),
        payload,
        qos: atLeastOnce ? 1 : 0,
      })
      // The count is the answer, not a formality: "sent" and "sent to nobody"
      // are the two outcomes worth telling apart.
      if (out.clients === 0) {
        toast.warning(t('Published to "{topic}", but nothing is subscribed to it', { topic: out.topic }))
      } else {
        toast.success(
          t('Published to "{topic}" — {count} client(s)', { topic: out.topic, count: out.clients }),
        )
      }
      refresh()
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="flex flex-col gap-6">
      <div className="flex flex-col gap-3">
        <SectionHeader
          title={t('Subscribers')}
          description={t('Client processes of this organization listening for messages, and the topic filters they asked for. A client running several services counts once.')}
        />
        <RecordList>
          {subscribers === null ? (
            <RecordSkeleton rows={2} />
          ) : subscribers.length === 0 ? (
            <RecordEmpty icon={<RadioTowerIcon />}>
              {t('Nothing is subscribed. A client subscribes with subscribe: in its config, or by attaching to its local message face.')}
            </RecordEmpty>
          ) : (
            subscribers.map((s) => (
              <RecordRow
                key={s.instance_group ?? s.service ?? Math.random()}
                title={
                  <>
                    {s.service ?? s.instance_group ?? t('unnamed client')}
                    {s.connections > 1 && (
                      <TintBadge tint="gray">
                        {t('{count} connection(s)', { count: s.connections })}
                      </TintBadge>
                    )}
                  </>
                }
              >
                {s.token_name && <RecordFact>{s.token_name}</RecordFact>}
                {s.topics.map((topic) => (
                  <RecordFact key={topic} className="font-mono">
                    {topic}
                  </RecordFact>
                ))}
              </RecordRow>
            ))
          )}
        </RecordList>
      </div>

      <div className="flex flex-col gap-3">
        <SectionHeader
          title={t('Publish')}
          description={t('Sends a message to this organization. The reply says how many client processes it reached, which is the quickest way to tell a wrong filter from a wrong topic.')}
        />
        <div className="flex flex-col gap-4 rounded-3xl border p-4">
          <div className="grid gap-2">
            <Label htmlFor="publish-topic">{t('Topic')}</Label>
            <Input
              id="publish-topic"
              value={topic}
              onChange={(e) => setTopic(e.target.value)}
              placeholder="deploy/web"
              autoComplete="off"
            />
            <p className="text-xs text-muted-foreground">
              {t('A topic, not a filter: `+` and `#` belong to subscriptions.')}
            </p>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="publish-payload">{t('Message')}</Label>
            <Textarea
              id="publish-payload"
              value={payload}
              onChange={(e) => setPayload(e.target.value)}
              rows={3}
              className="font-mono text-xs"
            />
          </div>
          <label className="flex items-center justify-between gap-3 rounded-3xl border px-4 py-3">
            <span className="flex flex-col gap-0.5">
              <span className="text-sm font-medium">{t('At least once')}</span>
              <span className="text-xs text-muted-foreground">
                {t('Held until each subscriber acknowledges it, and resent meanwhile. Nothing is stored for a client that is offline.')}
              </span>
            </span>
            <Switch checked={atLeastOnce} onCheckedChange={setAtLeastOnce} />
          </label>
          <div>
            <Button onClick={send} disabled={busy || !topic.trim()}>
              <SendIcon /> {t('Publish')}
            </Button>
          </div>
        </div>
      </div>
    </section>
  )
}
