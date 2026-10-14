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
}
