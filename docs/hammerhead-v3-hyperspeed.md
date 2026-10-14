# Razer Hammerhead V3 HyperSpeed (dongle PID 0x0A61): protocol notes

Status: **observed only.** No request has been sent to the device yet. Everything here comes from the
HID report descriptor plus passive captures (`razerctl listen 0a61 [--guided]`) with Synapse uninstalled.
"Confirmed" means it matched several captures; "guess" means a hypothesis that still needs testing.

## HID collections (interface MI_03)

| Usage page | Reports | Role |
|---|---|---|
| `000c` Consumer | in `0x0c`, 1 byte bitmask | media keys from the earbuds |
| `ff13` vendor | out `0x06` 61 B, in `0x07` 61 B | probably a request/response channel (unused so far) |
| `000b` Telephony | in/out `0x41` | mute and hook switch (standard) |
| `ff14` vendor | in `0x05` 15 B, feature `0x02` 63 B | event pushes plus a message buffer |

The OpenRazer 90-byte feature report does **not** apply. The device rejects it.

## Consumer report `0x0c` (confirmed from the descriptor)

Bit order follows the descriptor usages: bit0 Vol+ (`e9`), bit1 Vol- (`ea`), bit2 Mute (`e2`),
bit3 Play/Pause (`cd`), bit4 Scan next (`b5`), bit5 Scan prev (`b6`), bit6 Play (`b0`), bit7 Pause (`b1`).
Putting the left earbud in the ear produced `0c 08` then `0c 00`, a Play/Pause press and release.
That's probably wear detection auto-resuming playback.

## Input report `0x05` on ff14: events (`05 <event> <data...>`)

| Event | Data | Meaning |
|---|---|---|
| `0x31` | `L R` | **Battery %** for the left and right earbuds. `0xff` means not connected. Confirmed: `63 ff` then `63 63` as the second bud connected. |
| `0x38` | `01 <mask> [<x>]` | **Connection mask** (guess: bit0 left, bit1 right). `00` happened in the case with the lid closed, `01` meant one bud was connected, `03` meant both. The third byte is unknown (seen as `03` once). |
| `0x11` | `01` | Unknown. Fired when the right earbud left the case. |

No events were seen for in-ear on or off (apart from the Play/Pause above), or for putting the buds back in the case or closing the lid. The disconnect may come a few seconds later, after the capture ended.

## Feature report `0x02` on ff14: message buffer

Layout (offsets include the report id byte):

```
0     1  2     3..5      6     7     8     9..     62
02    05 60    00 00 00  len   cmd   sub   data    xor
```

- **`xor` at byte 62 = XOR of bytes 6..61.** This held for all 6 distinct frames we captured.
- `len` = number of `data` bytes after `cmd sub`. This held for every frame, assuming `18 9f` carries `xx 00`.
- Every observed `sub` has the high bit set. In OpenRazer's convention that marks a "get" response (`0x80 | id`).
- Reads sometimes return all zeros. The buffer appears to be consumed by the read, or empty when nothing is pending.
- `05 60` is constant. It might be a target or channel id; that's a guess.

Observed frames:

| cmd sub | data | Interpretation |
|---|---|---|
| `07 80` | `63 63` | **Battery L/R = 99 %.** It matches event `0x31`. |
| `18 9f` | `00`/`01` (+`00`) | Some boolean that toggles with case/wear state. Unknown which. |
| `18 84` | `01 02 01` | Unknown. |
| `18 9a` | `01 03 03` | Matches the `0x38` event payload `01 03 03`, so connection state. |

It's unclear who issues the requests these frames answer, since Synapse is gone. Either the dongle polls the buds itself, or something else on the system queries it.

## Next steps

1. Send a **get** request copying an observed frame: battery, cmd `07`, sub `00`, response sub `80`. Send it as SET_FEATURE `0x02` with the same header and checksum, then read feature `0x02` back. If that doesn't respond, try output `0x06` on ff13.
2. If gets work, sweep the read-only `cmd`/`sub` space with `sub | 0x80` to map out what exists, then figure out settings (EQ, ANC, gaming mode). Setting writes need care, because there's no Synapse capture to compare against.
