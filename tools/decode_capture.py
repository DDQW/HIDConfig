"""Decode a USBPcap capture of the Hammerhead V3 HyperSpeed dongle into a timeline.

Usage: python -I tools/decode_capture.py <capture.pcap> [markers.txt] [--device N]

Shows SET_REPORT requests, GET_REPORT replies (feature 0x02 frames) and interrupt-IN
events, interleaved with markers written by tools/mark.ps1.
"""

import struct
import sys

FRAME_LEN = 64  # report id + 63


def packets(path):
    with open(path, "rb") as f:
        data = f.read()
    off = 24  # pcap global header
    while off + 16 <= len(data):
        ts_sec, ts_usec, incl, _orig = struct.unpack_from("<IIII", data, off)
        off += 16
        yield ts_sec + ts_usec / 1e6, data[off : off + incl]
        off += incl


def parse_usbpcap(pkt):
    hlen, _irp, _status, _func, info, _bus, dev, ep, xfer, dlen = struct.unpack_from(
        "<HQIHBHHBBI", pkt, 0
    )
    stage = pkt[27] if xfer == 2 and hlen > 27 else None
    return dev, ep, xfer, info & 1, stage, pkt[hlen : hlen + dlen]


def frame_str(b):
    """Format a feature-0x02 frame: status, seq, len, cmd, sub, data, crc check."""
    if len(b) < FRAME_LEN or b[0] != 0x02:
        return b.hex(" ")
    status, seq, n, cmd, sub = b[1], b[2], b[6], b[7], b[8]
    body = b[9 : 9 + min(n, 53)]
    crc_ok = b[62] == _xor(b[6:62])
    return (
        f"st={status:02x} seq={seq:02x} len={n:<2} {cmd:02x} {sub:02x} | {body.hex(' ')}"
        + ("" if crc_ok else "  [BAD CRC]")
    )


def _xor(bs):
    x = 0
    for v in bs:
        x ^= v
    return x


def main():
    args = sys.argv[1:]
    device = 2
    if "--device" in args:
        i = args.index("--device")
        device = int(args[i + 1])
        del args[i : i + 2]
    pcap = args[0]
    markers = []
    if len(args) > 1:
        with open(args[1], encoding="utf-8-sig") as f:
            for line in f:
                ms, _, text = line.rstrip("\n").partition("\t")
                markers.append((int(ms) / 1000, text.strip()))

    events = []
    for ts, pkt in packets(pcap):
        dev, ep, xfer, to_host, stage, payload = parse_usbpcap(pkt)
        if dev != device or not payload:
            continue
        if xfer == 2 and not to_host and stage == 0 and len(payload) > 8:
            setup, body = payload[:8], payload[8:]
            # bRequest 0x09 = SET_REPORT
            if setup[1] == 0x09:
                events.append((ts, "SET ", frame_str(body)))
        elif xfer == 2 and to_host and stage == 3:
            if any(payload[1:]):
                events.append((ts, "GET ", frame_str(payload)))
        elif xfer == 1 and to_host:
            events.append((ts, "INT ", payload.hex(" ")))
    for ts, text in markers:
        events.append((ts, "####", text))
    events.sort(key=lambda e: e[0])
    t0 = events[0][0] if events else 0
    for ts, kind, text in events:
        print(f"{ts - t0:8.3f} {kind} {text}")


if __name__ == "__main__":
    main()
