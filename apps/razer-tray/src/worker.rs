//! Device thread: owns the [`Watcher`], applies edits, and publishes snapshots.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Instant,
};

use razer_settings::{Config, Event, Status, TICK, Watcher};

use crate::platform::log;

pub enum Cmd {
    /// Edit the config; changed sections are applied immediately.
    Edit(Box<dyn FnOnce(&mut Config) + Send>),
    /// Live mic toggle (not saved).
    MicOn(bool),
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub config: Config,
    pub status: Status,
    pub last_event: String,
}

pub fn spawn(publish: impl Fn(Snapshot) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("devices".into())
        .spawn(move || run(rx, publish))
        .expect("spawning device thread");
    tx
}

fn describe(ev: &Event) -> String {
    match ev {
        Event::Applied(t) => format!("Applied settings to {t:?}"),
        Event::Failed(t, e) => format!("{t:?}: {e}"),
        Event::Retrying(t, e) => format!("{t:?}: {e} (retrying)"),
        Event::EarbudsWaiting => "Earbuds not connected; waiting".into(),
        Event::ConfigError(e) => format!("Config error: {e}"),
        Event::Reapplying(t) => format!("Re-applying ({t:?})"),
    }
}

fn run(rx: Receiver<Cmd>, publish: impl Fn(Snapshot)) {
    let last_event = std::cell::RefCell::new(String::new());
    let mut on_event = |ev: Event| {
        let text = describe(&ev);
        // Waiting repeats every poll; only log changes.
        if *last_event.borrow() != text {
            log(&text);
        }
        *last_event.borrow_mut() = text;
    };
    let mut watcher = match Watcher::new(&mut on_event) {
        Ok(w) => w,
        Err(e) => {
            log(&format!("cannot start device watcher: {e:#}"));
            return;
        }
    };
    watcher.tick(&mut on_event);
    let mut next_tick = Instant::now() + TICK;
    let mut published: Option<Snapshot> = None;

    loop {
        match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Edit(edit)) => {
                if let Err(e) = watcher.update_config(edit, &mut on_event) {
                    on_event(Event::ConfigError(format!("{e:#}")));
                }
            }
            Ok(Cmd::MicOn(on)) => match watcher.set_mic_muted(!on) {
                Ok(()) => on_event(Event::Applied(razer_settings::Target::Earbuds)),
                Err(e) => on_event(Event::Failed(razer_settings::Target::Earbuds, format!("mic: {e:#}"))),
            },
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                watcher.tick(&mut on_event);
                next_tick = Instant::now() + TICK;
            }
        }
        let snap = Snapshot {
            config: watcher.config().clone(),
            status: watcher.status().clone(),
            last_event: last_event.borrow().clone(),
        };
        if published.as_ref() != Some(&snap) {
            publish(snap.clone());
            published = Some(snap);
        }
    }
}
