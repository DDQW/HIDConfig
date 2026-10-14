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
    if let Cmd::List = cli.cmd {
        return list(&api);
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
        Cmd::List => unreachable!(),
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
