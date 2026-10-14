use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use hidconfig_hid::{Device, RAZER_VID, hidapi::HidApi};
use hidconfig_proto::extended::Effect;

#[derive(Parser)]
#[command(about = "Control Razer devices without Synapse")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List every Razer HID collection (supported or not).
    List,
    /// Dump raw HID report descriptors for a product id (hex), for reverse engineering.
    Descriptor { pid: String },
    /// Read-only: GET every feature report id and print input reports for a while.
    Listen {
        pid: String,
        #[arg(long, default_value_t = 15)]
        secs: u64,
        /// Walk through earbud actions; press Enter after each to mark the log.
        #[arg(long)]
        guided: bool,
    },
    /// Hammerhead V3 HyperSpeed earbuds.
    Earbuds {
        #[command(subcommand)]
        cmd: EarbudsCmd,
    },
    /// Show firmware and current settings of supported devices.
    Info,
    /// Print the config file path and contents.
    Config,
    /// Save what the devices currently report into the config file.
    Save,
    /// Apply the config file once.
    Apply,
    /// Keep running: re-apply the config on device arrival, resume, or config edits.
    Watch,
    /// Displays: Windows display settings and monitor controls (DDC/CI).
    Display {
        #[command(subcommand)]
        cmd: DisplayCmd,
    },
    /// Game controllers and the Xbox Wireless Adapter.
    Gamepad {
        #[command(subcommand)]
        cmd: PadCmd,
    },
    /// Windows audio devices and app volumes.
    Audio {
        #[command(subcommand)]
        cmd: AudioCmd,
    },
    /// Windows mouse and keyboard settings.
    Win {
        #[command(subcommand)]
        cmd: WinCmd,
    },
    /// Set DPI (one value for both axes, or X and Y).
    Dpi { x: u16, y: Option<u16> },
    /// Set DPI stages, e.g. `stages 400 800 1600x1200 --active 2` (1-5 stages).
    Stages {
        /// Each stage as DPI or XxY.
        #[arg(required = true, num_args = 1..=5)]
        dpi: Vec<String>,
        /// Active stage, 1-based.
        #[arg(long, default_value_t = 1)]
        active: u8,
    },
    /// Set polling rate in Hz (125, 500, 1000).
    Poll { hz: u16 },
    /// Set the logo LED effect.
    Logo {
        effect: EffectArg,
        /// Colour as RRGGBB (static/breathing).
        color: Option<String>,
    },
    /// Set logo brightness 0-255.
    Brightness { value: u8 },
}

#[derive(Clone, Copy, ValueEnum)]
enum EffectArg {
    Off,
    Static,
    Breathing,
    Spectrum,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let api = HidApi::new()?;
    match &cli.cmd {
        Cmd::List => return list(&api),
        Cmd::Descriptor { pid } => return descriptor(&api, pid),
        Cmd::Listen { pid, secs, guided } => return listen(&api, pid, *secs, *guided),
        Cmd::Earbuds { cmd } => return earbuds(&api, cmd),
        Cmd::Config => return show_config(),
        Cmd::Save => return save(&api),
        Cmd::Apply => return apply(&api),
        Cmd::Watch => return watch(),
        Cmd::Win { cmd } => return win(cmd),
        Cmd::Audio { cmd } => return audio(cmd),
        Cmd::Gamepad { cmd } => return gamepad(cmd),
        Cmd::Display { cmd } => return display(cmd),
        _ => {}
    }
    let devices: Vec<Device> = hidconfig_hid::open_all(&api)
        .into_iter()
        .collect::<Result<_>>()?;
    if devices.is_empty() {
        bail!("no supported Razer device found");
    }
    for dev in &devices {
        run(dev, &cli.cmd).with_context(|| dev.model.name)?;
    }
    Ok(())
}

fn run(dev: &Device, cmd: &Cmd) -> Result<()> {
    match *cmd {
        Cmd::List
        | Cmd::Descriptor { .. }
        | Cmd::Listen { .. }
        | Cmd::Earbuds { .. }
        | Cmd::Config
        | Cmd::Save
        | Cmd::Apply
        | Cmd::Watch
        | Cmd::Win { .. }
        | Cmd::Audio { .. }
        | Cmd::Gamepad { .. }
        | Cmd::Display { .. } => unreachable!(),
        Cmd::Info => {
            let (maj, min) = dev.firmware()?;
            let (x, y) = dev.dpi()?;
            println!("{} (firmware v{maj}.{min})", dev.model.name);
            println!("  dpi:        {x} x {y}");
            println!("  polling:    {} Hz", dev.polling_rate()?);
            println!("  brightness: {}", dev.logo_brightness()?);
            let st = dev.dpi_stages()?;
            let list: Vec<String> = st
                .stages
                .iter()
                .enumerate()
                .map(|(i, &(x, y))| {
                    let mark = if i + 1 == st.active as usize { "*" } else { "" };
                    if x == y {
                        format!("{mark}{x}")
                    } else {
                        format!("{mark}{x}x{y}")
                    }
                })
                .collect();
            println!("  dpi stages: {}  (* = active)", list.join(" "));
        }
        Cmd::Dpi { x, y } => {
            dev.set_dpi(x, y.unwrap_or(x))?;
            let (rx, ry) = dev.dpi()?;
            println!("{}: dpi now {rx} x {ry}", dev.model.name);
        }
        Cmd::Stages { ref dpi, active } => {
            let stages = dpi
                .iter()
                .map(|s| parse_xy(s))
                .collect::<Result<Vec<_>>>()?;
            dev.set_dpi_stages(&hidconfig_proto::mouse::DpiStages { active, stages })?;
            let st = dev.dpi_stages()?;
            println!(
                "{}: stages now {:?}, active {}",
                dev.model.name, st.stages, st.active
            );
            let (x, y) = dev.dpi()?;
            println!("{}: current dpi {x} x {y}", dev.model.name);
        }
        Cmd::Poll { hz } => {
            dev.set_polling_rate(hz)?;
            println!("{}: polling now {} Hz", dev.model.name, dev.polling_rate()?);
        }
        Cmd::Logo { effect, ref color } => {
            let rgb = || parse_rgb(color.as_deref().context("this effect needs a colour")?);
            let effect = match effect {
                EffectArg::Off => Effect::Off,
                EffectArg::Static => Effect::Static(rgb()?),
                EffectArg::Breathing => Effect::Breathing(rgb()?),
                EffectArg::Spectrum => Effect::Spectrum,
            };
            dev.set_logo_effect(effect)?;
            println!("{}: logo set", dev.model.name);
        }
        Cmd::Brightness { value } => {
            dev.set_logo_brightness(value)?;
            println!(
                "{}: brightness now {}",
                dev.model.name,
                dev.logo_brightness()?
            );
        }
    }
    Ok(())
}

use hidconfig_settings::parse_rgb;

/// `1600` or `1600x1200`.
fn parse_xy(s: &str) -> Result<(u16, u16)> {
    let (x, y) = s.split_once(['x', 'X']).unwrap_or((s, s));
    Ok((x.parse().context("bad DPI")?, y.parse().context("bad DPI")?))
}

fn show_config() -> Result<()> {
    let p = hidconfig_settings::path()?;
    println!("# {}", p.display());
    match std::fs::read_to_string(&p) {
        Ok(s) => print!("{s}"),
        Err(_) => println!("# (no config yet; run `hidconfigctl save`)"),
    }
    Ok(())
}

fn save(api: &HidApi) -> Result<()> {
    let base = hidconfig_settings::Config::load()?;
    let cfg = hidconfig_settings::snapshot(api, &base)?;
    let p = cfg.save()?;
    println!("saved to {}", p.display());
    print!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}

fn apply(api: &HidApi) -> Result<()> {
    let cfg = hidconfig_settings::Config::load()?;
    for dev in hidconfig_hid::open_all(api) {
        let dev = dev?;
        hidconfig_settings::apply_viper(&dev, &cfg.viper).with_context(|| dev.model.name)?;
        println!("{}: applied", dev.model.name);
    }
    if let Some(e) = hidconfig_hid::earbuds::Earbuds::open(api)? {
        hidconfig_settings::apply_earbuds(&e, &cfg.earbuds).context("earbuds")?;
        println!("Hammerhead V3 HyperSpeed: applied");
    }
    Ok(())
}

#[derive(Subcommand)]
enum WinCmd {
    /// Print current Windows values (as config TOML).
    Show,
    /// Apply settings now, e.g. `win set mouse.pointer_speed=10 keyboard.repeat_rate=31`.
    Set {
        #[arg(required = true)]
        assignments: Vec<String>,
        /// Also store them in config.toml so they are re-applied.
        #[arg(long)]
        save: bool,
    },
    /// Put back the values saved before HIDConfig first changed anything.
    Restore,
    /// Report whether the mouse acceleration curve is the Windows default.
    Curve {
        /// Reset it to the default (takes effect after sign-out).
        #[arg(long)]
        reset: bool,
    },
}

fn parse_assignments(items: &[String]) -> Result<hidconfig_win::WindowsSettings> {
    let mut s = hidconfig_win::WindowsSettings::default();
    for item in items {
        let (key, value) = item.split_once('=').context("expected section.key=value")?;
        s.set_field(key, value)?;
    }
    Ok(s)
}

fn win(cmd: &WinCmd) -> Result<()> {
    match cmd {
        WinCmd::Show => print!("{}", toml::to_string_pretty(&hidconfig_win::read()?)?),
        WinCmd::Set { assignments, save } => {
            let s = parse_assignments(assignments)?;
            hidconfig_settings::apply_windows(&s)?;
            if *save {
                let mut cfg = hidconfig_settings::Config::load()?;
                cfg.windows.merge(&s)?;
                cfg.save()?;
            }
            let now = hidconfig_win::read()?;
            let now_t = toml::Table::try_from(&now)?;
            for (sec, v) in toml::Table::try_from(&s)? {
                if let toml::Value::Table(fields) = v {
                    for k in fields.keys() {
                        println!("{sec}.{k} = {}", now_t[&sec][k]);
                    }
                }
            }
        }
        WinCmd::Restore => {
            let path = hidconfig_settings::windows_backup_path()?;
            hidconfig_win::restore(&path)?;
            println!("restored from {}", path.display());
        }
        WinCmd::Curve { reset } => {
            if *reset {
                hidconfig_win::mouse::reset_curve()?;
                println!("default curve written; sign out and back in to use it");
            }
            println!(
                "acceleration curve is default: {}",
                hidconfig_win::mouse::curve_is_default()
            );
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum DisplayCmd {
    /// Displays with mode, HDR, scaling and monitor (DDC/CI) values.
    List,
    /// Set resolution/refresh, e.g. `mode 1 3840x2160@60` (display number or \\.\DISPLAYn).
    Mode { display: String, mode: String },
    /// HDR on/off.
    Hdr { display: String, state: OnOff },
    /// Scaling in percent (100, 125, 150 ...).
    Scale { display: String, percent: u32 },
    /// Set a monitor control: brightness, contrast, volume, input, preset, sharpness, or a hex VCP code.
    Set {
        display: String,
        control: String,
        value: String,
    },
}

fn display_device(arg: &str) -> String {
    if arg.chars().all(|c| c.is_ascii_digit()) {
        format!(r"\\.\DISPLAY{arg}")
    } else {
        arg.to_string()
    }
}

fn display(cmd: &DisplayCmd) -> Result<()> {
    use hidconfig_win::display::{self as d, vcp};
    match cmd {
        DisplayCmd::List => {
            for x in d::displays(true)? {
                println!(
                    "{} {}{}",
                    x.device,
                    x.name,
                    if x.primary { " (primary)" } else { "" }
                );
                println!(
                    "  mode:     {}  rotation {}°  ({} modes)",
                    x.mode,
                    x.rotation,
                    x.modes.len()
                );
                println!(
                    "  HDR:      {}",
                    if !x.hdr_supported {
                        "not supported".into()
                    } else if x.hdr_on {
                        "on".to_string()
                    } else {
                        "off".into()
                    }
                );
                println!(
                    "  scaling:  {}  options {:?}",
                    x.scale.map_or("?".into(), |s| format!("{s}%")),
                    x.scales
                );
                match &x.ddc {
                    None => println!("  DDC/CI:   no reply"),
                    Some(ddc) => {
                        for (code, (cur, max)) in &ddc.values {
                            let label = match *code {
                                vcp::INPUT => format!("{} ({cur})", d::input_name(*cur as u8)),
                                vcp::COLOR_PRESET => {
                                    format!("{} ({cur})", d::preset_name(*cur as u8))
                                }
                                _ => format!("{cur} / {max}"),
                            };
                            println!("  VCP {code:02x}:   {label}");
                        }
                        if let Some(inputs) = ddc.caps.get(&vcp::INPUT) {
                            println!(
                                "  inputs:   {}",
                                inputs
                                    .iter()
                                    .map(|v| d::input_name(*v))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                    }
                }
            }
        }
        DisplayCmd::Mode { display, mode } => {
            let (res, hz) = mode.split_once('@').context("format: WIDTHxHEIGHT@HZ")?;
            let (w, h) = res
                .split_once(['x', 'X'])
                .context("format: WIDTHxHEIGHT@HZ")?;
            let m = d::Mode {
                width: w.parse()?,
                height: h.parse()?,
                hz: hz.parse()?,
            };
            d::set_mode(&display_device(display), m, None)?;
            println!("mode set to {m}");
        }
        DisplayCmd::Hdr { display, state } => {
            d::set_hdr(&display_device(display), state.on())?;
            println!("HDR {}", if state.on() { "on" } else { "off" });
        }
        DisplayCmd::Scale { display, percent } => {
            d::set_scale(&display_device(display), *percent)?;
            println!("scaling {percent}%");
        }
        DisplayCmd::Set {
            display,
            control,
            value,
        } => {
            let code = match control.as_str() {
                "brightness" => vcp::BRIGHTNESS,
                "contrast" => vcp::CONTRAST,
                "volume" => vcp::VOLUME,
                "input" => vcp::INPUT,
                "preset" => vcp::COLOR_PRESET,
                "sharpness" => vcp::SHARPNESS,
                hex => u8::from_str_radix(hex.trim_start_matches("0x"), 16)
                    .context("unknown control")?,
            };
            let v = if let Some(h) = value.strip_prefix("0x") {
                u32::from_str_radix(h, 16)?
            } else {
                value.parse()?
            };
            d::set_vcp(&display_device(display), code, v)?;
            println!("VCP {code:02x} = {v}");
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum PadCmd {
    /// Connected controllers, adapter and gaming toggles.
    List,
    /// Print live input of a controller for a few seconds.
    Test {
        #[arg(default_value_t = 0)]
        index: u32,
        #[arg(long, default_value_t = 5)]
        secs: u64,
    },
    /// Allow/forbid Windows to power down the Xbox Wireless Adapter (needs admin;
    /// restarts the adapter).
    AdapterPowerSaving { state: OnOff },
    /// Rumble a controller briefly (strengths 0-100).
    Rumble {
        #[arg(default_value_t = 0)]
        index: u32,
        #[arg(long, default_value_t = 50.0)]
        left: f64,
        #[arg(long, default_value_t = 50.0)]
        right: f64,
        #[arg(long, default_value_t = 0.0)]
        left_trigger: f64,
        #[arg(long, default_value_t = 0.0)]
        right_trigger: f64,
    },
}

fn gamepad(cmd: &PadCmd) -> Result<()> {
    use hidconfig_win::gamepad as gp;
    use std::{thread::sleep, time::Duration};
    // Enumeration is asynchronous: give Windows.Gaming.Input a moment.
    let _ = gp::pads();
    sleep(Duration::from_millis(800));
    match cmd {
        PadCmd::List => {
            let pads = gp::pads()?;
            if pads.is_empty() {
                println!("no controllers");
            }
            for p in pads {
                println!(
                    "#{} {} ({:04x}:{:04x}) {} battery {}{}",
                    p.index,
                    p.name,
                    p.vid,
                    p.pid,
                    if p.wireless { "wireless" } else { "wired" },
                    p.battery.map_or("n/a".into(), |b| format!("{b}%")),
                    if p.charging { " (charging)" } else { "" }
                );
            }
            match gp::adapter() {
                Some(a) => println!(
                    "adapter: {} driver {} power saving allowed: {:?}
         {}",
                    a.name, a.driver_version, a.power_saving_allowed, a.instance_id
                ),
                None => println!("adapter: none"),
            }
            println!("{:?}", gp::read()?);
        }
        PadCmd::Test { index, secs } => {
            let end = std::time::Instant::now() + Duration::from_secs(*secs);
            while std::time::Instant::now() < end {
                let r = gp::reading(*index)?;
                let pressed: Vec<&str> = gp::BUTTONS
                    .iter()
                    .filter(|(b, _)| r.buttons & b != 0)
                    .map(|(_, n)| *n)
                    .collect();
                println!(
                    "L({:+.2},{:+.2}) R({:+.2},{:+.2}) LT {:.2} RT {:.2} {}",
                    r.left.0,
                    r.left.1,
                    r.right.0,
                    r.right.1,
                    r.left_trigger,
                    r.right_trigger,
                    pressed.join(" ")
                );
                sleep(Duration::from_millis(250));
            }
        }
        PadCmd::AdapterPowerSaving { state } => {
            let a = gp::adapter().context("no Xbox Wireless Adapter connected")?;
            gp::set_adapter_power_saving(&a, state.on())?;
            println!(
                "{}: power saving {}",
                a.name,
                if state.on() { "allowed" } else { "off" }
            );
        }
        PadCmd::Rumble {
            index,
            left,
            right,
            left_trigger,
            right_trigger,
        } => {
            gp::rumble(
                *index,
                left / 100.0,
                right / 100.0,
                left_trigger / 100.0,
                right_trigger / 100.0,
            )?;
            sleep(Duration::from_millis(600));
            gp::rumble(*index, 0.0, 0.0, 0.0, 0.0)?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum AudioCmd {
    /// Active devices with volume, mute and default flags.
    List,
    /// Set a device's volume in percent. DEVICE: id, name, or part of the name.
    Volume { device: String, percent: f32 },
    /// Mute or unmute a device.
    Mute { device: String, state: OnOff },
    /// Make a device the default (games/media), or with --comms the default for calls.
    Default {
        device: String,
        #[arg(long)]
        comms: bool,
    },
    /// Apps currently playing audio.
    Apps,
    /// Advanced options of a device: format, enhancements, exclusive mode.
    Options { device: String },
    /// Set the device's default format, e.g. `format "Speakers (Razer" 48000 24`.
    Format {
        device: String,
        rate: u32,
        bits: u16,
    },
    /// Audio enhancements on/off.
    Enhancements { device: String, state: OnOff },
    /// Allow apps to take exclusive control (and optionally give them priority).
    Exclusive {
        device: String,
        state: OnOff,
        #[arg(long)]
        priority: Option<OnOff>,
    },
    /// Spatial sound state of every output.
    Spatial,
    /// Set a device's spatial format: windows-sonic, dolby-atmos-headphones, dts-headphone-x, ... or off.
    SpatialSet { device: String, format: String },
    /// Set an app's volume (percent) and/or mute by process id.
    App {
        pid: u32,
        #[arg(long)]
        volume: Option<f32>,
        #[arg(long)]
        mute: Option<OnOff>,
    },
}

fn audio(cmd: &AudioCmd) -> Result<()> {
    use hidconfig_win::audio::{self, Flow, Role};
    match cmd {
        AudioCmd::List => {
            for e in audio::endpoints()? {
                let tags = [(e.is_default, "default"), (e.is_default_comms, "calls")]
                    .iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, t)| *t)
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{:6} {:3.0}%{} {:<48} [{}]\n       {}",
                    if e.flow == Flow::Output { "out" } else { "in" },
                    e.volume * 100.0,
                    if e.muted { " muted" } else { "      " },
                    e.name,
                    tags,
                    e.id
                );
            }
        }
        AudioCmd::Volume { device, percent } => {
            let e = audio::find(None, device)?;
            audio::set_volume(&e.id, percent / 100.0)?;
            let now = audio::find(None, &e.id)?;
            println!("{}: {:.0}%", now.name, now.volume * 100.0);
        }
        AudioCmd::Mute { device, state } => {
            let e = audio::find(None, device)?;
            audio::set_mute(&e.id, state.on())?;
            println!("{}: muted = {}", e.name, audio::find(None, &e.id)?.muted);
        }
        AudioCmd::Default { device, comms } => {
            let e = audio::find(None, device)?;
            audio::set_default(
                &e.id,
                if *comms {
                    Role::Communications
                } else {
                    Role::Default
                },
            )?;
            let now = audio::find(None, &e.id)?;
            println!(
                "{}: default = {}, calls = {}",
                now.name, now.is_default, now.is_default_comms
            );
        }
        AudioCmd::Spatial => {
            for e in audio::endpoints()?
                .into_iter()
                .filter(|e| e.flow == Flow::Output)
            {
                match audio::spatial(&e.id) {
                    Ok(s) => println!(
                        "{}
    supported: {}  selected: {}  active: {}
    available: {}",
                        e.name,
                        s.supported,
                        s.selected.as_deref().unwrap_or("off"),
                        s.active.as_deref().unwrap_or("off"),
                        s.available.join(", ")
                    ),
                    Err(err) => println!(
                        "{}
    {err:#}",
                        e.name
                    ),
                }
            }
        }
        AudioCmd::SpatialSet { device, format } => {
            let e = audio::find(Some(Flow::Output), device)?;
            audio::set_spatial(&e.id, Some(format.as_str()))?;
            let st = audio::spatial(&e.id)?;
            println!(
                "{}: selected {}, active {}",
                e.name,
                st.selected.as_deref().unwrap_or("off"),
                st.active.as_deref().unwrap_or("off")
            );
        }
        AudioCmd::Options { device } => {
            let e = audio::find(None, device)?;
            let o = audio::device_options(&e.id)?;
            println!("{} ({} channels)", e.name, o.channels);
            println!(
                "  format:       {}",
                o.format.map_or("unknown".into(), |f| f.to_string())
            );
            println!(
                "  supported:    {}",
                o.formats
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            println!(
                "  enhancements: {}",
                o.enhancements
                    .map_or("not available", |on| if on { "on" } else { "off" })
            );
            println!(
                "  exclusive:    allowed {}, priority {}",
                o.exclusive_allowed, o.exclusive_priority
            );
        }
        AudioCmd::Format { device, rate, bits } => {
            let e = audio::find(None, device)?;
            audio::set_device_format(
                &e.id,
                audio::SampleFormat {
                    rate: *rate,
                    bits: *bits,
                },
            )?;
            let o = audio::device_options(&e.id)?;
            println!(
                "{}: {}",
                e.name,
                o.format.map_or("unknown".into(), |f| f.to_string())
            );
        }
        AudioCmd::Enhancements { device, state } => {
            let e = audio::find(None, device)?;
            audio::set_enhancements(&e.id, state.on())?;
            println!(
                "{}: enhancements {:?}",
                e.name,
                audio::device_options(&e.id)?.enhancements
            );
        }
        AudioCmd::Exclusive {
            device,
            state,
            priority,
        } => {
            let e = audio::find(None, device)?;
            audio::set_exclusive(&e.id, Some(state.on()), priority.map(|p| p.on()))?;
            let o = audio::device_options(&e.id)?;
            println!(
                "{}: exclusive allowed {}, priority {}",
                e.name, o.exclusive_allowed, o.exclusive_priority
            );
        }
        AudioCmd::Apps => {
            for s in audio::sessions()? {
                println!(
                    "{:>6}  {:3.0}%{}  {}",
                    s.pid,
                    s.volume * 100.0,
                    if s.muted { " muted" } else { "" },
                    s.app
                );
            }
        }
        AudioCmd::App { pid, volume, mute } => {
            let n = audio::set_session(*pid, volume.map(|v| v / 100.0), mute.map(|m| m.on()))?;
            println!("updated {n} session(s)");
        }
    }
    Ok(())
}

fn watch() -> Result<()> {
    println!(
        "watching (config: {}), Ctrl-C to stop",
        hidconfig_settings::path()?.display()
    );
    hidconfig_settings::watch(
        |ev| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() % 86400)
                .unwrap_or(0);
            println!(
                "[{:02}:{:02}:{:02} UTC] {ev:?}",
                now / 3600,
                now / 60 % 60,
                now % 60
            );
        },
        || true,
    )
}

fn descriptor(api: &HidApi, pid: &str) -> Result<()> {
    let pid = u16::from_str_radix(pid.trim_start_matches("0x"), 16).context("pid must be hex")?;
    for d in api
        .device_list()
        .filter(|d| d.vendor_id() == RAZER_VID && d.product_id() == pid)
    {
        println!(
            "if={} usage_page={:04x} usage={:04x}",
            d.interface_number(),
            d.usage_page(),
            d.usage()
        );
        let mut buf = [0u8; hidconfig_hid::hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
        match d
            .open_device(api)
            .and_then(|h| h.get_report_descriptor(&mut buf))
        {
            Ok(n) => {
                for chunk in buf[..n].chunks(16) {
                    println!(
                        "    {}",
                        chunk
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
            }
            Err(e) => println!("    error: {e}"),
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

const GUIDED_STEPS: &[&str] = &[
    "Both earbuds in the case, lid closed",
    "Open the lid",
    "Take the LEFT earbud out",
    "Take the RIGHT earbud out",
    "Put the LEFT earbud in your ear",
    "Put the RIGHT earbud in your ear",
    "Take the LEFT earbud out of your ear",
    "Put the LEFT earbud back in the case",
    "Put the RIGHT earbud back in the case",
    "Close the lid",
];

fn listen(api: &HidApi, pid: &str, secs: u64, guided: bool) -> Result<()> {
    use std::{
        io::{BufRead, Write},
        sync::mpsc,
        time::{Duration, Instant},
    };
    let pid = u16::from_str_radix(pid.trim_start_matches("0x"), 16).context("pid must be hex")?;
    std::fs::create_dir_all("captures")?;
    let log_path = format!(
        "captures/{pid:04x}-{}.log",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
    );
    let mut log = std::fs::File::create(&log_path)?;
    let mut out = |line: String| {
        println!("{line}");
        let _ = writeln!(log, "{line}");
    };

    let mut open = Vec::new();
    for d in api
        .device_list()
        .filter(|d| d.vendor_id() == RAZER_VID && d.product_id() == pid)
    {
        let tag = format!("{:04x}/{:04x}", d.usage_page(), d.usage());
        match d.open_device(api) {
            Ok(h) => {
                h.set_blocking_mode(false)?;
                open.push((tag, h));
            }
            Err(e) => out(format!("{tag}: open failed: {e}")),
        }
    }
    // GET_REPORT is a read; it never changes device state.
    let dump_features = |out: &mut dyn FnMut(String)| {
        for (tag, h) in &open {
            for id in 1..=0x10u8 {
                let mut buf = [0u8; 128];
                buf[0] = id;
                if let Ok(n) = h.get_feature_report(&mut buf) {
                    let n = n.min(64); // hidapi on Windows reports the buffer length
                    out(format!("{tag} feature {id:#04x}: {}", hex(&buf[..n])));
                }
            }
        }
    };
    dump_features(&mut out);

    let (tx, rx) = mpsc::channel();
    if guided {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                if line.is_err() || tx.send(()).is_err() {
                    break;
                }
            }
        });
        println!("\nGuided capture. Do each step, then press Enter.\n");
        println!(">> step 1: {}", GUIDED_STEPS[0]);
    } else {
        println!("listening {secs}s for input reports...");
    }

    let start = Instant::now();
    let mut step = 0;
    let mut buf = [0u8; 128];
    loop {
        if !guided && start.elapsed() >= Duration::from_secs(secs) {
            break;
        }
        for (tag, h) in &open {
            if let Ok(n) = h.read(&mut buf)
                && n > 0
            {
                let t = start.elapsed().as_secs_f32();
                out(format!("{t:6.1}s {tag} input: {}", hex(&buf[..n])));
            }
        }
        if rx.try_recv().is_ok() {
            let t = start.elapsed().as_secs_f32();
            out(format!(
                "{t:6.1}s ---- DONE step {}: {}",
                step + 1,
                GUIDED_STEPS[step]
            ));
            dump_features(&mut out);
            step += 1;
            if step == GUIDED_STEPS.len() {
                break;
            }
            println!(">> step {}: {}", step + 1, GUIDED_STEPS[step]);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("\nsaved to {log_path}");
    Ok(())
}
#[derive(Subcommand)]
enum EarbudsCmd {
    /// Read battery, connection and all readable settings.
    Info,
    /// Noise control.
    Anc { mode: AncArg },
    /// EQ preset.
    Eq { preset: EqArg },
    /// Do-not-disturb during 2.4 GHz gaming.
    Dnd { state: OnOff },
    /// Microphone on/off (on = live).
    Mic { state: OnOff },
    /// Wireless power saving.
    PowerSaving { state: OnOff },
    /// Power-saving timeout in minutes (5, 15, 30, 45, 60 in Synapse).
    Timeout { minutes: u16 },
    /// Auto-pause when an earbud is removed.
    AutoPause { state: OnOff },
    /// Touch gestures.
    Gestures { state: OnOff },
    /// Voice prompts.
    Prompts { state: OnOff },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl OnOff {
    fn on(self) -> bool {
        matches!(self, OnOff::On)
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum AncArg {
    Off,
    Anc,
    Ambient,
}

#[derive(Clone, Copy, ValueEnum)]
enum EqArg {
    Game,
    Music,
    Movie,
    Flat,
}

fn earbuds(api: &HidApi, cmd: &EarbudsCmd) -> Result<()> {
    use hidconfig_hid::earbuds::Earbuds;
    use hidconfig_proto::hyperspeed::{Anc, EqPreset};

    let e = Earbuds::open(api)?.context("Hammerhead V3 HyperSpeed dongle not found")?;
    let onoff = |b: bool| if b { "on" } else { "off" };
    match *cmd {
        EarbudsCmd::Info => {
            let pct = |p: Option<u8>| p.map_or("not connected".into(), |p| format!("{p}%"));
            match e.battery() {
                Ok(b) => println!(
                    "battery:       left {}, right {}",
                    pct(b.left),
                    pct(b.right)
                ),
                Err(err) => println!("battery:       {err:#}"),
            }
            println!("connection:    {:02x?}", e.connection()?);
            let (mode, calls) = e.anc()?;
            println!(
                "noise control: {mode:?} (reduce during BT calls: {})",
                onoff(calls == 0x01)
            );
            println!(
                "mic:           {}",
                if e.mic_muted()? { "off (muted)" } else { "on" }
            );
            println!("dnd (gaming):  {}", onoff(e.dnd()?));
            println!(
                "power saving:  {}, after {} min",
                onoff(e.power_saving()?),
                e.power_timeout_secs()? / 60
            );
        }
        EarbudsCmd::Anc { mode } => {
            let (_, calls) = e.anc()?;
            let mode = match mode {
                AncArg::Off => Anc::Off,
                AncArg::Anc => Anc::On,
                AncArg::Ambient => Anc::Ambient,
            };
            e.set_anc(mode, calls)?;
            println!("noise control now {:?}", e.anc()?.0);
        }
        EarbudsCmd::Eq { preset } => {
            e.set_eq(match preset {
                EqArg::Game => EqPreset::Game,
                EqArg::Music => EqPreset::Music,
                EqArg::Movie => EqPreset::Movie,
                EqArg::Flat => EqPreset::Custom,
            })?;
            println!("eq set");
        }
        EarbudsCmd::Dnd { state } => {
            e.set_dnd(state.on())?;
            println!("dnd now {}", onoff(e.dnd()?));
        }
        EarbudsCmd::Mic { state } => {
            e.set_mic_muted(!state.on())?;
            println!("mic now {}", onoff(!e.mic_muted()?));
        }
        EarbudsCmd::PowerSaving { state } => {
            e.set_power_saving(state.on())?;
            println!("power saving now {}", onoff(e.power_saving()?));
        }
        EarbudsCmd::Timeout { minutes } => {
            if !(1..=60).contains(&minutes) {
                bail!("timeout must be 1..=60 minutes");
            }
            e.set_power_timeout_secs(minutes * 60)?;
            println!("timeout now {} min", e.power_timeout_secs()? / 60);
        }
        EarbudsCmd::AutoPause { state } => {
            e.set_auto_pause(state.on())?;
            println!("auto-pause set {}", onoff(state.on()));
        }
        EarbudsCmd::Gestures { state } => {
            e.set_gestures(state.on())?;
            println!("gestures set {}", onoff(state.on()));
        }
        EarbudsCmd::Prompts { state } => {
            e.set_audio_prompts(state.on())?;
            println!("prompts set {}", onoff(state.on()));
        }
    }
    Ok(())
}

fn list(api: &HidApi) -> Result<()> {
    for d in api.device_list().filter(|d| d.vendor_id() == RAZER_VID) {
        println!(
            "pid={:04x} if={} usage_page={:04x} usage={:04x} product={:?}{}",
            d.product_id(),
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.product_string().unwrap_or(""),
            if hidconfig_hid::model(d.product_id()).is_some() {
                " [supported]"
            } else {
                ""
            }
        );
    }
    Ok(())
}
