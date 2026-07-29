#!/usr/bin/env python3
"""A minimal MQTT 3.1.1 client, for testing the client's MQTT face.

Hand-rolled on purpose. The face encodes with `mqttbytes`; a test that used
the same crate would agree with it about any misreading of the spec. Forty
lines of struct packing is an independent second opinion on the wire format.

    mqtt_probe.py subscribe <host> <port> <filter> <seconds>
    mqtt_probe.py publish   <host> <port> <topic> <payload>
"""
import socket, struct, sys, time


def remaining_length(n):
    out = b""
    while True:
        byte = n % 128
        n //= 128
        out += bytes([byte | (0x80 if n > 0 else 0)])
        if n == 0:
            return out


def read_remaining_length(sock):
    multiplier, value = 1, 0
    while True:
        byte = sock.recv(1)
        if not byte:
            raise EOFError
        value += (byte[0] & 127) * multiplier
        if not byte[0] & 0x80:
            return value
        multiplier *= 128


def string(s):
    raw = s.encode()
    return struct.pack("!H", len(raw)) + raw


def connect(host, port):
    sock = socket.create_connection((host, int(port)), timeout=10)
    payload = string("MQTT") + bytes([4, 0x02]) + struct.pack("!H", 30) + string("probe")
    sock.sendall(bytes([0x10]) + remaining_length(len(payload)) + payload)
    header = sock.recv(1)
    assert header and header[0] >> 4 == 2, f"expected CONNACK, got {header!r}"
    read_remaining_length(sock)
    body = sock.recv(2)
    assert body[1] == 0, f"CONNACK refused: {body!r}"
    return sock


def main():
    mode, host, port = sys.argv[1], sys.argv[2], sys.argv[3]
    sock = connect(host, port)
    if mode == "publish":
        topic, payload = sys.argv[4], sys.argv[5].encode()
        body = string(topic) + payload
        sock.sendall(bytes([0x30]) + remaining_length(len(body)) + body)
        time.sleep(0.5)
        return
    filter_, seconds = sys.argv[4], float(sys.argv[5])
    body = struct.pack("!H", 1) + string(filter_) + bytes([0])
    sock.sendall(bytes([0x82]) + remaining_length(len(body)) + body)
    header = sock.recv(1)
    assert header and header[0] >> 4 == 9, f"expected SUBACK, got {header!r}"
    read_remaining_length(sock)
    granted = sock.recv(3)
    print(f"suback granted={granted[2]}", flush=True)

    deadline = time.time() + seconds
    while time.time() < deadline:
        sock.settimeout(max(0.2, deadline - time.time()))
        try:
            header = sock.recv(1)
        except socket.timeout:
            break
        if not header:
            break
        if header[0] >> 4 != 3:
            read_remaining_length(sock)
            continue
        length = read_remaining_length(sock)
        rest = b""
        while len(rest) < length:
            rest += sock.recv(length - len(rest))
        topic_len = struct.unpack("!H", rest[:2])[0]
        topic = rest[2 : 2 + topic_len].decode()
        print(f"message topic={topic} payload={rest[2 + topic_len:].decode()}", flush=True)


main()
