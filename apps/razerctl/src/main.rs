use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use razer_hid::{Device, RAZER_VID, hidapi::HidApi};
use razer_proto::extended::Effect;

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
    /// Hammerhead V3 HyperSpeed: send one battery query to the dongle (experimental).
    EarbudsBattery,
    /// Show firmware and current settings of supported devices.
    Info,
    /// Set DPI (one value for both axes, or X and Y).
    Dpi { x: u16, y: Option<u16> },
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
        Cmd::EarbudsBattery => return earbuds_battery(&api),
        _ => {}
    }
    let devices: Vec<Device> = razer_hid::open_all(&api).into_iter().collect::<Result<_>>()?;
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
        Cmd::List | Cmd::Descriptor { .. } | Cmd::Listen { .. } | Cmd::EarbudsBattery => unreachable!(),
        Cmd::Info => {
            let (maj, min) = dev.firmware()?;
            let (x, y) = dev.dpi()?;
            println!("{} (firmware v{maj}.{min})", dev.model.name);
            println!("  dpi:        {x} x {y}");
            println!("  polling:    {} Hz", dev.polling_rate()?);
            println!("  brightness: {}", dev.logo_brightness()?);
        }
        Cmd::Dpi { x, y } => {
            dev.set_dpi(x, y.unwrap_or(x))?;
            let (rx, ry) = dev.dpi()?;
            println!("{}: dpi now {rx} x {ry}", dev.model.name);
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
            println!("{}: brightness now {}", dev.model.name, dev.logo_brightness()?);
        }
    }
    Ok(())
}

fn parse_rgb(s: &str) -> Result<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        bail!("colour must be RRGGBB");
    }
    let v = u32::from_str_radix(s, 16).context("colour must be hex RRGGBB")?;
    Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn descriptor(api: &HidApi, pid: &str) -> Result<()> {
    let pid = u16::from_str_radix(pid.trim_start_matches("0x"), 16).context("pid must be hex")?;
    for d in api.device_list().filter(|d| d.vendor_id() == RAZER_VID && d.product_id() == pid) {
        println!("if={} usage_page={:04x} usage={:04x}", d.interface_number(), d.usage_page(), d.usage());
        let mut buf = [0u8; razer_hid::hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
        match d.open_device(api).and_then(|h| h.get_report_descriptor(&mut buf)) {
            Ok(n) => {
                for chunk in buf[..n].chunks(16) {
                    println!("    {}", chunk.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
                }
            }
            Err(e) => println!("    error: {e}"),
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
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
    let log_path = format!("captures/{pid:04x}-{}.log", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?.as_secs());
    let mut log = std::fs::File::create(&log_path)?;
    let mut out = |line: String| {
        println!("{line}");
        let _ = writeln!(log, "{line}");
    };

    let mut open = Vec::new();
    for d in api.device_list().filter(|d| d.vendor_id() == RAZER_VID && d.product_id() == pid) {
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
            out(format!("{t:6.1}s ---- DONE step {}: {}", step + 1, GUIDED_STEPS[step]));
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
fn earbuds_battery(api: &HidApi) -> Result<()> {
    use razer_proto::hyperspeed::{self, FRAME_LEN};
    use std::time::{Duration, Instant};
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == RAZER_VID && d.product_id() == 0x0a61 && d.usage_page() == 0xff14)
        .context("Hammerhead V3 HyperSpeed dongle not found")?;
    let h = info.open_device(api)?;
    h.set_blocking_mode(false)?;
    let req = hyperspeed::encode(&hyperspeed::get_battery());
    println!("send  : {}", hex(&req[..16]));
    h.send_feature_report(&req).context("SET_FEATURE 0x02")?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        let mut f = [0u8; FRAME_LEN];
        f[0] = hyperspeed::REPORT_ID;
        if h.get_feature_report(&mut f).is_ok() && f[1..].iter().any(|&b| b != 0) {
            let t = start.elapsed().as_millis();
            match hyperspeed::decode(&f) {
                Some(m) => println!("{t:4}ms reply: {m:02x?}"),
                None => println!("{t:4}ms raw  : {}", hex(&f)),
            }
            if let Some(m) = hyperspeed::decode(&f)
                && m.cmd == 0x07
                && m.sub == 0x80
                && m.data.len() >= 2
            {
                let pct = |b: u8| if b == 0xff { "n/a".to_string() } else { format!("{b}%") };
                println!("battery: left {}, right {}", pct(m.data[0]), pct(m.data[1]));
                return Ok(());
            }
        }
        let mut buf = [0u8; 64];
        if let Ok(n) = h.read(&mut buf)
            && n > 0
        {
            println!("{:4}ms event: {}", start.elapsed().as_millis(), hex(&buf[..n]));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("no battery reply within 2s")
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
            if razer_hid::model(d.product_id()).is_some() { " [supported]" } else { "" }
        );
    }
    Ok(())
}
