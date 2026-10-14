//! Hammerhead V3 HyperSpeed dongle: feature-report-0x02 mailbox on the ff14 collection.

use std::{
    cell::Cell,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use hidapi::{HidApi, HidDevice};
use razer_proto::hyperspeed::{
    self, Anc, EqPreset, FRAME_LEN, Frame, Message, SEQ_RANGE, STATUS_OK, STATUS_PUSH, req,
};

use crate::RAZER_VID;

pub const PID: u16 = 0x0a61;
const USAGE_PAGE: u16 = 0xff14;

pub struct Earbuds {
    hid: HidDevice,
    seq: Cell<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Percent, `None` when that bud isn't connected.
    pub left: Option<u8>,
    pub right: Option<u8>,
}

impl Battery {
    fn from_bytes(l: u8, r: u8) -> Self {
        let pct = |b: u8| (b != 0xff).then_some(b);
        Battery { left: pct(l), right: pct(r) }
    }
}

impl Earbuds {
    pub fn open(api: &HidApi) -> Result<Option<Self>> {
        let Some(info) = api
            .device_list()
            .find(|d| d.vendor_id() == RAZER_VID && d.product_id() == PID && d.usage_page() == USAGE_PAGE)
        else {
            return Ok(None);
        };
        let hid = info.open_device(api).context("opening Hammerhead V3 HyperSpeed")?;
        // Start somewhere other than where Synapse would, in case it is still running.
        let start = *SEQ_RANGE.start() + (std::process::id() % 31) as u8;
        Ok(Some(Earbuds { hid, seq: Cell::new(start) }))
    }

    fn next_seq(&self) -> u8 {
        let s = self.seq.get();
        self.seq.set(if s >= *SEQ_RANGE.end() { *SEQ_RANGE.start() } else { s + 1 });
        s
    }

    fn read_frame(&self) -> Result<Option<Frame>> {
        let mut f = [0u8; FRAME_LEN];
        f[0] = hyperspeed::REPORT_ID;
        self.hid.get_feature_report(&mut f).context("GET_FEATURE 0x02")?;
        Ok(hyperspeed::decode(&f))
    }

    /// Sends a request and returns the data of the reply with the same sequence number.
    /// Older replies and device pushes queued ahead of it are skipped.
    pub fn transact(&self, m: &Message) -> Result<Vec<u8>> {
        let seq = self.next_seq();
        self.hid
            .send_feature_report(&hyperspeed::request(seq, m))
            .context("SET_FEATURE 0x02")?;
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(60));
            let Some(f) = self.read_frame()? else { continue };
            if f.status == STATUS_PUSH || f.seq != seq {
                continue;
            }
            if f.msg.cmd != m.cmd || f.msg.sub != m.sub {
                bail!("reply for seq {seq:#04x} has cmd {:02x} {:02x}", f.msg.cmd, f.msg.sub);
            }
            if f.status != STATUS_OK {
                bail!("device returned status {:#04x} for {m:02x?}", f.status);
            }
            return Ok(f.msg.data);
        }
        bail!("no reply to {:02x} {:02x} (are the earbuds out of the case?)", m.cmd, m.sub)
    }

    /// Reply data after the echoed selector bytes of a get request.
    fn get(&self, m: Message) -> Result<Vec<u8>> {
        let skip = m.data.len();
        let d = self.transact(&m)?;
        if d.len() <= skip {
            bail!("short reply to {:02x} {:02x}: {d:02x?}", m.cmd, m.sub);
        }
        Ok(d[skip..].to_vec())
    }

    fn set(&self, m: Message) -> Result<()> {
        self.transact(&m).map(|_| ())
    }

    pub fn battery(&self) -> Result<Battery> {
        let d = self.get(req::get_battery())?;
        if d.len() < 2 {
            bail!("short battery reply {d:02x?}");
        }
        Ok(Battery::from_bytes(d[0], d[1]))
    }

    /// Raw connection state bytes (`03 03` = both buds connected, by observation).
    pub fn connection(&self) -> Result<Vec<u8>> {
        self.get(req::connection())
    }

    /// `(mode, call_reduction)`; the second byte is `0x0a` normally, `0x01` when
    /// "reduce ANC during Bluetooth calls" is on.
    pub fn anc(&self) -> Result<(Anc, u8)> {
        let d = self.get(req::get_anc())?;
        Ok((Anc::from_code(d[0]), d.get(1).copied().unwrap_or(0x0a)))
    }

    pub fn set_anc(&self, mode: Anc, call_reduction: u8) -> Result<()> {
        self.set(req::set_anc(mode, call_reduction))
    }

    pub fn power_saving(&self) -> Result<bool> {
        Ok(self.get(req::get_power_saving())?[0] != 0)
    }

    pub fn set_power_saving(&self, on: bool) -> Result<()> {
        self.set(req::set_power_saving(on))
    }

    pub fn power_timeout_secs(&self) -> Result<u16> {
        let d = self.transact(&req::get_power_timeout())?;
        if d.len() < 2 {
            bail!("short timeout reply {d:02x?}");
        }
        Ok(u16::from_be_bytes([d[0], d[1]]))
    }

    pub fn set_power_timeout_secs(&self, secs: u16) -> Result<()> {
        self.set(req::set_power_timeout(secs))
    }

    pub fn dnd(&self) -> Result<bool> {
        Ok(self.get(req::get_dnd())?[0] != 0)
    }

    pub fn set_dnd(&self, on: bool) -> Result<()> {
        self.set(req::set_dnd(on))
    }

    pub fn mic_muted(&self) -> Result<bool> {
        Ok(self.get(req::get_mic_mute())?[0] != 0)
    }

    pub fn set_mic_muted(&self, muted: bool) -> Result<()> {
        self.set(req::set_mic_mute(muted))
    }

    pub fn set_auto_pause(&self, on: bool) -> Result<()> {
        self.set(req::set_auto_pause(on))
    }

    pub fn set_gestures(&self, on: bool) -> Result<()> {
        self.set(req::set_gestures(on))
    }

    pub fn set_audio_prompts(&self, on: bool) -> Result<()> {
        self.set(req::set_audio_prompts(on))
    }

    /// Selects a preset and sends its band values, as Synapse does.
    pub fn set_eq(&self, preset: EqPreset) -> Result<()> {
        self.set(req::set_eq_preset(preset))?;
        self.set(req::set_eq_bands(preset.bands()))
    }
}
