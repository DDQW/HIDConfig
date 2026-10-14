//! Saved device settings (`%APPDATA%\openrazer-win\config.toml`) and the loop that
//! re-applies them when a device appears, the PC resumes, or the file changes.

use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, bail};
use razer_hid::{
    Device,
    earbuds::{Battery, Earbuds},
    hidapi::HidApi,
};
use razer_proto::{
    extended::Effect,
    hyperspeed::{Anc, EqPreset},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub viper: Viper,
    pub earbuds: EarbudsCfg,
}

/// Every field is optional: unset means "leave the device alone".
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Viper {
    pub dpi: Option<u16>,
    pub polling_hz: Option<u16>,
    pub logo: Option<Logo>,
    pub brightness: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logo {
    pub effect: LogoEffect,
    /// `RRGGBB`, for static and breathing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogoEffect {
    Off,
    Static,
    Breathing,
    Spectrum,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EarbudsCfg {
    pub noise_control: Option<NoiseControl>,
    pub reduce_anc_on_calls: Option<bool>,
    pub eq: Option<Eq>,
    pub dnd_while_gaming: Option<bool>,
    pub power_saving: Option<bool>,
    pub power_saving_minutes: Option<u16>,
    pub auto_pause: Option<bool>,
    pub gestures: Option<bool>,
    pub audio_prompts: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoiseControl {
    Off,
    Anc,
    Ambient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eq {
    Game,
    Music,
    Movie,
    Flat,
}

impl From<NoiseControl> for Anc {
    fn from(n: NoiseControl) -> Self {
        match n {
            NoiseControl::Off => Anc::Off,
            NoiseControl::Anc => Anc::On,
            NoiseControl::Ambient => Anc::Ambient,
        }
    }
}

impl From<Eq> for EqPreset {
    fn from(e: Eq) -> Self {
        match e {
            Eq::Game => EqPreset::Game,
            Eq::Music => EqPreset::Music,
            Eq::Movie => EqPreset::Movie,
            Eq::Flat => EqPreset::Custom,
        }
    }
}

/// Byte the device uses for "reduce ANC during Bluetooth calls".
const CALLS_REDUCED: u8 = 0x01;
const CALLS_NORMAL: u8 = 0x0a;

pub fn parse_rgb(s: &str) -> Result<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        bail!("colour must be RRGGBB, got {s:?}");
    }
    let v = u32::from_str_radix(s, 16).with_context(|| format!("colour {s:?} is not hex"))?;
    Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

impl Logo {
    pub fn effect(&self) -> Result<Effect> {
        let rgb = || parse_rgb(self.color.as_deref().context("logo effect needs a color")?);
        Ok(match self.effect {
            LogoEffect::Off => Effect::Off,
            LogoEffect::Static => Effect::Static(rgb()?),
            LogoEffect::Breathing => Effect::Breathing(rgb()?),
            LogoEffect::Spectrum => Effect::Spectrum,
        })
    }
}

pub fn path() -> Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(appdata).join("openrazer-win").join("config.toml"))
}

impl Config {
    /// Missing file = empty config.
    pub fn load() -> Result<Self> {
        let p = path()?;
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    pub fn save(&self) -> Result<PathBuf> {
        let p = path()?;
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(p)
    }
}

/// Applies every set field; keeps going past individual failures and reports them together.
pub fn apply_viper(dev: &Device, cfg: &Viper) -> Result<()> {
    let mut errors = Vec::new();
    let mut run = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            errors.push(format!("{what}: {e:#}"));
        }
    };
    if let Some(d) = cfg.dpi {
        run("dpi", dev.set_dpi(d, d));
    }
    if let Some(hz) = cfg.polling_hz {
        run("polling", dev.set_polling_rate(hz));
    }
    if let Some(logo) = &cfg.logo {
        run("logo", logo.effect().and_then(|e| dev.set_logo_effect(e)));
    }
    if let Some(b) = cfg.brightness {
        run("brightness", dev.set_logo_brightness(b));
    }
    finish(errors)
}

pub fn apply_earbuds(e: &Earbuds, cfg: &EarbudsCfg) -> Result<()> {
    let mut errors = Vec::new();
    let mut run = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            errors.push(format!("{what}: {e:#}"));
        }
    };
    if cfg.noise_control.is_some() || cfg.reduce_anc_on_calls.is_some() {
        run(
            "noise control",
            e.anc().and_then(|(mode, calls)| {
                let mode = cfg.noise_control.map_or(mode, Anc::from);
                let calls = match cfg.reduce_anc_on_calls {
                    Some(true) => CALLS_REDUCED,
                    Some(false) => CALLS_NORMAL,
                    None => calls,
                };
                e.set_anc(mode, calls)
            }),
        );
    }
    if let Some(eq) = cfg.eq {
        run("eq", e.set_eq(eq.into()));
    }
    if let Some(v) = cfg.dnd_while_gaming {
        run("dnd", e.set_dnd(v));
    }
    if let Some(v) = cfg.power_saving {
        run("power saving", e.set_power_saving(v));
    }
    if let Some(m) = cfg.power_saving_minutes {
        run(
            "power saving timeout",
            if (1..=60).contains(&m) {
                e.set_power_timeout_secs(m * 60)
            } else {
                Err(anyhow::anyhow!("must be 1..=60 minutes"))
            },
        );
    }
    if let Some(v) = cfg.auto_pause {
        run("auto-pause", e.set_auto_pause(v));
    }
    if let Some(v) = cfg.gestures {
        run("gestures", e.set_gestures(v));
    }
    if let Some(v) = cfg.audio_prompts {
        run("audio prompts", e.set_audio_prompts(v));
    }
    finish(errors)
}

fn finish(errors: Vec<String>) -> Result<()> {
    if errors.is_empty() { Ok(()) } else { bail!(errors.join("; ")) }
}

/// Reads what the devices can report into a config. The logo effect, EQ, auto-pause,
/// gestures and prompts have no getter and are kept from `base`.
pub fn snapshot(api: &HidApi, base: &Config) -> Result<Config> {
    let mut cfg = base.clone();
    for dev in razer_hid::open_all(api).into_iter().flatten() {
        let (x, _) = dev.dpi()?;
        cfg.viper.dpi = Some(x);
        cfg.viper.polling_hz = Some(dev.polling_rate()?);
        cfg.viper.brightness = Some(dev.logo_brightness()?);
    }
    if let Some(e) = Earbuds::open(api)? {
        let (mode, calls) = e.anc()?;
        cfg.earbuds.noise_control = Some(match mode {
            Anc::Off => NoiseControl::Off,
            Anc::Ambient => NoiseControl::Ambient,
            _ => NoiseControl::Anc,
        });
        cfg.earbuds.reduce_anc_on_calls = Some(calls == CALLS_REDUCED);
        cfg.earbuds.dnd_while_gaming = Some(e.dnd()?);
        cfg.earbuds.power_saving = Some(e.power_saving()?);
        cfg.earbuds.power_saving_minutes = Some(e.power_timeout_secs()? / 60);
    }
    Ok(cfg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Viper,
    Earbuds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Applied(Target),
    Failed(Target, String),
    /// No earbud is connected to the dongle (usually: in the case); will check again.
    EarbudsWaiting,
    /// Applying failed; another attempt follows.
    Retrying(Target, String),
    ConfigError(String),
    Reapplying(Trigger),
}

/// Why everything is re-applied (device arrival is handled per device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Resume,
    ConfigChanged,
}

/// What the devices last reported. Refreshed on apply and every [`STATUS_EVERY`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Status {
    pub viper: Option<ViperStatus>,
    pub earbuds: Option<EarbudsStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViperStatus {
    pub name: &'static str,
    pub dpi: u16,
    pub polling_hz: u16,
    pub brightness: u8,
}

/// `None` fields could not be read (typically: buds in the case).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct EarbudsStatus {
    pub connected: bool,
    pub battery: Option<Battery>,
    pub noise_control: Option<NoiseControl>,
    pub mic_muted: Option<bool>,
}

pub const TICK: Duration = Duration::from_secs(2);
/// A tick that took this long means the machine slept.
const RESUME_GAP: Duration = Duration::from_secs(15);
/// Let the wireless link come up after a plug-in or wake-up before talking to the buds.
const EARBUDS_SETTLE: Duration = Duration::from_secs(5);
/// While no bud is connected.
const EARBUDS_POLL: Duration = Duration::from_secs(20);
const EARBUDS_RETRY: Duration = Duration::from_secs(10);
const EARBUDS_ATTEMPTS: u32 = 6;
pub const STATUS_EVERY: Duration = Duration::from_secs(30);

/// Keeps devices in line with the config. Drive it by calling [`Watcher::tick`]
/// every [`TICK`]; it re-applies on device arrival, wake-up and config edits.
pub struct Watcher {
    api: HidApi,
    config: Config,
    config_mtime: Option<SystemTime>,
    viper_present: bool,
    dongle_present: bool,
    // When the next earbuds attempt is due, and how many have failed so far.
    earbuds_due: Option<Instant>,
    earbuds_failures: u32,
    last_tick: Instant,
    status: Status,
    status_due: Instant,
}

impl Watcher {
    pub fn new(on_event: &mut dyn FnMut(Event)) -> Result<Self> {
        let config = Config::load().unwrap_or_else(|e| {
            on_event(Event::ConfigError(format!("{e:#}")));
            Config::default()
        });
        Ok(Watcher {
            api: HidApi::new()?,
            config,
            config_mtime: mtime(),
            viper_present: false,
            dongle_present: false,
            earbuds_due: None,
            earbuds_failures: 0,
            last_tick: Instant::now(),
            status: Status::default(),
            status_due: Instant::now(),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Edits the config, saves it, and applies the sections that changed right away.
    pub fn update_config(
        &mut self,
        edit: impl FnOnce(&mut Config),
        on_event: &mut dyn FnMut(Event),
    ) -> Result<()> {
        let mut new = self.config.clone();
        edit(&mut new);
        if new == self.config {
            return Ok(());
        }
        new.save()?;
        self.config_mtime = mtime();
        let old = std::mem::replace(&mut self.config, new);
        self.apply_changed(&old, on_event);
        Ok(())
    }

    /// Mic mute is a live toggle, not a saved setting.
    pub fn set_mic_muted(&mut self, muted: bool) -> Result<()> {
        let e = Earbuds::open(&self.api)?.context("earbuds dongle not connected")?;
        e.set_mic_muted(muted)?;
        if let Some(s) = &mut self.status.earbuds {
            s.mic_muted = Some(muted);
        }
        Ok(())
    }

    /// Forces a status refresh on the next tick.
    pub fn refresh_status_soon(&mut self) {
        self.status_due = Instant::now();
    }

    fn apply_changed(&mut self, old: &Config, on_event: &mut dyn FnMut(Event)) {
        if old.viper != self.config.viper {
            self.apply_viper_now(on_event);
        }
        if old.earbuds != self.config.earbuds {
            // Immediate attempt; failures fall back to the retry schedule.
            self.earbuds_due = Some(Instant::now());
            self.earbuds_failures = 0;
            self.try_earbuds(on_event);
        }
        self.status_due = Instant::now();
    }

    fn apply_viper_now(&mut self, on_event: &mut dyn FnMut(Event)) {
        if self.config.viper == Viper::default() {
            return;
        }
        for d in razer_hid::open_all(&self.api).into_iter().flatten() {
            on_event(match apply_viper(&d, &self.config.viper) {
                Ok(()) => Event::Applied(Target::Viper),
                Err(e) => Event::Failed(Target::Viper, format!("{e:#}")),
            });
        }
    }

    fn try_earbuds(&mut self, on_event: &mut dyn FnMut(Event)) {
        let Some(due) = self.earbuds_due else { return };
        if Instant::now() < due {
            return;
        }
        let Ok(Some(e)) = Earbuds::open(&self.api) else {
            self.earbuds_due = None;
            return;
        };
        if self.config.earbuds == EarbudsCfg::default() {
            self.earbuds_due = None;
            return;
        }
        // Cheap probe first so buds in the case cost one timeout, not one per setting.
        if !connected(&e) {
            on_event(Event::EarbudsWaiting);
            self.earbuds_due = Some(Instant::now() + EARBUDS_POLL);
            return;
        }
        match apply_earbuds(&e, &self.config.earbuds) {
            Ok(()) => {
                on_event(Event::Applied(Target::Earbuds));
                self.earbuds_due = None;
            }
            Err(err) => {
                self.earbuds_failures += 1;
                if self.earbuds_failures < EARBUDS_ATTEMPTS {
                    on_event(Event::Retrying(Target::Earbuds, format!("{err:#}")));
                    self.earbuds_due = Some(Instant::now() + EARBUDS_RETRY);
                } else {
                    on_event(Event::Failed(Target::Earbuds, format!("{err:#}")));
                    self.earbuds_due = None;
                }
            }
        }
    }

    pub fn tick(&mut self, on_event: &mut dyn FnMut(Event)) {
        let mut trigger = None;
        if self.last_tick.elapsed() > RESUME_GAP {
            trigger = Some(Trigger::Resume);
        }
        self.last_tick = Instant::now();

        let m = mtime();
        if m != self.config_mtime {
            self.config_mtime = m;
            match Config::load() {
                Ok(c) if c != self.config => {
                    let old = std::mem::replace(&mut self.config, c);
                    if trigger.is_none() {
                        on_event(Event::Reapplying(Trigger::ConfigChanged));
                        self.apply_changed(&old, on_event);
                    }
                }
                Ok(_) => {}
                Err(e) => on_event(Event::ConfigError(format!("{e:#}"))),
            }
        }

        if let Err(e) = self.api.refresh_devices() {
            on_event(Event::Failed(Target::Viper, format!("enumerating HID devices: {e}")));
        }
        if let Some(t) = trigger {
            on_event(Event::Reapplying(t));
        }
        let viper_now = razer_hid::open_all(&self.api).iter().any(|d| d.is_ok());
        let dongle_now = matches!(Earbuds::open(&self.api), Ok(Some(_)));

        if viper_now && (!self.viper_present || trigger.is_some()) {
            self.apply_viper_now(on_event);
            self.status_due = Instant::now();
        }
        if dongle_now && (!self.dongle_present || trigger.is_some()) {
            self.earbuds_due = Some(Instant::now() + EARBUDS_SETTLE);
            self.earbuds_failures = 0;
        }
        if viper_now != self.viper_present || dongle_now != self.dongle_present {
            self.status_due = Instant::now();
        }
        self.viper_present = viper_now;
        self.dongle_present = dongle_now;

        let had_due = self.earbuds_due.is_some();
        self.try_earbuds(on_event);
        if had_due && self.earbuds_due.is_none() {
            self.status_due = Instant::now();
        }

        if Instant::now() >= self.status_due {
            self.refresh_status();
            self.status_due = Instant::now() + STATUS_EVERY;
        }
    }

    fn refresh_status(&mut self) {
        let viper = razer_hid::open_all(&self.api).into_iter().flatten().next().and_then(|d| {
            Some(ViperStatus {
                name: d.model.name,
                dpi: d.dpi().ok()?.0,
                polling_hz: d.polling_rate().ok()?,
                brightness: d.logo_brightness().ok()?,
            })
        });
        let earbuds = Earbuds::open(&self.api).ok().flatten().map(|e| {
            if !connected(&e) {
                return EarbudsStatus::default();
            }
            EarbudsStatus {
                connected: true,
                battery: e.battery().ok(),
                noise_control: e.anc().ok().map(|(m, _)| match m {
                    Anc::Off => NoiseControl::Off,
                    Anc::Ambient => NoiseControl::Ambient,
                    _ => NoiseControl::Anc,
                }),
                mic_muted: e.mic_muted().ok(),
            }
        });
        self.status = Status { viper, earbuds };
    }
}

fn connected(e: &Earbuds) -> bool {
    e.connection().is_ok_and(|d| d.iter().any(|&b| b != 0))
}

/// Runs a [`Watcher`] forever (until `keep_going` returns false).
pub fn watch(mut on_event: impl FnMut(Event), mut keep_going: impl FnMut() -> bool) -> Result<()> {
    let mut w = Watcher::new(&mut on_event)?;
    while keep_going() {
        w.tick(&mut on_event);
        thread::sleep(TICK);
    }
    Ok(())
}

fn mtime() -> Option<SystemTime> {
    path().ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip() {
        let src = r#"
[viper]
dpi = 1800
polling_hz = 1000
brightness = 178

[viper.logo]
effect = "static"
color = "ff0000"

[earbuds]
noise_control = "ambient"
eq = "flat"
power_saving_minutes = 15
"#;
        let c: Config = toml::from_str(src).unwrap();
        assert_eq!(c.viper.dpi, Some(1800));
        assert_eq!(c.viper.logo.as_ref().unwrap().effect().unwrap(), Effect::Static([0xff, 0, 0]));
        assert_eq!(c.earbuds.noise_control, Some(NoiseControl::Ambient));
        assert_eq!(c.earbuds.gestures, None);
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn empty_and_typos() {
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
        assert!(toml::from_str::<Config>("[viper]\ndpj = 800").is_err());
    }
}
