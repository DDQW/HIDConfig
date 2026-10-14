# openrazer-win

Control Razer devices on Windows without Synapse, in the spirit of
[OpenRazer](https://github.com/openrazer/openrazer) on Linux.

- **No kernel driver.** Everything talks to the devices over HID from user mode.
- **Small tray app.** A tray icon and menu for quick changes, plus a settings window.
- **Settings stick.** They are saved to a TOML file and re-applied when a device is plugged in, the PC wakes from sleep, or you edit the file.
- **`razerctl` CLI** for scripting and debugging.

Not affiliated with Razer.

## Supported devices

| Device | USB id | What works | Status |
|---|---|---|---|
| Razer Viper (2019, wired) | `1532:0078` | Live DPI (up to 16,000, separate X/Y), DPI stages 1–5 with active stage, polling rate 125/500/1000 Hz, logo effect (off, static, breathing, spectrum), logo colour and brightness | Tested on hardware |
| Razer Hammerhead V3 HyperSpeed (2.4 GHz dongle) | `1532:0A61` | Battery (left/right), connection state, noise control (off, ANC, ambient), reduce ANC on calls, EQ presets (game, music, movie, flat), microphone on/off, do-not-disturb while gaming, power saving and its timeout, auto-pause, touch gestures, voice prompts | Tested on hardware; protocol reverse-engineered, see [docs](docs/hammerhead-v3-hyperspeed.md) |

Other Razer mice that use the same protocol as OpenRazer's `razermouse` driver can
usually be added with a device-table entry; see [Adding a device](#adding-a-device).

## Install

1. Run `openrazer-win-<version>-setup.exe`. It installs into `C:\Program Files\openrazer-win` and asks for admin rights once.
2. Leave **Start openrazer-win when I sign in** ticked if you want it to start at login. You can change this later in the app.
3. Find the green ring icon in the tray. If it isn't visible, check the hidden icons (^).

Uninstalling (Settings → Apps) stops the tray and removes autostart. Your settings in
`%APPDATA%\openrazer-win\` are kept.

### Synapse

openrazer-win works with Synapse installed, but Synapse's services can fight it:
- Its Chroma service overwrites the logo lighting.
- Its services share the device's reply slot (openrazer-win retries when that happens).

Stop Synapse without uninstalling it, then bring it back whenever you need it:

```powershell
powershell -ExecutionPolicy Bypass -File tools\synapse.ps1 off   # services disabled, app closed, autostart off
powershell -ExecutionPolicy Bypass -File tools\synapse.ps1 on    # restore
```

## Using it

**Tray menu** (right-click):
- **Viper:** DPI presets, DPI stage, polling rate, logo effect, logo colour, logo brightness.
- **Hammerhead:** noise control, EQ, power saving and options, plus a microphone toggle.
- **App:** Settings…, Start with Windows, Open config file, Quit.

The menu headers show live DPI and earbud battery. The icon turns orange when an
earbud is at 20 % or below, and grey when no device is connected.

**Settings window** (left-click the icon): every setting, including the DPI stage editor and custom logo colours.

**Config file:** `%APPDATA%\openrazer-win\config.toml`. Every key is optional; anything missing is left alone on the device.

```toml
[viper]
dpi = 1600                        # live DPI; applied after the stages
dpi_y = 1200                      # optional separate Y
dpi_stages = [400, 800, [1600, 1200], 3200]   # 1-5 stages, number or [x, y]
dpi_stage = 2                     # active stage, 1-based
polling_hz = 1000                 # 125, 500, 1000
brightness = 200                  # 0-255

[viper.logo]
effect = "static"                 # off, static, breathing, spectrum
color = "44d62c"                  # for static / breathing

[earbuds]
noise_control = "anc"             # off, anc, ambient
reduce_anc_on_calls = false
eq = "flat"                       # game, music, movie, flat
dnd_while_gaming = false
power_saving = true
power_saving_minutes = 15         # 1-60
auto_pause = true
gestures = true
audio_prompts = true
```

**CLI** (`razerctl.exe`, next to the tray exe):

```
razerctl info                     # firmware, DPI, stages, polling, brightness
razerctl dpi 1600 [1200]          # live DPI (X [Y])
razerctl stages 400 800 1600x1200 --active 2
razerctl poll 1000
razerctl logo static ff0000 | breathing 00c8ff | spectrum | off
razerctl brightness 200
razerctl earbuds info             # battery, connection, ANC, mic, DND, power saving
razerctl earbuds anc off|anc|ambient
razerctl earbuds eq game|music|movie|flat
razerctl earbuds mic on|off       # also: dnd, power-saving, timeout <min>, auto-pause, gestures, prompts
razerctl save                     # write what the devices report into config.toml
razerctl apply                    # apply config.toml once
razerctl watch                    # what the tray does, in a console
razerctl list | descriptor <pid> | listen <pid> [--guided]   # reverse-engineering helpers
```

Logs: `%APPDATA%\openrazer-win\tray.log`.

## How it works

```
apps/razer-tray      tray icon, menu, Slint settings window; UI thread + device thread
apps/razerctl        CLI
crates/razer-settings  config.toml, apply, Watcher (arrival / resume / config edits, live status)
crates/razer-hid     Windows HID transport (hidapi): device table, Viper + earbuds commands
crates/razer-proto   pure protocol: OpenRazer 90-byte reports, HyperSpeed mailbox frames
```

- **Viper:** OpenRazer's 90-byte feature report on interface 0, with transaction id `0x1f`.
- **Hammerhead dongle:** a 63-byte request/reply mailbox on vendor collection `ff14` (feature report `0x02`), plus pushed events for battery and connection. Both were decoded from USB captures; the format is documented in [docs/hammerhead-v3-hyperspeed.md](docs/hammerhead-v3-hyperspeed.md).

## Build from source

Requirements:
- Rust stable (1.98 or newer) with the MSVC toolchain.
- [Inno Setup 6](https://jrsoftware.org/isinfo.php) for the installer (`winget install JRSoftware.InnoSetup`).

```powershell
cargo test --workspace
cargo build --release                                            # target\release\razer-tray.exe, razerctl.exe
powershell -ExecutionPolicy Bypass -File tools\build-release.ps1 # dist\openrazer-win-<ver>-setup.exe
```

`tools\make_icon.py` regenerates `packaging\openrazer.ico` and `.png`.

## Adding a device

**Mice that use OpenRazer's protocol:**
1. Run `razerctl list` and note the product id and the interface whose usage page is `0001`, usage `0002` (usually interface 0).
2. Look the device up in OpenRazer's `driver/razermouse_driver.c` for its transaction id (`0x1f` or `0x3f` for most recent mice) and its max DPI.
3. Add a `Model` entry to `MODELS` in `crates/razer-hid/src/lib.rs` with the supported `Feature`s.
4. Check with `razerctl info` (read-only first), then try the setters.

**Devices with an unknown protocol** (headsets, dongles): reverse-engineer it the way the Hammerhead was done.
1. Run `razerctl descriptor <pid>` to see the report layout.
2. Run `razerctl listen <pid>` to watch unsolicited events.
3. Record Synapse's traffic with USBPcap while marking each UI action with `tools\mark.ps1`.
4. Turn the recording into a timeline with `tools\decode_capture.py <pcap> <markers>`.
5. Write the protocol up in `docs/` before writing code, and add unit tests that re-encode captured frames byte for byte.

## Credits

The mouse protocol knowledge comes from OpenRazer (`razercommon.h`,
`razerchromacommon.c`, `razermouse_driver.c`).
