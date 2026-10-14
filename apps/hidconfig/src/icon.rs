//! Tray icon drawn at runtime: a ring whose colour shows overall state.

use tray_icon::Icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// At least one device connected, nothing to worry about.
    Ok,
    /// An earbud is at or below 20 %.
    LowBattery,
    /// No supported device connected.
    Idle,
}

const SIZE: u32 = 32;

pub fn icon(state: State) -> Icon {
    let (r, g, b) = match state {
        State::Ok => (0x44, 0xd6, 0x2c),
        State::LowBattery => (0xff, 0x9a, 0x1f),
        State::Idle => (0x8a, 0x8a, 0x8a),
    };
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let c = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // Ring from radius 9.5 to 15.5 plus a centre dot of radius 4, anti-aliased.
            let ring = coverage(d, 9.5, 15.5);
            let dot = coverage(d, -1.0, 4.0);
            let a = ring.max(dot);
            rgba.extend_from_slice(&[r, g, b, (a * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon buffer")
}

/// 1 inside `[inner, outer]`, fading out over one pixel at each edge.
fn coverage(d: f32, inner: f32, outer: f32) -> f32 {
    let a = (d - inner + 0.5).clamp(0.0, 1.0);
    let b = (outer - d + 0.5).clamp(0.0, 1.0);
    a.min(b)
}
