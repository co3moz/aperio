import { request } from 'node:http'
import type { ClientRequest } from 'node:http'

/**
 * An open subscription to the client's local message face.
 *
 * Server-sent events are a stream, so the test needs the lines as they
 * arrive rather than a body at the end. The bash phase does this with
 * `curl -sN --max-time 12 > file &` and then greps the file, which means
 * every assertion is really "has it appeared in the file yet" plus a retry.
 */
export class SseStream {
  readonly events: { event: string; data: string }[] = []
  private req?: ClientRequest
  private buffer = ''

  static async open(faceUrl: string, topic: string): Promise<SseStream> {
    const stream = new SseStream()
    const url = new URL(`/subscribe?topic=${encodeURIComponent(topic)}`, faceUrl)
    await new Promise<void>((resolve, reject) => {
      stream.req = request(
        { hostname: url.hostname, port: url.port, path: `${url.pathname}${url.search}` },
        (res) => {
          res.setEncoding('utf8')
          res.on('data', (chunk: string) => stream.consume(chunk))
          resolve()
        },
      )
      stream.req.on('error', reject)
      stream.req.end()
    })
    return stream
  }

  private consume(chunk: string) {
    this.buffer += chunk
    const blocks = this.buffer.split('\n\n')
    this.buffer = blocks.pop() ?? ''
    for (const block of blocks) {
      let event = ''
      let data = ''
      for (const line of block.split('\n')) {
        if (line.startsWith('event: ')) event = line.slice(7).trim()
        else if (line.startsWith('data: ')) data = line.slice(6).trim()
      }
      if (event) this.events.push({ event, data })
    }
  }

  count(event: string): number {
    return this.events.filter((e) => e.event === event).length
  }

  first(event: string): { event: string; data: string } | undefined {
    return this.events.find((e) => e.event === event)
  }

  close(): void {
    this.req?.destroy()
  }
}
