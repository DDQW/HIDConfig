#![windows_subsystem = "windows"]
//! Tray app: icon + menu for quick changes, a settings window for everything else.
//! Device I/O happens on the worker thread; this thread only runs the UI.

mod icon;
mod menu;
mod platform;
mod worker;

use std::{cell::RefCell, sync::mpsc::Sender};

use anyhow::Result;
use hidconfig_settings::{Eq, LogoEffect, NoiseControl, StageDpi, parse_rgb};
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
        .with_tooltip("HIDConfig: starting…")
        .with_icon(icon::icon(icon::State::Idle))
        .build()?;

    let tx = worker::spawn(|snap| {
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.apply(snap)));
    });

    let ui = SettingsWindow::new()?;
    ui.global::<AppState>().set_config_path(
        hidconfig_settings::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.global::<AppState>().set_autostart(autostart);
    wire_ui(&ui, &tx);
    wire_windows(&ui, &tx);
    wire_audio(&ui, &tx);
    wire_pads(&ui);
    wire_display(&ui, &tx);
    wire_graphics(&ui, &tx);
    // Number fields: accept both decimal separators; -1 000 000 means "not a number".
    ui.global::<Num>().on_parse(|t| {
        let t: String = t
            .trim()
            .replace(',', ".")
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        t.parse::<f32>().unwrap_or(-1_000_000.0)
    });

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
pub fn set_stages(c: &mut hidconfig_settings::Config, stages: Vec<StageDpi>, active: u8) {
    c.viper.dpi_stages = Some(stages);
    c.viper.dpi_stage = Some(active);
    c.viper.dpi = None;
    c.viper.dpi_y = None;
}

/// Sends a config edit to the worker.
fn edit(tx: &Sender<Cmd>, f: impl FnOnce(&mut hidconfig_settings::Config) + Send + 'static) {
    let _ = tx.send(Cmd::Edit(Box::new(f)));
}

/// Runs `f` against the latest snapshot's audio state.
fn with_audio<R>(f: impl FnOnce(&hidconfig_settings::AudioLive) -> Option<R>) -> Option<R> {
    let mut out = None;
    with_app(|app| out = app.snapshot.as_ref().and_then(|s| f(&s.status.audio)));
    out
}

fn device_name(id: &str) -> Option<String> {
    with_audio(|a| {
        a.endpoints
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.name.clone())
    })
}

fn live(tx: &Sender<Cmd>, op: impl FnOnce() -> anyhow::Result<()> + Send + 'static) {
    let _ = tx.send(Cmd::Audio(Box::new(op)));
}

fn wire_audio(ui: &SettingsWindow, tx: &Sender<Cmd>) {
    use hidconfig_win::audio::{self, Ducking, Flow, SampleFormat};
    let a = ui.global::<AudioState>();

    let t = tx.clone();
    a.on_volume(move |id, v| {
        let id = id.to_string();
        live(&t, move || audio::set_volume(&id, v / 100.0));
    });
    let t = tx.clone();
    a.on_mute(move |id, m| {
        let id = id.to_string();
        live(&t, move || audio::set_mute(&id, m));
    });
    let t = tx.clone();
    a.on_app_volume(move |pid, v| {
        live(&t, move || {
            audio::set_session(pid as u32, Some(v / 100.0), None).map(|_| ())
        });
    });
    let t = tx.clone();
    a.on_app_mute(move |pid, m| {
        live(&t, move || {
            audio::set_session(pid as u32, None, Some(m)).map(|_| ())
        });
    });

    // Profile settings: saved, then applied by the watcher.
    let t = tx.clone();
    a.on_make_default(move |id| {
        let Some((name, flow)) = with_audio(|a| {
            a.endpoints
                .iter()
                .find(|e| e.id == id.as_str())
                .map(|e| (e.name.clone(), e.flow))
        }) else {
            return;
        };
        edit(&t, move |c| match flow {
            Flow::Output => c.windows.audio.default_output = Some(name),
            Flow::Input => c.windows.audio.default_input = Some(name),
        });
    });
    let t = tx.clone();
    a.on_spatial(move |id, index| {
        let Some((name, key)) = with_audio(|a| {
            let name = a
                .endpoints
                .iter()
                .find(|e| e.id == id.as_str())?
                .name
                .clone();
            let available = &a.details.get(id.as_str())?.spatial.as_ref()?.available;
            let key = if index == 0 {
                "off"
            } else {
                available.get(index as usize - 1)?
            };
            Some((name, key.to_string()))
        }) else {
            return;
        };
        edit(&t, move |c| {
            c.windows.audio.devices.entry(name).or_default().spatial = Some(key)
        });
    });
    let t = tx.clone();
    a.on_format(move |id, index| {
        let Some((name, f)) = with_audio(|a| {
            let name = a
                .endpoints
                .iter()
                .find(|e| e.id == id.as_str())?
                .name
                .clone();
            let f: SampleFormat = *a
                .details
                .get(id.as_str())?
                .options
                .as_ref()?
                .formats
                .get(index as usize)?;
            Some((name, f))
        }) else {
            return;
        };
        edit(&t, move |c| {
            let d = c.windows.audio.devices.entry(name).or_default();
            d.sample_rate = Some(f.rate);
            d.bit_depth = Some(f.bits);
        });
    });
    let t = tx.clone();
    a.on_enhancements(move |id, on| {
        let Some(name) = device_name(&id) else { return };
        edit(&t, move |c| {
            c.windows
                .audio
                .devices
                .entry(name)
                .or_default()
                .enhancements = Some(on)
        });
    });
    let t = tx.clone();
    a.on_exclusive(move |id, allowed, priority| {
        let Some(name) = device_name(&id) else { return };
        edit(&t, move |c| {
            let d = c.windows.audio.devices.entry(name).or_default();
            d.exclusive_allowed = Some(allowed);
            d.exclusive_priority = Some(priority);
        });
    });
    let t = tx.clone();
    a.on_comms(move |output, index| {
        let flow = if output { Flow::Output } else { Flow::Input };
        let Some(name) = with_audio(|a| {
            let mut devs = a.endpoints.iter().filter(|e| e.flow == flow);
            if index == 0 {
                // "Same as default": follow the current default device.
                devs.find(|e| e.is_default).map(|e| e.name.clone())
            } else {
                devs.nth(index as usize - 1).map(|e| e.name.clone())
            }
        }) else {
            return;
        };
        edit(&t, move |c| {
            if output {
                c.windows.audio.comms_output = Some(name);
            } else {
                c.windows.audio.comms_input = Some(name);
            }
        });
    });
    let t = tx.clone();
    a.on_ducking(move |index| {
        if let Some(&d) = Ducking::ALL.get(index as usize) {
            edit(&t, move |c| c.windows.audio.ducking = Some(d));
        }
    });
    let t = tx.clone();
    a.on_mono_changed(move |on| edit(&t, move |c| c.windows.audio.mono = Some(on)));
}

/// Updates a list model in place when the row count is unchanged, so rows (and a
/// slider being dragged) aren't recreated on every refresh.
fn sync_model<T: Clone + PartialEq + 'static>(
    current: slint::ModelRc<T>,
    new: Vec<T>,
) -> Option<slint::ModelRc<T>> {
    use slint::Model;
    if let Some(vec) = current.as_any().downcast_ref::<slint::VecModel<T>>()
        && vec.row_count() == new.len()
    {
        for (i, row) in new.into_iter().enumerate() {
            if vec.row_data(i).as_ref() != Some(&row) {
                vec.set_row_data(i, row);
            }
        }
        return None;
    }
    Some(slint::ModelRc::new(slint::VecModel::from(new)))
}

/// Reuses `prev` when it already holds `new`, so row data compares equal and
/// combo boxes aren't reset.
fn reuse_strings(
    prev: Option<slint::ModelRc<slint::SharedString>>,
    new: Vec<slint::SharedString>,
) -> slint::ModelRc<slint::SharedString> {
    use slint::Model;
    match prev {
        Some(p) if p.iter().eq(new.iter().cloned()) => p,
        _ => slint::ModelRc::new(slint::VecModel::from(new)),
    }
}

fn update_audio(ui: &SettingsWindow, s: &Snapshot) {
    use hidconfig_win::audio::{self, Flow};
    let a = ui.global::<AudioState>();
    let live = &s.status.audio;
    let previous: Vec<AudioDev> = {
        use slint::Model;
        a.get_outputs()
            .iter()
            .chain(a.get_inputs().iter())
            .collect()
    };
    let prev_dev = |id: &str| previous.iter().find(|d| d.id == id).cloned();
    let to_dev = |e: &audio::Endpoint| {
        let detail = live.details.get(&e.id);
        let spatial = detail.and_then(|d| d.spatial.as_ref());
        let opts = detail.and_then(|d| d.options.as_ref());
        let mut spatial_names = vec![slint::SharedString::from("Off")];
        let mut spatial_index = 0;
        let mut note = String::new();
        if let Some(sp) = spatial {
            for (i, key) in sp.available.iter().enumerate() {
                spatial_names.push(audio::spatial_display_name(key).into());
                if sp.selected.as_deref() == Some(*key) {
                    spatial_index = i as i32 + 1;
                }
            }
            if sp.selected != sp.active {
                note = format!(
                    "Selected, but {} is active",
                    sp.active
                        .as_deref()
                        .map_or("nothing", audio::spatial_display_name)
                );
            }
        }
        let formats: Vec<slint::SharedString> = opts.map_or(vec![], |o| {
            o.formats.iter().map(|f| f.to_string().into()).collect()
        });
        let format_index = opts
            .and_then(|o| {
                o.format
                    .and_then(|f| o.formats.iter().position(|x| *x == f))
            })
            .map_or(-1, |i| i as i32);
        AudioDev {
            id: e.id.clone().into(),
            name: e.name.clone().into(),
            output: e.flow == Flow::Output,
            is_default: e.is_default,
            is_comms: e.is_default_comms,
            volume: e.volume * 100.0,
            muted: e.muted,
            spatial_supported: spatial.is_some_and(|s| s.supported),
            spatial_names: reuse_strings(prev_dev(&e.id).map(|d| d.spatial_names), spatial_names),
            spatial_index,
            spatial_note: note.into(),
            formats: reuse_strings(prev_dev(&e.id).map(|d| d.formats), formats),
            format_index,
            has_enhancements: opts.is_some_and(|o| o.enhancements.is_some()),
            enhancements: opts.and_then(|o| o.enhancements).unwrap_or(false),
            exclusive: opts.is_some_and(|o| o.exclusive_allowed),
            exclusive_priority: opts.is_some_and(|o| o.exclusive_priority),
        }
    };
    let model = |flow| -> Vec<AudioDev> {
        live.endpoints
            .iter()
            .filter(|e| e.flow == flow)
            .map(to_dev)
            .collect()
    };
    if let Some(m) = sync_model(a.get_outputs(), model(Flow::Output)) {
        a.set_outputs(m);
    }
    if let Some(m) = sync_model(a.get_inputs(), model(Flow::Input)) {
        a.set_inputs(m);
    }
    let apps: Vec<AudioApp> = live
        .sessions
        .iter()
        .map(|x| AudioApp {
            pid: x.pid as i32,
            name: x.app.clone().into(),
            volume: x.volume * 100.0,
            muted: x.muted,
        })
        .collect();
    if let Some(m) = sync_model(a.get_apps(), apps) {
        a.set_apps(m);
    }

    for (flow, set_names, set_index) in [
        (
            Flow::Output,
            &(|n| a.set_comms_output_names(n)) as &dyn Fn(slint::ModelRc<slint::SharedString>),
            &(|i| a.set_comms_output_index(i)) as &dyn Fn(i32),
        ),
        (
            Flow::Input,
            &(|n| a.set_comms_input_names(n)) as &dyn Fn(slint::ModelRc<slint::SharedString>),
            &(|i| a.set_comms_input_index(i)) as &dyn Fn(i32),
        ),
    ] {
        let devs: Vec<&audio::Endpoint> =
            live.endpoints.iter().filter(|e| e.flow == flow).collect();
        let mut names = vec![slint::SharedString::from("Same as default")];
        names.extend(
            devs.iter()
                .map(|e| slint::SharedString::from(e.name.as_str())),
        );
        set_names(slint::ModelRc::new(slint::VecModel::from(names)));
        let comms = devs.iter().position(|e| e.is_default_comms);
        let same = devs.iter().any(|e| e.is_default && e.is_default_comms);
        set_index(if same {
            0
        } else {
            comms.map_or(0, |i| i as i32 + 1)
        });
    }
    if let Some(w) = &s.status.windows {
        if let Some(d) = w.audio.ducking {
            a.set_ducking_index(
                audio::Ducking::ALL
                    .iter()
                    .position(|x| *x == d)
                    .unwrap_or(1) as i32,
            );
        }
        a.set_mono(w.audio.mono.unwrap_or(false));
    }
}

/// Stick samples while a drift measurement runs: (start, left samples, right samples).
type DriftRun = (std::time::Instant, Vec<(f64, f64)>, Vec<(f64, f64)>);

thread_local! {
    static DRIFT: RefCell<Option<DriftRun>> = const { RefCell::new(None) };
}

fn wire_pads(ui: &SettingsWindow) {
    use hidconfig_win::gamepad as gp;
    let p = ui.global::<PadState>();
    let names: Vec<slint::SharedString> = gp::BUTTONS.iter().map(|(_, n)| (*n).into()).collect();
    p.set_button_names(slint::ModelRc::new(slint::VecModel::from(names)));

    let weak = ui.as_weak();
    p.on_poll(move || {
        let Some(ui) = weak.upgrade() else { return };
        let p = ui.global::<PadState>();
        let Ok(r) = gp::reading(p.get_selected().max(0) as u32) else {
            return;
        };
        p.set_lx(r.left.0 as f32);
        p.set_ly(r.left.1 as f32);
        p.set_rx(r.right.0 as f32);
        p.set_ry(r.right.1 as f32);
        p.set_lt(r.left_trigger as f32);
        p.set_rt(r.right_trigger as f32);
        let pressed: Vec<bool> = gp::BUTTONS
            .iter()
            .map(|(b, _)| r.buttons & b != 0)
            .collect();
        p.set_buttons(slint::ModelRc::new(slint::VecModel::from(pressed)));
        DRIFT.with(|d| {
            let mut d = d.borrow_mut();
            let Some((start, left, right)) = d.as_mut() else {
                return;
            };
            left.push(r.left);
            right.push(r.right);
            if start.elapsed() >= std::time::Duration::from_secs(2) {
                let describe = |name: &str, samples: &[(f64, f64)]| {
                    gp::drift(samples).map_or(String::new(), |dr| {
                        format!(
                            "{name}: rests {:.0}% off centre → dead zone {:.0}% hides it",
                            dr.max * 100.0,
                            dr.suggested_deadzone * 100.0
                        )
                    })
                };
                let text = format!(
                    "{}\n{}\nMost games use about 10–25%; under 10% is healthy.",
                    describe("Left stick", left),
                    describe("Right stick", right)
                );
                p.set_drift_text(text.into());
                p.set_measuring(false);
                *d = None;
            }
        });
    });
    let weak = ui.as_weak();
    p.on_measure_drift(move || {
        if let Some(ui) = weak.upgrade() {
            ui.global::<PadState>().set_measuring(true);
            ui.global::<PadState>().set_drift_text("".into());
        }
        DRIFT.with(|d| *d.borrow_mut() = Some((std::time::Instant::now(), vec![], vec![])));
    });
    let weak = ui.as_weak();
    p.on_rumble(move |which| {
        let Some(ui) = weak.upgrade() else { return };
        let index = ui.global::<PadState>().get_selected().max(0) as u32;
        let (l, r, lt, rt) = match which.as_str() {
            "left" => (0.6, 0.0, 0.0, 0.0),
            "right" => (0.0, 0.6, 0.0, 0.0),
            "triggers" => (0.0, 0.0, 0.6, 0.6),
            _ => (0.5, 0.5, 0.5, 0.5),
        };
        std::thread::spawn(move || {
            if let Err(e) = gp::rumble(index, l, r, lt, rt) {
                log(&format!("rumble: {e:#}"));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = gp::rumble(index, 0.0, 0.0, 0.0, 0.0);
        });
    });
    p.on_adapter_power_saving_changed(|on| {
        if let Err(e) = platform::run_elevated_ctl(&[
            "gamepad",
            "adapter-power-saving",
            if on { "on" } else { "off" },
        ]) {
            log(&format!("adapter power saving: {e:#}"));
        }
        with_app(|app| {
            let _ = app.tx.send(Cmd::RefreshStatus);
        });
    });
}

fn update_pads(ui: &SettingsWindow, s: &Snapshot) {
    let p = ui.global::<PadState>();
    let pads: Vec<PadInfo> = s
        .status
        .pads
        .iter()
        .map(|x| PadInfo {
            index: x.index as i32,
            name: x.name.clone().into(),
            wireless: x.wireless,
            battery: x.battery.map_or(-1, i32::from),
            charging: x.charging,
        })
        .collect();
    if p.get_selected() >= pads.len() as i32 {
        p.set_selected(0);
    }
    if let Some(m) = sync_model(p.get_pads(), pads) {
        p.set_pads(m);
    }
    p.set_adapter_present(s.status.adapter.is_some());
    if let Some(a) = &s.status.adapter {
        p.set_adapter_name(a.name.clone().into());
        p.set_adapter_driver(a.driver_version.clone().into());
        p.set_adapter_power_saving(a.power_saving_allowed.unwrap_or(false));
    }
    if let Some(g) = s.status.windows.as_ref().map(|w| &w.gaming) {
        p.set_xbox_button_game_bar(g.xbox_button_opens_game_bar.unwrap_or(false));
        p.set_game_mode(g.game_mode.unwrap_or(false));
        p.set_game_capture(g.game_capture.unwrap_or(false));
        p.set_background_recording(g.background_recording.unwrap_or(false));
    }
}

thread_local! {
    /// Display mode to go back to if a change isn't confirmed: (device, mode, rotation).
    static REVERT: RefCell<Option<(String, hidconfig_win::display::Mode, u32)>> = const { RefCell::new(None) };
}

fn display_op(tx: &Sender<Cmd>, op: impl FnOnce() -> anyhow::Result<()> + Send + 'static) {
    let _ = tx.send(Cmd::Display(Box::new(op)));
}

fn with_display<R>(
    device: &str,
    f: impl FnOnce(&hidconfig_win::display::Display) -> Option<R>,
) -> Option<R> {
    let mut out = None;
    with_app(|app| {
        out = app
            .snapshot
            .as_ref()
            .and_then(|s| s.status.displays.as_ref())
            .and_then(|ds| ds.iter().find(|d| d.device == device))
            .and_then(f);
    });
    out
}

/// Resolutions (largest first) and the refresh rates for a resolution.
fn resolutions(d: &hidconfig_win::display::Display) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = d.modes.iter().map(|m| (m.width, m.height)).collect();
    v.dedup();
    v
}

fn rates(d: &hidconfig_win::display::Display, w: u32, h: u32) -> Vec<u32> {
    let mut v: Vec<u32> = d
        .modes
        .iter()
        .filter(|m| m.width == w && m.height == h)
        .map(|m| m.hz)
        .collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    v
}

/// Applies a mode change and arms the 15 s revert.
fn change_mode(
    ui: &slint::Weak<SettingsWindow>,
    tx: &Sender<Cmd>,
    device: &str,
    mode: hidconfig_win::display::Mode,
    rotation: Option<u32>,
) {
    use hidconfig_win::display as d;
    let Some((cur, cur_rot)) = with_display(device, |x| Some((x.mode, x.rotation))) else {
        return;
    };
    if cur == mode && rotation.is_none_or(|r| r == cur_rot) {
        return;
    }
    REVERT.with(|r| *r.borrow_mut() = Some((device.to_string(), cur, cur_rot)));
    let dev = device.to_string();
    display_op(tx, move || d::set_mode(&dev, mode, rotation));
    if let Some(ui) = ui.upgrade() {
        let st = ui.global::<DisplayState>();
        st.set_confirm_seconds(15);
        st.set_confirm_pending(true);
    }
}

fn wire_display(ui: &SettingsWindow, tx: &Sender<Cmd>) {
    use hidconfig_win::display::{self as d, Mode, vcp};
    let st = ui.global::<DisplayState>();

    let (w, t) = (ui.as_weak(), tx.clone());
    st.on_resolution(move |dev, i| {
        let Some(mode) = with_display(&dev, |x| {
            let (rw, rh) = *resolutions(x).get(i as usize)?;
            // Keep the refresh rate if this resolution offers it, else take the highest.
            let rs = rates(x, rw, rh);
            let hz = if rs.contains(&x.mode.hz) {
                x.mode.hz
            } else {
                *rs.first()?
            };
            Some(Mode {
                width: rw,
                height: rh,
                hz,
            })
        }) else {
            return;
        };
        change_mode(&w, &t, &dev, mode, None);
    });
    let (w, t) = (ui.as_weak(), tx.clone());
    st.on_rate(move |dev, i| {
        let Some(mode) = with_display(&dev, |x| {
            let hz = *rates(x, x.mode.width, x.mode.height).get(i as usize)?;
            Some(Mode { hz, ..x.mode })
        }) else {
            return;
        };
        change_mode(&w, &t, &dev, mode, None);
    });
    let (w, t) = (ui.as_weak(), tx.clone());
    st.on_rotation(move |dev, i| {
        let Some(mode) = with_display(&dev, |x| Some(x.mode)) else {
            return;
        };
        change_mode(&w, &t, &dev, mode, Some(i as u32 * 90));
    });
    let t = tx.clone();
    st.on_scale(move |dev, i| {
        let Some(pct) = with_display(&dev, |x| x.scales.get(i as usize).copied()) else {
            return;
        };
        let dev = dev.to_string();
        display_op(&t, move || d::set_scale(&dev, pct));
    });
    let t = tx.clone();
    st.on_hdr(move |dev, on| {
        let dev = dev.to_string();
        display_op(&t, move || d::set_hdr(&dev, on));
    });
    let t = tx.clone();
    st.on_vcp(move |dev, code, value| {
        let dev = dev.to_string();
        display_op(&t, move || {
            d::set_vcp(&dev, code as u8, value.max(0) as u32)
        });
    });
    let t = tx.clone();
    st.on_preset(move |dev, i| {
        let Some(v) = with_display(&dev, |x| {
            x.ddc
                .as_ref()?
                .caps
                .get(&vcp::COLOR_PRESET)?
                .get(i as usize)
                .copied()
        }) else {
            return;
        };
        let dev = dev.to_string();
        display_op(&t, move || d::set_vcp(&dev, vcp::COLOR_PRESET, v as u32));
    });
    let t = tx.clone();
    st.on_input(move |dev, i| {
        let Some(v) = with_display(&dev, |x| {
            x.ddc
                .as_ref()?
                .caps
                .get(&vcp::INPUT)?
                .get(i as usize)
                .copied()
        }) else {
            return;
        };
        let dev = dev.to_string();
        display_op(&t, move || d::set_vcp(&dev, vcp::INPUT, v as u32));
    });
    let t = tx.clone();
    st.on_kelvin(move |dev, k| {
        let dev = dev.to_string();
        display_op(&t, move || d::set_color_temp(&dev, k.max(0) as u32));
    });
    let t = tx.clone();
    st.on_language(move |dev, i| {
        let Some(v) = with_display(&dev, |x| {
            x.ddc
                .as_ref()?
                .caps
                .get(&vcp::OSD_LANGUAGE)?
                .get(i as usize)
                .copied()
        }) else {
            return;
        };
        let dev = dev.to_string();
        display_op(&t, move || d::set_vcp(&dev, vcp::OSD_LANGUAGE, v as u32));
    });
    let t = tx.clone();
    st.on_buttons_locked(move |dev, lock| {
        let dev = dev.to_string();
        let v = if lock {
            vcp::OSD_LOCKED
        } else {
            vcp::OSD_UNLOCKED
        };
        display_op(&t, move || d::set_vcp(&dev, vcp::OSD, v));
    });
    let t = tx.clone();
    st.on_reset(move |dev, code| {
        let dev = dev.to_string();
        display_op(&t, move || d::set_vcp(&dev, code as u8, 1));
    });
    let w = ui.as_weak();
    st.on_scan(move |dev| {
        let Some(ui) = w.upgrade() else { return };
        let st = ui.global::<DisplayState>();
        st.set_scan_device(dev.clone());
        st.set_scanning(true);
        st.set_scan_lines(slint::ModelRc::default());
        let dev = dev.to_string();
        let w = w.clone();
        std::thread::spawn(move || {
            let lines: Vec<String> = match d::scan(&dev) {
                Ok(v) => v
                    .iter()
                    .map(|v| {
                        format!(
                            "{:02X}  {:<28} {:>6} / {:<6} {}{}",
                            v.code,
                            d::vcp_name(v.code),
                            v.current,
                            v.max,
                            if v.momentary { "action" } else { "value" },
                            if v.listed { "" } else { "  (not advertised)" }
                        )
                    })
                    .collect(),
                Err(e) => vec![format!("scan failed: {e:#}")],
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = w.upgrade() {
                    let st = ui.global::<DisplayState>();
                    let model: Vec<slint::SharedString> =
                        lines.into_iter().map(Into::into).collect();
                    st.set_scan_lines(slint::ModelRc::new(slint::VecModel::from(model)));
                    st.set_scanning(false);
                }
            });
        });
    });
    let w = ui.as_weak();
    st.on_keep(move || {
        REVERT.with(|r| *r.borrow_mut() = None);
        if let Some(ui) = w.upgrade() {
            ui.global::<DisplayState>().set_confirm_pending(false);
        }
    });
    let (w, t) = (ui.as_weak(), tx.clone());
    st.on_revert(move || {
        if let Some((dev, mode, rot)) = REVERT.with(|r| r.borrow_mut().take()) {
            display_op(&t, move || d::set_mode(&dev, mode, Some(rot)));
        }
        if let Some(ui) = w.upgrade() {
            ui.global::<DisplayState>().set_confirm_pending(false);
        }
    });
}

thread_local! {
    /// Catalog hits and downloaded profile files of the current finder run.
    static FINDER: RefCell<(Vec<hidconfig_win::icc::CatalogHit>, Vec<std::path::PathBuf>)> = RefCell::default();
}

fn finder_update(w: &slint::Weak<SettingsWindow>, busy: bool, status: String) {
    let w = w.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = w.upgrade() else { return };
        let g = ui.global::<GfxState>();
        g.set_finder_busy(busy);
        g.set_finder_status(status.into());
        FINDER.with(|f| {
            let f = f.borrow();
            let hits: Vec<ProfileHit> =
                f.0.iter()
                    .map(|h| ProfileHit {
                        id: h.id.clone().into(),
                        title: h.title.clone().into(),
                        detail: format!("{} · {} · {}", h.products, h.date, h.size).into(),
                    })
                    .collect();
            g.set_finder_hits(slint::ModelRc::new(slint::VecModel::from(hits)));
            let files: Vec<slint::SharedString> =
                f.1.iter().map(|p| p.display().to_string().into()).collect();
            g.set_finder_files(slint::ModelRc::new(slint::VecModel::from(files)));
        });
    });
}

fn wire_graphics(ui: &SettingsWindow, tx: &Sender<Cmd>) {
    use hidconfig_win::{display as d, graphics as g, icc};
    let gs = ui.global::<GfxState>();
    gs.on_displays_off(g::displays_off);
    let t = tx.clone();
    gs.on_gpu_scheduling_changed(move |on| {
        if let Err(e) = platform::run_elevated_ctl(&[
            "display",
            "gpu-scheduling",
            if on { "on" } else { "off" },
        ]) {
            log(&format!("GPU scheduling: {e:#}"));
        }
        let _ = t.send(Cmd::RefreshStatus);
    });
    let t = tx.clone();
    gs.on_sdr(move |dev, nits| {
        let dev = dev.to_string();
        display_op(&t, move || d::set_sdr_brightness(&dev, nits.max(80) as u32));
    });
    let t = tx.clone();
    gs.on_set_profile(move |dev, index| {
        let Some((key, profile)) = with_display(&dev, |x| {
            let key = x.monitor_key.clone()?;
            let profile = (index >= 0)
                .then(|| {
                    g::color_profiles(&key)
                        .installed
                        .get(index as usize)
                        .cloned()
                })
                .flatten();
            Some((key, profile))
        }) else {
            return;
        };
        display_op(&t, move || g::set_color_profile(&key, profile.as_deref()));
    });
    gs.on_web_search(|dev| {
        if let Some((vendor, model)) = with_display(&dev, |x| {
            Some((icc::vendor_name(&x.vendor).to_string(), x.name.clone()))
        }) && let Err(e) = icc::web_search(&vendor, &model)
        {
            log(&format!("web search: {e:#}"));
        }
    });
    let w = ui.as_weak();
    gs.on_find_online(move |dev| {
        let Some((vendor, models)) = with_display(&dev, |x| {
            let mut m = vec![x.name.clone()];
            if let Some(model) = x.ddc.as_ref().and_then(|d| d.model.clone()) {
                m.push(model);
            }
            Some((icc::vendor_name(&x.vendor).to_string(), m))
        }) else {
            return;
        };
        if let Some(ui) = w.upgrade() {
            ui.global::<GfxState>().set_finder_device(dev.clone());
        }
        FINDER.with(|f| *f.borrow_mut() = Default::default());
        finder_update(&w, true, format!("Searching the Microsoft Update Catalog for {vendor} {}…", models.join(" / ")));
        let w = w.clone();
        std::thread::spawn(move || {
            let mut hits: Vec<icc::CatalogHit> = Vec::new();
            let mut error = None;
            for m in &models {
                match icc::search_for_monitor(&vendor, m) {
                    Ok(found) => {
                        for h in found {
                            if !hits.iter().any(|x| x.id == h.id) {
                                hits.push(h);
                            }
                        }
                    }
                    Err(e) => error = Some(format!("{e:#}")),
                }
            }
            let status = match (&error, hits.len()) {
                (Some(e), 0) => format!("Search failed: {e}"),
                (_, 0) => "Nothing in the Microsoft Update Catalog for this model. Try \"Search the web\" for the maker's download page, then \"Install from file…\".".into(),
                (_, n) => format!("{n} package(s) found. \"Get\" downloads one from Microsoft (a few KB) and shows the profiles inside."),
            };
            let _ = slint::invoke_from_event_loop(move || FINDER.with(|f| f.borrow_mut().0 = hits));
            finder_update(&w, false, status);
        });
    });
    let w = ui.as_weak();
    gs.on_fetch(move |id| {
        finder_update(&w, true, "Downloading from Microsoft…".into());
        let w = w.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let (files, status) = match icc::fetch_profiles(&id) {
                Ok(f) if f.is_empty() => (f, "That package contains no colour profile.".to_string()),
                Ok(f) => {
                    let n = f.len();
                    (f, format!("{n} profile(s) in the package. \"Install & use\" needs administrator rights once."))
                }
                Err(e) => (vec![], format!("Download failed: {e:#}")),
            };
            let _ = slint::invoke_from_event_loop(move || FINDER.with(|f| f.borrow_mut().1 = files));
            finder_update(&w, false, status);
        });
    });
    let (w, t) = (ui.as_weak(), tx.clone());
    gs.on_install_file(move |dev, path| {
        let path = if path.is_empty() {
            match rfd::FileDialog::new()
                .set_title("Choose an ICC colour profile")
                .add_filter("Colour profiles", &["icm", "icc"])
                .pick_file()
            {
                Some(p) => p.display().to_string(),
                None => return,
            }
        } else {
            path.to_string()
        };
        let dev = dev.to_string();
        let r =
            platform::run_elevated_ctl(&["display", "install-profile", &path, "--display", &dev]);
        let status = match r {
            Ok(()) => "Profile installed and set as this monitor's default.".to_string(),
            Err(e) => format!("Install failed: {e:#}"),
        };
        if let Some(ui) = w.upgrade() {
            ui.global::<GfxState>().set_finder_device(dev.into());
        }
        finder_update(&w, false, status);
        let _ = t.send(Cmd::RefreshStatus);
    });
}

fn update_graphics(ui: &SettingsWindow, s: &Snapshot) {
    let gs = ui.global::<GfxState>();
    if let Some(gr) = s.status.windows.as_ref().map(|w| &w.graphics) {
        gs.set_auto_hdr(gr.auto_hdr.unwrap_or(false));
        gs.set_vrr(gr.variable_refresh_rate.unwrap_or(false));
        gs.set_windowed_opt(gr.windowed_game_optimizations.unwrap_or(true));
        gs.set_warm_kelvin(gr.warm_filter_kelvin.unwrap_or(6500) as i32);
        gs.set_gamma(gr.gamma.unwrap_or(1.0) as f32);
        let minutes = [0, 1, 2, 3, 5, 10, 15, 20, 30, 45, 60, 120, 300];
        gs.set_sleep_index(
            gr.display_off_minutes
                .and_then(|m| minutes.iter().position(|&x| x == m))
                .map_or(-1, |i| i as i32),
        );
    }
    let sched = hidconfig_win::graphics::gpu_scheduling();
    gs.set_gpu_scheduling_known(sched.is_some());
    gs.set_gpu_scheduling(sched.unwrap_or(false));
}

fn update_display(ui: &SettingsWindow, s: &Snapshot) {
    use hidconfig_win::display::{self as d, vcp};
    let st = ui.global::<DisplayState>();
    let Some(displays) = &s.status.displays else {
        return;
    };
    st.set_loading(false);
    let previous: Vec<DisplayInfo> = {
        use slint::Model;
        st.get_displays().iter().collect()
    };
    let infos: Vec<DisplayInfo> = displays
        .iter()
        .map(|x| {
            let prev = previous.iter().find(|p| p.device == x.device.as_str());
            let keep = |prev: Option<slint::ModelRc<slint::SharedString>>, new: Vec<String>| {
                reuse_strings(prev, new.into_iter().map(Into::into).collect())
            };
            let res = resolutions(x);
            let rs = rates(x, x.mode.width, x.mode.height);
            let ddc = x.ddc.as_ref();
            let val = |code| ddc.and_then(|c| c.get(code)).map_or(-1, |v| v as i32);
            let list = |code| {
                ddc.and_then(|c| c.caps.get(&code))
                    .cloned()
                    .unwrap_or_default()
            };
            let presets = list(vcp::COLOR_PRESET);
            let inputs = list(vcp::INPUT);
            let langs = list(vcp::OSD_LANGUAGE);
            let cp = x
                .monitor_key
                .as_deref()
                .map(hidconfig_win::graphics::color_profiles);
            let mut profile_names = vec![match &cp {
                Some(c) if !c.per_user => format!(
                    "System default ({})",
                    c.default.as_deref().unwrap_or("none")
                ),
                _ => "System default".to_string(),
            }];
            profile_names.extend(cp.iter().flat_map(|c| c.installed.clone()));
            let profile_index = match &cp {
                Some(c) if c.per_user => c
                    .default
                    .as_ref()
                    .and_then(|d| c.installed.iter().position(|p| p.eq_ignore_ascii_case(d)))
                    .map_or(0, |i| i as i32 + 1),
                _ => 0,
            };
            let ct = ddc.and_then(|c| c.color_temp());
            let cur = |code| ddc.and_then(|c| c.get(code)).map(|v| v as u8);
            DisplayInfo {
                device: x.device.clone().into(),
                name: x.name.clone().into(),
                primary: x.primary,
                resolutions: keep(
                    prev.map(|p| p.resolutions.clone()),
                    res.iter().map(|(w, h)| format!("{w} × {h}")).collect(),
                ),
                resolution_index: res
                    .iter()
                    .position(|&r| r == (x.mode.width, x.mode.height))
                    .map_or(-1, |i| i as i32),
                rates: keep(
                    prev.map(|p| p.rates.clone()),
                    rs.iter().map(|hz| format!("{hz} Hz")).collect(),
                ),
                rate_index: rs
                    .iter()
                    .position(|&hz| hz == x.mode.hz)
                    .map_or(-1, |i| i as i32),
                rotation_index: (x.rotation / 90) as i32,
                scales: keep(
                    prev.map(|p| p.scales.clone()),
                    x.scales.iter().map(|p| format!("{p}%")).collect(),
                ),
                scale_index: x
                    .scale
                    .and_then(|c| x.scales.iter().position(|&p| p == c))
                    .map_or(-1, |i| i as i32),
                hdr_supported: x.hdr_supported,
                hdr_on: x.hdr_on,
                ddc: ddc.is_some(),
                brightness: val(vcp::BRIGHTNESS),
                contrast: val(vcp::CONTRAST),
                sharpness: val(vcp::SHARPNESS),
                sharpness_max: ddc
                    .and_then(|c| c.values.get(&vcp::SHARPNESS))
                    .map_or(100, |v| v.1 as i32),
                red: val(vcp::RED_GAIN),
                green: val(vcp::GREEN_GAIN),
                blue: val(vcp::BLUE_GAIN),
                volume: val(vcp::VOLUME),
                presets: keep(
                    prev.map(|p| p.presets.clone()),
                    presets.iter().map(|v| d::preset_name(*v)).collect(),
                ),
                preset_index: cur(vcp::COLOR_PRESET)
                    .and_then(|c| presets.iter().position(|&v| v == c))
                    .map_or(-1, |i| i as i32),
                inputs: keep(
                    prev.map(|p| p.inputs.clone()),
                    inputs.iter().map(|v| d::input_name(*v)).collect(),
                ),
                kelvin: ct.map_or(-1, |t| t.0 as i32),
                kelvin_min: ct.map_or(3000, |t| t.1 as i32),
                kelvin_max: ct.map_or(10000, |t| t.2 as i32),
                kelvin_step: ct.map_or(100, |t| t.3 as i32),
                languages: keep(
                    prev.map(|p| p.languages.clone()),
                    langs.iter().map(|v| d::language_name(*v)).collect(),
                ),
                language_index: cur(vcp::OSD_LANGUAGE)
                    .and_then(|c| langs.iter().position(|&v| v == c))
                    .map_or(-1, |i| i as i32),
                has_osd: ddc.is_some_and(|c| c.supports(vcp::OSD)),
                reset_all: ddc.is_some_and(|c| c.supports(vcp::RESET_ALL)),
                reset_color: ddc.is_some_and(|c| c.supports(vcp::RESET_COLOR)),
                reset_picture: ddc.is_some_and(|c| c.supports(vcp::RESET_BRIGHTNESS_CONTRAST)),
                signal: x
                    .hdr_info
                    .as_ref()
                    .map_or(String::new(), |h| {
                        format!("{}-bit {}", h.bits_per_channel, h.encoding)
                    })
                    .into(),
                sdr_nits: if x.hdr_on {
                    x.hdr_info
                        .as_ref()
                        .and_then(|h| h.sdr_nits)
                        .map_or(-1, |n| n as i32)
                } else {
                    -1
                },
                profiles: keep(prev.map(|p| p.profiles.clone()), profile_names.clone()),
                profile_index,
                input_index: cur(vcp::INPUT)
                    .and_then(|c| inputs.iter().position(|&v| v == c))
                    .map_or(-1, |i| i as i32),
            }
        })
        .collect();
    if let Some(m) = sync_model(st.get_displays(), infos) {
        st.set_displays(m);
    }
}

fn wire_windows(ui: &SettingsWindow, tx: &Sender<Cmd>) {
    let win = ui.global::<WinState>();
    let t = tx.clone();
    win.on_set(move |key, value| {
        let (key, value) = (key.to_string(), value.to_string());
        edit(&t, move |c| {
            if let Err(e) = c.windows.set_field(&key, &value) {
                log(&format!("{key} = {value}: {e:#}"));
            }
        });
    });
    win.on_reset_curve(|| {
        if let Err(e) = hidconfig_win::mouse::reset_curve() {
            log(&format!("reset curve: {e:#}"));
        }
        with_app(|app| app.refresh());
    });
    let t = tx.clone();
    win.on_restore(move || {
        let _ = t.send(Cmd::WinRestore);
    });
}

fn update_windows(ui: &SettingsWindow, w: Option<&hidconfig_win::WindowsSettings>) {
    let st = ui.global::<WinState>();
    let Some(w) = w else {
        st.set_available(false);
        return;
    };
    st.set_available(true);
    let (m, k) = (&w.mouse, &w.keyboard);
    let int = |v: Option<u32>| v.unwrap_or(0) as i32;
    let flag = |v: Option<bool>| v.unwrap_or(false);
    st.set_pointer_speed(m.pointer_speed.unwrap_or(10).clamp(1, 20) as i32);
    st.set_enhance_precision(flag(m.enhance_precision));
    st.set_scroll_lines(int(m.scroll_lines));
    st.set_scroll_chars(int(m.scroll_chars));
    st.set_scroll_inactive_windows(flag(m.scroll_inactive_windows));
    st.set_double_click_ms(int(m.double_click_ms));
    st.set_swap_buttons(flag(m.swap_buttons));
    st.set_double_click_area(int(m.double_click_area));
    st.set_drag_threshold(int(m.drag_threshold));
    st.set_hover_ms(int(m.hover_ms));
    st.set_focus_follows_mouse(flag(m.focus_follows_mouse));
    st.set_focus_raise(flag(m.focus_raise));
    st.set_focus_delay_ms(int(m.focus_delay_ms));
    st.set_hide_while_typing(flag(m.hide_while_typing));
    st.set_locate_with_ctrl(flag(m.locate_with_ctrl));
    st.set_trails(int(m.trails));
    st.set_click_lock(flag(m.click_lock));
    st.set_click_lock_ms(int(m.click_lock_ms));
    st.set_snap_to_default_button(flag(m.snap_to_default_button));
    st.set_mouse_keys(flag(m.mouse_keys));
    st.set_curve_default(hidconfig_win::mouse::curve_is_default());

    st.set_repeat_delay(k.repeat_delay.unwrap_or(1) as i32);
    st.set_repeat_rate(k.repeat_rate.unwrap_or(31) as i32);
    st.set_caret_blink_ms(int(k.caret_blink_ms));
    st.set_custom_repeat(flag(k.custom_repeat));
    st.set_custom_repeat_delay_ms(int(k.custom_repeat_delay_ms));
    st.set_custom_repeat_interval_ms(int(k.custom_repeat_interval_ms));
    st.set_sticky_keys_shortcut(flag(k.sticky_keys_shortcut));
    st.set_filter_keys_shortcut(flag(k.filter_keys_shortcut));
    st.set_toggle_keys_shortcut(flag(k.toggle_keys_shortcut));
    st.set_lock_key_beep(flag(k.lock_key_beep));
    use hidconfig_win::keyboard::LanguageHotkey as L;
    st.set_language_hotkey_index(match k.language_hotkey {
        Some(L::AltShift) => 0,
        Some(L::CtrlShift) => 1,
        Some(L::Grave) => 2,
        Some(L::None) => 3,
        None => -1,
    });
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
            c.viper.logo = Some(hidconfig_settings::Logo {
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
    for p in &s.status.pads {
        if let Some(b) = p.battery {
            parts.push(format!("Pad {b}%"));
        }
    }
    if parts.is_empty() {
        "HIDConfig: no devices".into()
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
        .is_some_and(|b| [b.left, b.right].into_iter().flatten().any(|p| p <= 20))
        || s.status
            .pads
            .iter()
            .any(|p| p.battery.is_some_and(|b| b <= 20) && !p.charging);
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
    update_windows(ui, s.status.windows.as_ref());
    update_audio(ui, s);
    update_pads(ui, s);
    update_display(ui, s);
    update_graphics(ui, s);
}
