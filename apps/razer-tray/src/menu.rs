//! Tray context menu: built once, then re-checked from each snapshot.

use anyhow::Result;
use razer_settings::{Config, Eq, EarbudsCfg, Logo, LogoEffect, NoiseControl};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::worker::Snapshot;

pub const DPI_PRESETS: &[u16] = &[400, 800, 1200, 1600, 1800, 2400, 3200, 4000, 6400];
pub const POLL_RATES: &[u16] = &[125, 500, 1000];
pub const LOGO_EFFECTS: &[(LogoEffect, &str)] = &[
    (LogoEffect::Off, "Off"),
    (LogoEffect::Static, "Static"),
    (LogoEffect::Breathing, "Breathing"),
    (LogoEffect::Spectrum, "Spectrum"),
];
pub const COLORS: &[(&str, &str)] = &[
    ("44d62c", "Razer green"),
    ("ff0000", "Red"),
    ("ff7a00", "Orange"),
    ("ffd400", "Yellow"),
    ("00c8ff", "Cyan"),
    ("0040ff", "Blue"),
    ("a020f0", "Purple"),
    ("ffffff", "White"),
];
/// (brightness byte, label)
pub const BRIGHTNESS: &[(u8, &str)] =
    &[(0, "Off"), (64, "25 %"), (128, "50 %"), (191, "75 %"), (255, "100 %")];
pub const NOISE: &[(NoiseControl, &str)] = &[
    (NoiseControl::Off, "Off"),
    (NoiseControl::Anc, "ANC"),
    (NoiseControl::Ambient, "Ambient"),
];
pub const EQS: &[(Eq, &str)] =
    &[(Eq::Game, "Game"), (Eq::Music, "Music"), (Eq::Movie, "Movie"), (Eq::Flat, "Flat")];
/// Power-saving timeout in minutes; 0 = power saving off.
pub const POWER: &[u16] = &[0, 5, 15, 30, 45, 60];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    ReduceCalls,
    Dnd,
    AutoPause,
    Gestures,
    Prompts,
}

impl Toggle {
    pub const ALL: [Toggle; 5] =
        [Toggle::ReduceCalls, Toggle::Dnd, Toggle::AutoPause, Toggle::Gestures, Toggle::Prompts];

    pub fn key(self) -> &'static str {
        match self {
            Toggle::ReduceCalls => "reduce-calls",
            Toggle::Dnd => "dnd",
            Toggle::AutoPause => "auto-pause",
            Toggle::Gestures => "gestures",
            Toggle::Prompts => "prompts",
        }
    }

    pub fn from_key(k: &str) -> Option<Self> {
        Toggle::ALL.into_iter().find(|t| t.key() == k)
    }

    fn label(self) -> &'static str {
        match self {
            Toggle::ReduceCalls => "Reduce ANC on Bluetooth calls",
            Toggle::Dnd => "Do not disturb while gaming",
            Toggle::AutoPause => "Auto-pause",
            Toggle::Gestures => "Touch gestures",
            Toggle::Prompts => "Voice prompts",
        }
    }

    fn slot(self, e: &mut EarbudsCfg) -> &mut Option<bool> {
        match self {
            Toggle::ReduceCalls => &mut e.reduce_anc_on_calls,
            Toggle::Dnd => &mut e.dnd_while_gaming,
            Toggle::AutoPause => &mut e.auto_pause,
            Toggle::Gestures => &mut e.gestures,
            Toggle::Prompts => &mut e.audio_prompts,
        }
    }

    pub fn get(self, e: &EarbudsCfg) -> Option<bool> {
        match self {
            Toggle::ReduceCalls => e.reduce_anc_on_calls,
            Toggle::Dnd => e.dnd_while_gaming,
            Toggle::AutoPause => e.auto_pause,
            Toggle::Gestures => e.gestures,
            Toggle::Prompts => e.audio_prompts,
        }
    }

    pub fn set(self, e: &mut EarbudsCfg, v: bool) {
        *self.slot(e) = Some(v);
    }
}

/// What a menu click asks for.
pub enum Action {
    Edit(Box<dyn FnOnce(&mut Config) + Send>),
    Mic(bool),
    Settings,
    Autostart(bool),
    OpenConfig,
    Quit,
}

pub struct TrayMenu {
    pub menu: Menu,
    viper_header: MenuItem,
    viper_subs: Vec<Submenu>,
    dpi: Vec<(u16, CheckMenuItem)>,
    poll: Vec<(u16, CheckMenuItem)>,
    logo: Vec<(LogoEffect, CheckMenuItem)>,
    colors: Vec<(&'static str, CheckMenuItem)>,
    bright: Vec<(u8, CheckMenuItem)>,
    buds_header: MenuItem,
    buds_subs: Vec<Submenu>,
    noise: Vec<(NoiseControl, CheckMenuItem)>,
    eq: Vec<(Eq, CheckMenuItem)>,
    power: Vec<(u16, CheckMenuItem)>,
    toggles: Vec<(Toggle, CheckMenuItem)>,
    mic: CheckMenuItem,
    autostart: CheckMenuItem,
}

fn check(id: String, text: &str) -> CheckMenuItem {
    CheckMenuItem::with_id(id, text, true, false, None)
}

fn submenu(text: &str, items: &[&CheckMenuItem]) -> Result<Submenu> {
    let items: Vec<&dyn IsMenuItem> = items.iter().map(|i| *i as &dyn IsMenuItem).collect();
    Ok(Submenu::with_items(text, true, &items)?)
}

impl TrayMenu {
    pub fn build(autostart: bool) -> Result<Self> {
        let viper_header = MenuItem::new("Razer Viper", false, None);
        let dpi: Vec<_> =
            DPI_PRESETS.iter().map(|&d| (d, check(format!("dpi:{d}"), &d.to_string()))).collect();
        let poll: Vec<_> =
            POLL_RATES.iter().map(|&h| (h, check(format!("poll:{h}"), &format!("{h} Hz")))).collect();
        let logo: Vec<_> = LOGO_EFFECTS
            .iter()
            .map(|&(e, l)| (e, check(format!("logo:{}", l.to_lowercase()), l)))
            .collect();
        let colors: Vec<_> =
            COLORS.iter().map(|&(hex, l)| (hex, check(format!("color:{hex}"), l))).collect();
        let bright: Vec<_> =
            BRIGHTNESS.iter().map(|&(b, l)| (b, check(format!("bright:{b}"), l))).collect();

        let viper_subs = vec![
            submenu("DPI", &dpi.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("Polling rate", &poll.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("Logo effect", &logo.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("Logo colour", &colors.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("Logo brightness", &bright.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
        ];

        let buds_header = MenuItem::new("Hammerhead V3 HyperSpeed", false, None);
        let noise: Vec<_> = NOISE
            .iter()
            .map(|&(n, l)| (n, check(format!("anc:{}", l.to_lowercase()), l)))
            .collect();
        let eq: Vec<_> =
            EQS.iter().map(|&(e, l)| (e, check(format!("eq:{}", l.to_lowercase()), l))).collect();
        let power: Vec<_> = POWER
            .iter()
            .map(|&m| {
                let label = if m == 0 { "Off".to_string() } else { format!("After {m} min") };
                (m, check(format!("power:{m}"), &label))
            })
            .collect();
        let toggles: Vec<_> =
            Toggle::ALL.iter().map(|&t| (t, check(format!("toggle:{}", t.key()), t.label()))).collect();
        let mic = check("mic".into(), "Microphone on");
        let buds_subs = vec![
            submenu("Noise control", &noise.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("EQ preset", &eq.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("Power saving", &power.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
            submenu("Options", &toggles.iter().map(|(_, i)| i).collect::<Vec<_>>())?,
        ];

        let settings = MenuItem::with_id("settings", "Settings…", true, None);
        let autostart_item = CheckMenuItem::with_id("autostart", "Start with Windows", true, autostart, None);
        let open_config = MenuItem::with_id("config", "Open config file", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);

        let menu = Menu::new();
        menu.append(&viper_header)?;
        for s in &viper_subs {
            menu.append(s)?;
        }
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&buds_header)?;
        for s in &buds_subs {
            menu.append(s)?;
        }
        menu.append(&mic)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append_items(&[&settings, &autostart_item, &open_config])?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        Ok(TrayMenu {
            menu,
            viper_header,
            viper_subs,
            dpi,
            poll,
            logo,
            colors,
            bright,
            buds_header,
            buds_subs,
            noise,
            eq,
            power,
            toggles,
            mic,
            autostart: autostart_item,
        })
    }

    /// Re-syncs labels and check marks. Values come from what the device reports
    /// when it can, otherwise from the config.
    pub fn update(&self, s: &Snapshot, autostart: bool) {
        let v = &s.status.viper;
        let cv = &s.config.viper;
        self.viper_header.set_text(match v {
            Some(v) => format!("{} · {} DPI · {} Hz", v.name, v.dpi, v.polling_hz),
            None => "Razer Viper (not connected)".into(),
        });
        for sub in &self.viper_subs {
            sub.set_enabled(v.is_some());
        }
        let dpi = v.as_ref().map(|v| v.dpi).or(cv.dpi);
        for (d, i) in &self.dpi {
            i.set_checked(Some(*d) == dpi);
        }
        let hz = v.as_ref().map(|v| v.polling_hz).or(cv.polling_hz);
        for (h, i) in &self.poll {
            i.set_checked(Some(*h) == hz);
        }
        let effect = cv.logo.as_ref().map(|l| l.effect);
        for (e, i) in &self.logo {
            i.set_checked(Some(*e) == effect);
        }
        let color = cv.logo.as_ref().and_then(|l| l.color.as_deref());
        for (hex, i) in &self.colors {
            i.set_checked(Some(*hex) == color);
        }
        let bright = v.as_ref().map(|v| v.brightness).or(cv.brightness);
        for (b, i) in &self.bright {
            i.set_checked(Some(*b) == bright);
        }

        let es = s.status.earbuds.as_ref();
        let ce = &s.config.earbuds;
        self.buds_header.set_text(match es {
            None => "Hammerhead V3 (dongle not connected)".into(),
            Some(e) if !e.connected => "Hammerhead V3 (not connected: in case or asleep)".into(),
            Some(e) => format!("Hammerhead V3 · {}", battery_text(e)),
        });
        let live = es.is_some_and(|e| e.connected);
        for sub in &self.buds_subs {
            sub.set_enabled(es.is_some());
        }
        let noise = es.and_then(|e| e.noise_control).or(ce.noise_control);
        for (n, i) in &self.noise {
            i.set_checked(Some(*n) == noise);
        }
        for (e, i) in &self.eq {
            i.set_checked(Some(*e) == ce.eq);
        }
        let power = match ce.power_saving {
            Some(false) => Some(0),
            _ => ce.power_saving_minutes,
        };
        for (m, i) in &self.power {
            i.set_checked(Some(*m) == power);
        }
        for (t, i) in &self.toggles {
            i.set_checked(t.get(ce) == Some(true));
        }
        self.mic.set_enabled(live);
        self.mic.set_checked(es.and_then(|e| e.mic_muted) == Some(false));
        self.autostart.set_checked(autostart);
    }

    /// Turns a clicked item id into an action. Check items have already toggled
    /// themselves, so their state is the requested one.
    pub fn action(&self, id: &str) -> Option<Action> {
        let (kind, arg) = id.split_once(':').unwrap_or((id, ""));
        let edit = |f: Box<dyn FnOnce(&mut Config) + Send>| Some(Action::Edit(f));
        match kind {
            "dpi" => {
                let d: u16 = arg.parse().ok()?;
                edit(Box::new(move |c| c.viper.dpi = Some(d)))
            }
            "poll" => {
                let h: u16 = arg.parse().ok()?;
                edit(Box::new(move |c| c.viper.polling_hz = Some(h)))
            }
            "logo" => {
                let effect = LOGO_EFFECTS.iter().find(|(_, l)| l.eq_ignore_ascii_case(arg))?.0;
                edit(Box::new(move |c| {
                    let color = c.viper.logo.as_ref().and_then(|l| l.color.clone());
                    let needs_color = matches!(effect, LogoEffect::Static | LogoEffect::Breathing);
                    c.viper.logo = Some(Logo {
                        effect,
                        color: if needs_color { color.or(Some(COLORS[0].0.into())) } else { color },
                    });
                }))
            }
            "color" => {
                let hex = arg.to_string();
                edit(Box::new(move |c| {
                    // Picking a colour implies a colour effect; keep breathing if chosen.
                    let effect = match c.viper.logo.as_ref().map(|l| l.effect) {
                        Some(LogoEffect::Breathing) => LogoEffect::Breathing,
                        _ => LogoEffect::Static,
                    };
                    c.viper.logo = Some(Logo { effect, color: Some(hex) });
                }))
            }
            "bright" => {
                let b: u8 = arg.parse().ok()?;
                edit(Box::new(move |c| c.viper.brightness = Some(b)))
            }
            "anc" => {
                let n = NOISE.iter().find(|(_, l)| l.eq_ignore_ascii_case(arg))?.0;
                edit(Box::new(move |c| c.earbuds.noise_control = Some(n)))
            }
            "eq" => {
                let e = EQS.iter().find(|(_, l)| l.eq_ignore_ascii_case(arg))?.0;
                edit(Box::new(move |c| c.earbuds.eq = Some(e)))
            }
            "power" => {
                let m: u16 = arg.parse().ok()?;
                edit(Box::new(move |c| set_power(&mut c.earbuds, m)))
            }
            "toggle" => {
                let t = Toggle::from_key(arg)?;
                let on = self.toggles.iter().find(|(x, _)| *x == t)?.1.is_checked();
                edit(Box::new(move |c| t.set(&mut c.earbuds, on)))
            }
            "mic" => Some(Action::Mic(self.mic.is_checked())),
            "settings" => Some(Action::Settings),
            "autostart" => Some(Action::Autostart(self.autostart.is_checked())),
            "config" => Some(Action::OpenConfig),
            "quit" => Some(Action::Quit),
            _ => None,
        }
    }
}

/// 0 minutes = power saving off.
pub fn set_power(e: &mut EarbudsCfg, minutes: u16) {
    if minutes == 0 {
        e.power_saving = Some(false);
    } else {
        e.power_saving = Some(true);
        e.power_saving_minutes = Some(minutes);
    }
}

pub fn battery_text(e: &razer_settings::EarbudsStatus) -> String {
    let pct = |p: Option<u8>| p.map_or("–".into(), |p| format!("{p}%"));
    match e.battery {
        Some(b) => format!("L {} · R {}", pct(b.left), pct(b.right)),
        None => "battery unknown".into(),
    }
}
