# Razer Hammerhead V3 HyperSpeed (dongle PID 0x0A61): protocol notes

Sources:
- Passive captures (`hidconfigctl listen 0a61 [--guided]`) with Synapse uninstalled.
- A USBPcap recording of Synapse changing every setting, timestamped against UI markers (`tools/mark.ps1`) and decoded with `tools/decode_capture.py`.

"Confirmed" means seen in the Synapse capture; "guess" means a hypothesis that still needs testing.

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

## Feature report `0x02` on ff14: request/reply mailbox (confirmed)

The host sends SET_REPORT `0x02`, then GET_REPORT `0x02` returns queued frames.

```
byte  0     1       2     3..5      6     7     8     9..        62
      02    status  seq   00 00 00  len   cmd   sub   data[len]  xor(bytes 6..61)
```

- **status:** `00` in requests, `02` for a successful reply, `05` for an unsolicited push (always with seq `60`).
- **seq:** the host counter. It runs `61..7f` and then wraps back to `61`, so `60` is reserved for pushes. The reply echoes it.
- **Reading:** GET returns the oldest queued frame. Synapse does SET and then GETs twice, because the first GET usually returns the *previous* reply or a pending push. A client must keep reading until `status == 02` and `seq` matches.
- **sub:** a "get" has bit 7 set (`0x80 | id`) and the matching "set" is the bare id, as in OpenRazer. A get request carries the selector bytes; the reply echoes them followed by the value.
- **First data byte:** always `01` in everything Synapse sends. It's probably a target index (guess).

Earlier failed attempts copied a push header (`05 60`) into a request, so the dongle ignored them.

### Commands (all confirmed from the Synapse capture unless marked)

| Get (request → reply) | Set | Meaning |
|---|---|---|
| `07 80` push only → `L R` | | Battery % for left/right. Synapse never sends this get; it arrives as a status-05 push. |
| `07 88 [01]` → `01 en` | `07 08 [01 en]` | Wireless power saving on/off. "On" was seen as `01 01`; "off" sent no SET in the capture because the device was already off. |
| `07 83 []` → `hi lo` | `07 03 [hi lo]` | Power-saving timeout in **seconds**, big-endian: 300/900/1800/2700/3600 = 5/15/30/45/60 min. |
| `18 86 [01]` → `01 mode x` | `18 06 [01 mode x]` | **ANC:** mode `00` off, `01` ANC, `50` ambient. `x`: `0a` normally, `01` when "reduce ANC during Bluetooth calls" is on. |
| `18 9e`? `[01]` | `18 1e [01 v]` | Auto-pause (wear detection): `00` off, `01` on. The get id is assumed. |
| `18 a2`? `[01]` | `18 22 [01 v]` | Touch gestures: `00` off, `01` on. The get id is assumed. |
| `18 85 [01]` → `01 v` | `18 05 [01 v]` | Do-not-disturb during 2.4 GHz gaming: `01` on, `00` off. |
| `18 90`? | `18 10 [01 ff v]` | Audio prompts: `01` on, `00` off. The meaning of `ff` is unknown (maybe "all prompts" or volume). |
| `18 84 [01 02]` → `01 02 v` | `18 04 [01 02 v]` | Microphone mute: `01` = muted (mic off), `00` = live. Push `05 21 01` follows each change. |
| `18 80 [01 01 00 00]`? → `01 01 01 p` | `18 00 [01 01 00 p]` | EQ preset: `01` game, `02` music, `03` movie, `1f` custom. |
| `18 81`? | `18 01 [01 01 b0..b9]` | EQ bands, sent right after the preset. Each value is sign-magnitude dB (`81` = -1). See below. |
| `18 9a [01]` → `01 a b` | | Connection state: `01 03 03` with both buds connected. Synapse reads it after every battery push. |

Mic **volume** never reaches the device. It's the Windows endpoint volume.

EQ band bytes sent with each preset (b0..b9):

| Preset | Bands |
|---|---|
| game | `02 02 03 04 01 81 02 03 03 03` |
| music | `02 02 81 81 81 81 01 03 03 03` |
| movie | `03 03 03 84 84 82 02 03 03 03` |
| custom (flat, as you left it) | `00 00 00 00 00 00 00 00 00 00` |

That might be 10 bands, or fewer bands plus trailing settings, since the last three are always `03` in presets. Not yet determined.

### Pushes (interrupt-IN report `0x05`)
- `05 31 L R`: battery. Synapse responds by reading `18 9a`.
- `05 21 01`: microphone mute state changed.
- `05 38 01 mask`: connection mask.
- `05 11 01`: unknown.

## Hardware verification (2026-10-14, `hidconfigctl earbuds`)

- Every get reads correctly: battery, connection, ANC, mic, DND, power saving, timeout.
- These sets were each changed, read back, and restored: ANC (ambient, off, ANC), DND, mic, timeout 15/5 min, power saving.
- These sets were accepted by the dongle (status 02), but there's no getter to read them back: auto-pause, gestures, prompts, EQ game then flat.

## Requests sent so far (before the Synapse capture)

| When | Frame (from byte 6) | Result |
|---|---|---|
| 2026-10-14 | `00 07 00` (len 0, cmd 07, sub 00) via SET_FEATURE 0x02 | Accepted by the driver. No reply, no events, no visible change. This was a mistake: under the OpenRazer convention the bare id is the *set* form, so it was likely an empty "set" that the dongle ignored. |

| 2026-10-14 | `00 07 80` (len 0), both buds out and connected | One reply 3 ms later: `18 9a 01 03 03`, the "both connected" frame. It was probably already queued, not an answer. No `07 80` reply within 2 s. |
| 2026-10-14 | `02 07 80 00 00` (len 2, same shape as the captured battery reply) | No reply, no events, buffer empty afterwards. |

Conclusion so far: SET_FEATURE `0x02` is accepted by the driver but doesn't produce answers. Feature `0x02` looks like an **outbound notification queue** that the dongle fills on its own (status changes, battery), not a request/response mailbox. Requests probably go through ff13 (output `0x06`, input `0x07`), which is untried.

## Next steps (superseded by the Synapse capture; kept for history)

1. Send a **get** request copying an observed frame: battery, cmd `07`, sub `00`, response sub `80`. Send it as SET_FEATURE `0x02` with the same header and checksum, then read feature `0x02` back. If that doesn't respond, try output `0x06` on ff13.
2. If gets work, sweep the read-only `cmd`/`sub` space with `sub | 0x80` to map out what exists, then figure out settings (EQ, ANC, gaming mode). Setting writes need care, because there's no Synapse capture to compare against.
