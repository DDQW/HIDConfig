# HIDConfig

A control centre for your input devices, audio and displays on Windows. It started as a
Synapse replacement for Razer devices, in the spirit of
[OpenRazer](https://github.com/openrazer/openrazer) on Linux, and grew into one place for
the settings Windows scatters across Settings, Control Panel and hidden registry keys.

- **No kernel driver, no services.** A small tray app and a CLI (`hidconfigctl`).
- **Profiles that stick.** Device and Windows settings live in one TOML file. They are
  re-applied at sign-in, after sleep, when a device is plugged in, and when you edit the file.
- **Backup first.** Before HIDConfig changes any Windows setting for the first time, it saves
  the old values, and **Restore previous Windows settings** puts them back.

Not affiliated with Razer, Microsoft or any monitor maker.

## What it covers

| Tab | What you can do |
|---|---|
| **Devices** | Razer Viper: DPI (up to 16,000, separate X/Y), DPI stages, polling rate, logo effect, colour and brightness. Razer Hammerhead V3 HyperSpeed: left/right battery, noise control (off, ANC, ambient), EQ, mic, do-not-disturb, power saving, auto-pause, gestures, voice prompts. |
| **Mouse** | Pointer speed (shown with its real multiplier), *Enhance pointer precision*, an acceleration-curve check and reset, scroll lines/characters, scroll inactive windows, double-click speed, swap buttons. Hidden settings: double-click area, drag threshold, hover time, focus-follows-mouse with raise and delay. Helpers: hide while typing, locate with Ctrl, trails, ClickLock, snap to default button, Mouse Keys. |
| **Keyboard** | Repeat delay and rate, cursor blink, input-language hotkey. Hidden settings: exact repeat timing in ms (via FilterKeys), and turning off the Sticky/Filter/Toggle Keys shortcuts that pop up in games. |
| **Audio** | Every input and output: volume, mute, default device for games and for calls. Per device: **spatial sound** (Windows Sonic, Dolby Atmos, DTS:X), sample rate and bit depth, enhancements, exclusive mode. A per-app mixer, call ducking and mono audio. The tray has an output-device switcher. |
| **Gamepads** | Xbox and other controllers: battery, a live input tester (sticks, triggers, buttons), stick-drift measurement with a dead-zone suggestion, rumble test including the impulse triggers. Xbox Wireless Adapter status and its USB power-saving switch. Game Bar, Game Mode and capture toggles. |
| **Display** | Resolution, refresh rate, scaling, orientation, HDR, **Automatic Color Management**, SDR content brightness, link details (bit depth, colour format), colour profile per monitor. Over **DDC/CI**: brightness, contrast, colour preset, input source, speaker volume, sharpness, RGB gain, colour temperature, menu language, menu-button lock, factory resets, and a read-only scan of every control the monitor answers. Also Auto HDR, VRR, windowed-game optimisations, GPU scheduling, a blue-light filter, software gamma, display sleep, and test patterns for gamma, contrast and black level. |

Mode changes that could leave you with a black screen (resolution, refresh rate,
orientation) revert after 15 seconds unless you confirm them. Every slider has a number
field for exact values.

### Supported Razer devices

| Device | USB id | Status |
|---|---|---|
| Razer Viper (2019, wired) | `1532:0078` | Tested on hardware |
| Razer Hammerhead V3 HyperSpeed (2.4 GHz dongle) | `1532:0A61` | Tested on hardware; protocol reverse-engineered, see [docs](docs/hammerhead-v3-hyperspeed.md) |

Other Razer mice that use OpenRazer's `razermouse` protocol can usually be added with a
device-table entry; see [Adding a device](#adding-a-device). Windows, audio, gamepad and
display features work with any hardware.

## Install

1. Run `HIDConfig-<version>-setup.exe`. It installs into `C:\Program Files\HIDConfig` and asks for admin rights once.
2. Leave **Start HIDConfig when I sign in** ticked if you want it to start at login. You can change this later in the app.
3. Find the green ring icon in the tray. If it isn't visible, check the hidden icons (^). Left-click opens the settings window; right-click opens the menu.

Uninstalling (Settings → Apps) stops the tray and removes autostart. Your settings in
`%APPDATA%\HIDConfig\` are kept.

A few actions need administrator rights and ask for them when you use them: GPU
scheduling, the Xbox adapter's power-saving switch, and installing a colour profile.
Nothing else does.

### Synapse

HIDConfig works alongside Synapse, but Synapse's services can fight it: the Chroma service
overwrites the logo lighting, and both share the device's reply slot (HIDConfig retries).
Stop Synapse without uninstalling it, and bring it back whenever you need it:

```powershell
powershell -ExecutionPolicy Bypass -File tools\synapse.ps1 off   # services disabled, app closed, autostart off
powershell -ExecutionPolicy Bypass -File tools\synapse.ps1 on    # restore
```

## Notes on displays

- **DDC/CI** has to be enabled in the monitor's own menu. Some monitors only start answering after a power cycle or replugging the cable.
- **Picture modes lock settings.** When a mode such as dynamic contrast, ECO or sRGB owns brightness or contrast, the monitor accepts changes but ignores them. HIDConfig notices and tells you.
- Some monitors report the **effective** brightness instead of the menu value. HIDConfig learns the factor from its own writes and shows menu numbers.
- **Colour temperature** is only offered in a monitor's *User* colour preset. Setting it while a fixed preset is active can shift that preset's stored white point on some monitors. **Factory reset → Colour** repairs that.
- **Colour profiles** only affect colour-managed apps (browsers, photo apps) unless **Automatic Color Management** is on. With ACM, Windows converts the whole desktop to the panel's real colour space, which is the accurate way to use a wide-gamut monitor without its own sRGB mode.
- **Find online** looks up a monitor's driver and profile package in the Microsoft Update Catalog (downloaded from Microsoft's servers and unpacked with Windows' own `expand.exe`). **Search the web** opens a search for the maker's download page.

## Config file

`%APPDATA%\HIDConfig\config.toml`. Every key is optional; anything missing is left alone.
The app writes this file for you; editing it by hand works too.

```toml
[viper]
dpi = 1600
dpi_stages = [400, 800, [1600, 1200], 3200]   # 1-5 stages, number or [x, y]
dpi_stage = 2
polling_hz = 1000

[viper.logo]
effect = "static"
color = "44d62c"

[earbuds]
noise_control = "anc"
eq = "flat"

[windows.mouse]
pointer_speed = 10
enhance_precision = false

[windows.keyboard]
custom_repeat = true
custom_repeat_delay_ms = 250
custom_repeat_interval_ms = 25
sticky_keys_shortcut = false

[windows.audio]
default_output = "Speakers (Razer Hammerhead V3 HyperSpeed)"   # applied whenever it's connected
ducking = "do-nothing"

[windows.audio.devices."Speakers (Razer Hammerhead V3 HyperSpeed)"]
spatial = "windows-sonic"
sample_rate = 48000
bit_depth = 24

[windows.gaming]
xbox_button_opens_game_bar = false

[windows.graphics]
auto_hdr = false
warm_filter_kelvin = 6500
display_off_minutes = 10
```

## CLI

`hidconfigctl.exe` is next to the tray exe. Run `hidconfigctl --help` (and `--help` on any
subcommand) for everything; the highlights:

```
hidconfigctl info | dpi | stages | poll | logo | brightness          # Viper
hidconfigctl earbuds info | anc | eq | mic | dnd | timeout ...       # Hammerhead
hidconfigctl win show | set mouse.pointer_speed=10 ... [--save] | restore | curve [--reset]
hidconfigctl audio list | volume | mute | default [--comms] | apps | app | spatial | spatial-set
hidconfigctl audio options | format | enhancements | exclusive
hidconfigctl gamepad list | test | rumble | adapter-power-saving
hidconfigctl display list | mode 1 3840x2160@60 | hdr | acm | scale | sdr | profile
hidconfigctl display set 1 brightness 70 | temp | reset | buttons | scan | find-profile | get-profile | install-profile
hidconfigctl save | apply | watch                                     # config file
hidconfigctl list | descriptor <pid> | listen <pid> [--guided]        # reverse-engineering helpers
```

Logs: `%APPDATA%\HIDConfig\tray.log`.

## How it works

```
apps/hidconfig             tray icon, menu, Slint settings window; UI thread + device thread
apps/hidconfigctl          CLI
crates/hidconfig-settings  config.toml, apply, Watcher (arrival / resume / config edits, live status)
crates/hidconfig-win       Windows: mouse, keyboard, audio, gamepads, displays/DDC-CI, graphics, ICC
crates/hidconfig-hid       Razer HID transport (hidapi): device table, Viper + earbuds commands
crates/hidconfig-proto     pure protocol: OpenRazer 90-byte reports, HyperSpeed mailbox frames
```

Mostly documented Windows APIs: SystemParametersInfo, Core Audio, Windows.Gaming.Input
and XInput, DisplayConfig, the Monitor Configuration API (DDC/CI), WCS colour management,
and the power API. Where Windows has no public API, HIDConfig uses the same interfaces
Windows Settings and well-known tools use, and says so in the code:
- `IPolicyConfig` for the default audio device and device formats;
- DPI scaling and SDR white level through DisplayConfig;
- the DirectX graphics toggles in the registry.

## Build from source

Requirements:
- Rust stable (1.98 or newer) with the MSVC toolchain.
- [Inno Setup 6](https://jrsoftware.org/isinfo.php) for the installer (`winget install JRSoftware.InnoSetup`).

```powershell
cargo test --workspace
cargo build --release                                            # target\release\hidconfig.exe, hidconfigctl.exe
powershell -ExecutionPolicy Bypass -File tools\build-release.ps1 # dist\HIDConfig-<ver>-setup.exe
```

`tools\make_icon.py` regenerates `packaging\hidconfig.ico` and `.png`.

## Adding a device

**Mice that use OpenRazer's protocol:**
1. Run `hidconfigctl list` and note the product id and the interface whose usage page is `0001`, usage `0002` (usually interface 0).
2. Look the device up in OpenRazer's `driver/razermouse_driver.c` for its transaction id (`0x1f` or `0x3f` for most recent mice) and its max DPI.
3. Add a `Model` entry to `MODELS` in `crates/hidconfig-hid/src/lib.rs` with the supported `Feature`s.
4. Check with `hidconfigctl info` (read-only first), then try the setters.

**Devices with an unknown protocol** (headsets, dongles): reverse-engineer it the way the Hammerhead was done.
1. Run `hidconfigctl descriptor <pid>` to see the report layout.
2. Run `hidconfigctl listen <pid>` to watch unsolicited events.
3. Record the vendor app's traffic with USBPcap while marking each UI action with `tools\mark.ps1`.
4. Turn the recording into a timeline with `tools\decode_capture.py <pcap> <markers>`.
5. Write the protocol up in `docs/` before writing code, and add unit tests that re-encode captured frames byte for byte.

## Credits

The Razer mouse protocol knowledge comes from OpenRazer (`razercommon.h`,
`razerchromacommon.c`, `razermouse_driver.c`). No OpenRazer code is used.

## License

GPL-3.0-or-later; see [LICENSE](LICENSE). Copyright (C) 2026 Dominik Wölki.
