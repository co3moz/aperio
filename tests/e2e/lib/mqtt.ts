import { connect, type Socket } from 'node:net'

/**
 * A minimal MQTT 3.1.1 client, hand-rolled on purpose.
 *
 * The client's MQTT face encodes with the `mqttbytes` crate, so a probe built
 * on the same crate would agree with it about any misreading of the spec.
 * This is the independent second opinion, which is the same reason the bash
 * suite carries its own probe rather than importing a library.
 */
export class MqttClient {
  private socket!: Socket
  private buffer = Buffer.alloc(0)
  readonly messages: { topic: string; payload: string }[] = []
  suback = false

  static async connect(port: number, clientId: string): Promise<MqttClient> {
    const mqtt = new MqttClient()
    await new Promise<void>((resolve, reject) => {
      mqtt.socket = connect(port, '127.0.0.1', () => {
        mqtt.socket.write(connectPacket(clientId))
        resolve()
      })
      mqtt.socket.on('error', reject)
      mqtt.socket.on('data', (chunk: Buffer) => mqtt.consume(chunk))
    })
    return mqtt
  }

  private consume(chunk: Buffer) {
    this.buffer = Buffer.concat([this.buffer, chunk])
    for (;;) {
      if (this.buffer.length < 2) return
      const { length, headerBytes } = decodeLength(this.buffer, 1)
      if (length < 0) return
      const total = 1 + headerBytes + length
      if (this.buffer.length < total) return
      const type = this.buffer[0] >> 4
      const body = this.buffer.subarray(1 + headerBytes, total)
      this.buffer = this.buffer.subarray(total)
      if (type === 9) this.suback = true // SUBACK
      if (type === 3) {
        // PUBLISH: topic length, topic, then the payload (QoS 0, no packet id)
        const topicLen = body.readUInt16BE(0)
        const topic = body.subarray(2, 2 + topicLen).toString()
        const payload = body.subarray(2 + topicLen).toString()
        this.messages.push({ topic, payload })
      }
    }
  }

  subscribe(filter: string): void {
    const topic = Buffer.from(filter)
    const body = Buffer.concat([
      u16(1), // packet id
      u16(topic.length),
      topic,
      Buffer.from([0]), // requested QoS
    ])
    this.socket.write(Buffer.concat([Buffer.from([0x82]), encodeLength(body.length), body]))
  }

  publish(topicName: string, payload: string): void {
    const topic = Buffer.from(topicName)
    const body = Buffer.concat([u16(topic.length), topic, Buffer.from(payload)])
    this.socket.write(Buffer.concat([Buffer.from([0x30]), encodeLength(body.length), body]))
  }

  close(): void {
    this.socket.destroy()
  }
}

function u16(n: number): Buffer {
  const b = Buffer.alloc(2)
  b.writeUInt16BE(n)
  return b
}

function encodeLength(n: number): Buffer {
  const out: number[] = []
  let value = n
  do {
    let byte = value % 128
    value = Math.floor(value / 128)
    if (value > 0) byte |= 0x80
    out.push(byte)
  } while (value > 0)
  return Buffer.from(out)
}

function decodeLength(buf: Buffer, at: number): { length: number; headerBytes: number } {
  let multiplier = 1
  let value = 0
  let bytes = 0
  for (;;) {
    if (at + bytes >= buf.length) return { length: -1, headerBytes: 0 }
    const byte = buf[at + bytes]
    bytes += 1
    value += (byte & 127) * multiplier
    if ((byte & 128) === 0) return { length: value, headerBytes: bytes }
    multiplier *= 128
    if (multiplier > 128 ** 3) return { length: -1, headerBytes: 0 }
  }
}

function connectPacket(clientId: string): Buffer {
  const id = Buffer.from(clientId)
  const body = Buffer.concat([
    u16(4),
    Buffer.from('MQTT'),
    Buffer.from([4]), // protocol level 3.1.1
    Buffer.from([0x02]), // clean session
    u16(60), // keep-alive
    u16(id.length),
    id,
  ])
  return Buffer.concat([Buffer.from([0x10]), encodeLength(body.length), body])
}
