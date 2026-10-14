#![windows_subsystem = "windows"]
//! Tray app: icon + menu for quick changes, a settings window for everything else.
//! Device I/O happens on the worker thread; this thread only runs the UI.

mod icon;
mod menu;
mod platform;
mod worker;

use std::{cell::RefCell, sync::mpsc::Sender};

use anyhow::Result;
use razer_settings::{Eq, LogoEffect, NoiseControl, StageDpi, parse_rgb};
use slint::ComponentHandle;
use slint::Model;
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, menu::MenuEvent,
};

use crate::{
    menu::{Action, EQS, LOGO_EFFECTS, NOISE, POLL_RATES, POWER, Toggle, TrayMenu, battery_text},
    platform::log,
    worker::{Cmd, Snapshot},
};

slint::include_modules!();

struct App {
    tray: TrayIcon,
    menu: TrayMenu,
    ui: SettingsWindow,
    tx: Sender<Cmd>,
    snapshot: Option<Snapshot>,
    icon_state: icon::State,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            f(app);
        }
    });
}

fn main() {
    platform::init_log();
    // Used by the installer, which runs these as the signed-in user (not elevated).
    for arg in std::env::args().skip(1) {
        let on = match arg.as_str() {
            "--enable-autostart" => true,
            "--disable-autostart" => false,
            _ => continue,
        };
        if let Err(e) = platform::set_autostart(on) {
            log(&format!("{arg}: {e:#}"));
            std::process::exit(1);
        }
        return;
    }
    let Some(_instance) = platform::single_instance() else {
        return; // already running
    };
    log("starting");
    if let Err(e) = run() {
        log(&format!("fatal: {e:#}"));
    }
    log("exiting");
}

fn run() -> Result<()> {
    let autostart = platform::autostart_enabled();
    let menu = TrayMenu::build(autostart)?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu.menu.clone()))
        .with_menu_on_left_click(false)
        .with_tooltip("openrazer-win: starting…")
        .with_icon(icon::icon(icon::State::Idle))
        .build()?;

    let tx = worker::spawn(|snap| {
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.apply(snap)));
    });

    let ui = SettingsWindow::new()?;
    ui.global::<AppState>().set_config_path(
        razer_settings::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.global::<AppState>().set_autostart(autostart);
    wire_ui(&ui, &tx);

    MenuEvent::set_event_handler(Some(|ev: MenuEvent| {
        let id = ev.id.0;
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.on_menu(&id)));
    }));
    TrayIconEvent::set_event_handler(Some(|ev: TrayIconEvent| {
        let open = matches!(
            ev,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        );
        if open {
            let _ = slint::invoke_from_event_loop(|| with_app(|app| app.show_settings()));
        }
    }));

    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            tray,
            menu,
            ui,
            tx: tx.clone(),
            snapshot: None,
            icon_state: icon::State::Idle,
        })
    });

    slint::run_event_loop_until_quit()?;
    let _ = tx.send(Cmd::Quit);
    APP.with(|a| a.borrow_mut().take());
    Ok(())
}

/// Stages become the source of the live DPI: the active stage's value applies.
pub fn set_stages(c: &mut razer_settings::Config, stages: Vec<StageDpi>, active: u8) {
    c.viper.dpi_stages = Some(stages);
    c.viper.dpi_stage = Some(active);
    c.viper.dpi = None;
    c.viper.dpi_y = None;
}

/// Sends a config edit to the worker.
fn edit(tx: &Sender<Cmd>, f: impl FnOnce(&mut razer_settings::Config) + Send + 'static) {
    let _ = tx.send(Cmd::Edit(Box::new(f)));
}

fn wire_ui(ui: &SettingsWindow, tx: &Sender<Cmd>) {
    let t = tx.clone();
    ui.global::<AppState>().on_dpi_changed(move |x, y| {
        let x = x.clamp(100, 16000) as u16;
        let y = y.clamp(100, 16000) as u16;
        edit(&t, move |c| {
            c.viper.dpi = Some(x);
            c.viper.dpi_y = (y != x).then_some(y);
        });
    });
    let weak = ui.as_weak();
    ui.global::<AppState>().on_stage_count_changed(move |n| {
        let Some(ui) = weak.upgrade() else { return };
        let mut stages: Vec<StageValue> = ui.global::<AppState>().get_stages().iter().collect();
        let n = n.clamp(1, 5) as usize;
        while stages.len() < n {
            let last = stages
                .last()
                .cloned()
                .unwrap_or(StageValue { x: 400, y: 400 });
            let next = |v: i32| (v * 2).min(16000);
            stages.push(StageValue {
                x: next(last.x),
                y: next(last.y),
            });
        }
        stages.truncate(n);
        ui.global::<AppState>()
            .set_stages(slint::ModelRc::new(slint::VecModel::from(stages)));
        if ui.global::<AppState>().get_active_stage() as usize > n {
            ui.global::<AppState>().set_active_stage(n as i32);
        }
        ui.global::<AppState>().set_stages_dirty(true);
    });
    ui.global::<AppState>().on_stages_revert(|| {
        with_app(|app| {
            app.ui.global::<AppState>().set_stages_dirty(false);
            app.refresh();
        })
    });
    let weak = ui.as_weak();
    let t = tx.clone();
    ui.global::<AppState>().on_stages_apply(move || {
        let Some(ui) = weak.upgrade() else { return };
        let xy = ui.global::<AppState>().get_xy_separate();
        let stages: Vec<StageDpi> = ui
            .global::<AppState>()
            .get_stages()
            .iter()
            .map(|s| {
                let x = s.x.clamp(100, 16000) as u16;
                let y = if xy { s.y.clamp(100, 16000) as u16 } else { x };
                StageDpi::from_xy((x, y))
            })
            .collect();
        let active = ui
            .global::<AppState>()
            .get_active_stage()
            .clamp(1, stages.len() as i32) as u8;
        ui.global::<AppState>().set_stages_dirty(false);
        edit(&t, move |c| set_stages(c, stages, active));
    });
    let t = tx.clone();
    ui.global::<AppState>().on_polling_changed(move |i| {
        if let Some(&hz) = POLL_RATES.get(i as usize) {
            edit(&t, move |c| c.viper.polling_hz = Some(hz));
        }
    });
    let t = tx.clone();
    ui.global::<AppState>().on_brightness_changed(move |v| {
        let b = v.clamp(0, 255) as u8;
        edit(&t, move |c| c.viper.brightness = Some(b));
    });
    let t = tx.clone();
    ui.global::<AppState>().on_logo_changed(move |i, hex| {
        let Some(&(effect, _)) = LOGO_EFFECTS.get(i as usize) else {
            return;
        };
        let hex = hex.trim().trim_start_matches('#').to_lowercase();
        if parse_rgb(&hex).is_err() {
            log(&format!("ignoring invalid colour {hex:?}"));
            return;
        }
        edit(&t, move |c| {
            c.viper.logo = Some(razer_settings::Logo {
                effect,
                color: Some(hex),
            })
        });
    });
    let t = tx.clone();
    ui.global::<AppState>().on_anc_changed(move |i| {
        if let Some(&(n, _)) = NOISE.get(i as usize) {
            edit(&t, move |c| c.earbuds.noise_control = Some(n));
        }
    });
    let t = tx.clone();
    ui.global::<AppState>().on_eq_changed(move |i| {
        if let Some(&(e, _)) = EQS.get(i as usize) {
            edit(&t, move |c| c.earbuds.eq = Some(e));
        }
    });
    let t = tx.clone();
    ui.global::<AppState>().on_timeout_changed(move |i| {
        if let Some(&m) = POWER.get(i as usize) {
            edit(&t, move |c| menu::set_power(&mut c.earbuds, m));
        }
    });
    let t = tx.clone();
    ui.global::<AppState>().on_mic_changed(move |on| {
        let _ = t.send(Cmd::MicOn(on));
    });
    let t = tx.clone();
    ui.global::<AppState>().on_toggle_changed(move |key, on| {
        if let Some(toggle) = Toggle::from_key(&key) {
            edit(&t, move |c| toggle.set(&mut c.earbuds, on));
        }
    });
    ui.global::<AppState>().on_autostart_changed(|on| {
        if let Err(e) = platform::set_autostart(on) {
            log(&format!("autostart: {e:#}"));
        }
        with_app(|app| app.refresh());
    });
    ui.global::<AppState>().on_open_config(|| {
        if let Err(e) = platform::open_config() {
            log(&format!("open config: {e:#}"));
        }
    });
}

impl App {
    fn show_settings(&mut self) {
        if let Err(e) = self.ui.show() {
            log(&format!("showing settings: {e}"));
        }
    }

    fn on_menu(&mut self, id: &str) {
        let stages = self
            .snapshot
            .as_ref()
            .and_then(|s| s.status.viper.as_ref()?.stages.clone());
        match self.menu.action(id, stages) {
            Some(Action::Edit(f)) => {
                let _ = self.tx.send(Cmd::Edit(f));
            }
            Some(Action::Mic(on)) => {
                let _ = self.tx.send(Cmd::MicOn(on));
            }
            Some(Action::Settings) => self.show_settings(),
            Some(Action::Autostart(on)) => {
                if let Err(e) = platform::set_autostart(on) {
                    log(&format!("autostart: {e:#}"));
                }
            }
            Some(Action::OpenConfig) => {
                if let Err(e) = platform::open_config() {
                    log(&format!("open config: {e:#}"));
                }
            }
            Some(Action::Quit) => {
                let _ = self.tx.send(Cmd::Quit);
                let _ = slint::quit_event_loop();
            }
            None => log(&format!("unknown menu id {id:?}")),
        }
        // Clicking a check item flips it locally; put marks back to the known state
        // until the worker's next snapshot arrives.
        self.refresh();
    }

    fn apply(&mut self, snap: Snapshot) {
        self.snapshot = Some(snap);
        self.refresh();
    }

    fn refresh(&mut self) {
        let autostart = platform::autostart_enabled();
        let Some(s) = &self.snapshot else { return };
        self.menu.update(s, autostart);
        let _ = self.tray.set_tooltip(Some(tooltip(s)));
        let state = icon_state(s);
        if state != self.icon_state {
            let _ = self.tray.set_icon(Some(icon::icon(state)));
            self.icon_state = state;
        }
        update_ui(&self.ui, s, autostart);
    }
}

fn tooltip(s: &Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &s.status.viper {
        parts.push(format!("Viper {} DPI", v.dpi));
    }
    match &s.status.earbuds {
        Some(e) if e.connected => parts.push(format!("Buds {}", battery_text(e))),
        Some(_) => parts.push("Buds not connected".into()),
        None => {}
    }
    if parts.is_empty() {
        "openrazer-win: no devices".into()
    } else {
        parts.join(" · ")
    }
}

fn icon_state(s: &Snapshot) -> icon::State {
    let low = s
        .status
        .earbuds
        .as_ref()
        .and_then(|e| e.battery)
        .is_some_and(|b| [b.left, b.right].into_iter().flatten().any(|p| p <= 20));
    if low {
        icon::State::LowBattery
    } else if s.status.viper.is_none() && s.status.earbuds.is_none() {
        icon::State::Idle
    } else {
        icon::State::Ok
    }
}

fn index_of<T: PartialEq>(items: &[(T, &str)], v: Option<T>) -> i32 {
    v.and_then(|v| items.iter().position(|(x, _)| *x == v))
        .map_or(-1, |i| i as i32)
}

fn update_ui(ui: &SettingsWindow, s: &Snapshot, autostart: bool) {
    let v = s.status.viper.as_ref();
    let cv = &s.config.viper;
    ui.global::<AppState>().set_viper_present(v.is_some());
    if let Some(v) = v {
        ui.global::<AppState>().set_viper_name(v.name.into());
    }
    if let Some(d) = v.map(|v| v.dpi).or(cv.dpi) {
        ui.global::<AppState>().set_dpi(d as i32);
    }
    if let Some(d) = v.map(|v| v.dpi_y).or(cv.dpi_y).or(cv.dpi) {
        ui.global::<AppState>().set_dpi_y(d as i32);
    }
    // Keep stage edits in progress.
    if !ui.global::<AppState>().get_stages_dirty() {
        let st = v.and_then(|v| v.stages.clone()).or_else(|| cv.stages());
        if let Some(st) = st {
            let differ = st.stages.iter().any(|&(x, y)| x != y);
            let model: Vec<StageValue> = st
                .stages
                .iter()
                .map(|&(x, y)| StageValue {
                    x: x as i32,
                    y: y as i32,
                })
                .collect();
            let current: Vec<StageValue> = ui.global::<AppState>().get_stages().iter().collect();
            if current != model {
                ui.global::<AppState>()
                    .set_stages(slint::ModelRc::new(slint::VecModel::from(model)));
            }
            ui.global::<AppState>().set_active_stage(st.active as i32);
            ui.global::<AppState>()
                .set_stage_count_index(st.stages.len() as i32 - 1);
            if differ {
                ui.global::<AppState>().set_xy_separate(true);
            }
        }
    }
    if v.is_some_and(|v| v.dpi != v.dpi_y) {
        ui.global::<AppState>().set_xy_separate(true);
    }
    if let Some(hz) = v.map(|v| v.polling_hz).or(cv.polling_hz) {
        ui.global::<AppState>().set_polling_index(
            POLL_RATES
                .iter()
                .position(|&h| h == hz)
                .map_or(-1, |i| i as i32),
        );
    }
    if let Some(b) = v.map(|v| v.brightness).or(cv.brightness) {
        ui.global::<AppState>().set_brightness(b as i32);
    }
    let effect: Option<LogoEffect> = cv.logo.as_ref().map(|l| l.effect);
    ui.global::<AppState>()
        .set_logo_effect_index(index_of(LOGO_EFFECTS, effect));
    if let Some(c) = cv.logo.as_ref().and_then(|l| l.color.clone()) {
        ui.global::<AppState>().set_logo_color(c.into());
    }

    let e = s.status.earbuds.as_ref();
    let ce = &s.config.earbuds;
    ui.global::<AppState>().set_dongle_present(e.is_some());
    ui.global::<AppState>()
        .set_buds_connected(e.is_some_and(|e| e.connected));
    let battery = e.filter(|e| e.connected).and_then(|e| e.battery);
    let pct = |p: Option<u8>| p.map_or(-1, i32::from);
    ui.global::<AppState>()
        .set_battery_left(pct(battery.and_then(|b| b.left)));
    ui.global::<AppState>()
        .set_battery_right(pct(battery.and_then(|b| b.right)));
    let noise: Option<NoiseControl> = e.and_then(|e| e.noise_control).or(ce.noise_control);
    ui.global::<AppState>()
        .set_anc_index(index_of(NOISE, noise));
    let eq: Option<Eq> = ce.eq;
    ui.global::<AppState>().set_eq_index(index_of(EQS, eq));
    let power = match ce.power_saving {
        Some(false) => Some(0),
        _ => ce.power_saving_minutes,
    };
    ui.global::<AppState>().set_timeout_index(
        power
            .and_then(|m| POWER.iter().position(|&x| x == m))
            .map_or(-1, |i| i as i32),
    );
    if let Some(muted) = e.and_then(|e| e.mic_muted) {
        ui.global::<AppState>().set_mic_on(!muted);
    }
    ui.global::<AppState>()
        .set_reduce_calls(Toggle::ReduceCalls.get(ce) == Some(true));
    ui.global::<AppState>()
        .set_dnd(Toggle::Dnd.get(ce) == Some(true));
    ui.global::<AppState>()
        .set_auto_pause(Toggle::AutoPause.get(ce) == Some(true));
    ui.global::<AppState>()
        .set_gestures(Toggle::Gestures.get(ce) == Some(true));
    ui.global::<AppState>()
        .set_prompts(Toggle::Prompts.get(ce) == Some(true));
    ui.global::<AppState>()
        .set_auto_pause_known(ce.auto_pause.is_some());
    ui.global::<AppState>()
        .set_gestures_known(ce.gestures.is_some());
    ui.global::<AppState>()
        .set_prompts_known(ce.audio_prompts.is_some());

    ui.global::<AppState>().set_autostart(autostart);
    ui.global::<AppState>()
        .set_last_event(s.last_event.clone().into());
}
