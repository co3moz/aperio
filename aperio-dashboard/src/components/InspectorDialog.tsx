import { PlayIcon } from '@radix-ui/react-icons'
import { Button, Callout, Dialog, Flex, Text, Tooltip } from '@radix-ui/themes'
import { useEffect, useState } from 'react'
import { api, ApiError, type CapturedRequest } from '../lib/api'
import { decodeBodyPreview, formatHeaders } from '../lib/format'

function Section({ label, content }: { label: string; content: string }) {
  return (
    <Flex direction="column" gap="1">
      <Text size="1" weight="bold" color="gray" style={{ textTransform: 'uppercase', letterSpacing: '1px' }}>
        {label}
      </Text>
      <pre className="inspector-pre">{content}</pre>
    </Flex>
  )
}

/** Detail view for a captured request, with one-click replay. */
export function InspectorDialog({ id, onClose }: { id: string | null; onClose: () => void }) {
  const [detail, setDetail] = useState<CapturedRequest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [replayResult, setReplayResult] = useState<string | null>(null)
  const [replaying, setReplaying] = useState(false)

  useEffect(() => {
    setDetail(null)
    setError(null)
    setReplayResult(null)
    if (!id) return
    api
      .requestDetail(id)
      .then(setDetail)
      .catch((e: unknown) => {
        setError(
          e instanceof ApiError && e.status === 404
            ? 'Detail not available for this request (only recent requests are captured).'
            : `Failed to load request detail: ${e instanceof Error ? e.message : String(e)}`,
        )
      })
  }, [id])

  const replay = async () => {
    if (!detail) return
    setReplaying(true)
    setReplayResult(null)
    try {
      const r = await api.replayRequest(detail.id)
      setReplayResult(`✔ Replayed: status ${r.status} in ${r.duration_ms} ms`)
    } catch (e) {
      setReplayResult(`Replay failed: ${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setReplaying(false)
    }
  }

  return (
    <Dialog.Root
      open={id !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <Dialog.Content maxWidth="860px">
        <Flex justify="between" align="center" gap="3">
          <Dialog.Title mb="0" size="4" style={{ wordBreak: 'break-all' }}>
            {detail
              ? `${detail.method} ${detail.uri} → ${detail.status} (${detail.duration_ms} ms)`
              : 'Request Detail'}
          </Dialog.Title>
          {detail && (
            <Tooltip
              content={
                detail.req_body_truncated
                  ? 'Body truncated at capture; cannot replay'
                  : 'Send this request through the tunnel again'
              }
            >
              <Button
                size="1"
                variant="soft"
                disabled={detail.req_body_truncated}
                loading={replaying}
                onClick={replay}
              >
                <PlayIcon /> Replay
              </Button>
            </Tooltip>
          )}
        </Flex>
        <Dialog.Description size="1" color="gray" mt="1">
          Captured transaction detail — bodies are capped at 64 KB.
        </Dialog.Description>
        <Flex direction="column" gap="3" mt="4">
          {replayResult && (
            <Callout.Root size="1" color={replayResult.startsWith('✔') ? 'green' : 'red'}>
              <Callout.Text>{replayResult}</Callout.Text>
            </Callout.Root>
          )}
          {error && (
            <Callout.Root size="1" color="red">
              <Callout.Text>{error}</Callout.Text>
            </Callout.Root>
          )}
          {detail && (
            <>
              <Section label="Request Headers" content={formatHeaders(detail.req_headers)} />
              <Section
                label="Request Body"
                content={decodeBodyPreview(detail.req_body, detail.req_body_truncated, false)}
              />
              <Section label="Response Headers" content={formatHeaders(detail.resp_headers)} />
              <Section
                label="Response Body"
                content={decodeBodyPreview(
                  detail.resp_body,
                  detail.resp_body_truncated,
                  detail.resp_streamed,
                )}
              />
            </>
          )}
        </Flex>
        <Flex mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Close
            </Button>
          </Dialog.Close>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}
