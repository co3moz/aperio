// A conformant WebSocket echo server, and nothing else.
//
// It is the far end of the tunnel for the Autobahn run: Autobahn drives a
// visitor-side connection, the frames cross the tunnel to here, and whatever
// comes back crosses it again. The point of the exercise is what the relay
// does to them in between, so this side must be beyond suspicion, which is
// why it is `ws` rather than anything written here. `ws` passes the suite
// cleanly on its own, so a failure in the report is a statement about the
// relay and not about the backend.
//
// Echoes verbatim, preserving the text/binary distinction, because Autobahn
// checks that a text frame comes back as text and that its payload is still
// valid UTF-8.
import { createServer } from 'node:http'
import { WebSocketServer } from 'ws'

const port = Number(process.argv[2] ?? 9010)

const http = createServer((_req, res) => {
  // The tunnel's readiness probe travels the same path and needs an answer.
  res.writeHead(200, { 'content-length': '2' }).end('ok')
})

const wss = new WebSocketServer({
  server: http,
  // Autobahn's 9.x cases send megabyte frames; the default limit would
  // reject them and the report would blame the relay for it.
  maxPayload: 64 * 1024 * 1024,
})

wss.on('connection', (socket) => {
  socket.on('message', (data, isBinary) => socket.send(data, { binary: isBinary }))
  socket.on('error', () => socket.terminate())
})

http.listen(port, '127.0.0.1', () => {
  process.stdout.write(`echo backend on ${port}\n`)
})
