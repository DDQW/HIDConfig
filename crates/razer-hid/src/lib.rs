//! Windows transport: talks the Razer 90-byte protocol over HID feature reports.

use std::{thread, time::Duration};

use anyhow::{Context, Result, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use razer_proto::{REPORT_LEN, Report, Status, extended::Effect, led, mouse, standard};

pub use hidapi;

pub const RAZER_VID: u16 = 0x1532;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Dpi,
    PollingRate,
    LogoEffect,
    LogoBrightness,
}

/// Static per-model knowledge (OpenRazer's `razermouse_driver.c` switch tables).
#[derive(Debug)]
pub struct Model {
    pub pid: u16,
    pub name: &'static str,
    /// USB interface carrying the control feature report.
    pub control_interface: i32,
    pub transaction_id: u8,
    pub max_dpi: u16,
    pub features: &'static [Feature],
}

pub static MODELS: &[Model] = &[Model {
    pid: 0x0078,
    name: "Razer Viper",
    control_interface: 0,
    transaction_id: 0x1f,
    max_dpi: 16000,
    features: &[Feature::Dpi, Feature::PollingRate, Feature::LogoEffect, Feature::LogoBrightness],
}];

pub fn model(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.pid == pid)
}

pub struct Device {
    pub model: &'static Model,
    pub path: String,
    hid: HidDevice,
}

/// All supported, connected devices.
pub fn open_all(api: &HidApi) -> Vec<Result<Device>> {
    api.device_list()
        .filter(|d| d.vendor_id() == RAZER_VID)
        .filter_map(|d| {
            let model = model(d.product_id())?;
            (d.interface_number() == model.control_interface).then(|| Device::open(api, d, model))
        })
        .collect()
}

impl Device {
    fn open(api: &HidApi, info: &DeviceInfo, model: &'static Model) -> Result<Self> {
        let hid = info.open_device(api).with_context(|| format!("opening {}", model.name))?;
        Ok(Device { model, path: info.path().to_string_lossy().into_owned(), hid })
    }

    pub fn supports(&self, f: Feature) -> bool {
        self.model.features.contains(&f)
    }

    fn txn(&self) -> u8 {
        self.model.transaction_id
    }

    /// Sends one request and reads the response, retrying while the device is busy.
    ///
    /// The device has a single response slot shared by every process, so another
    /// client (e.g. Synapse's Chroma service) can overwrite it between our set and
    /// get. A reply that doesn't match our request is treated as such a collision
    /// and the request is resent.
    pub fn transact(&self, req: &Report) -> Result<Report> {
        let mut buf = [0u8; REPORT_LEN + 1]; // byte 0 = HID report id
        buf[1..].copy_from_slice(&req.to_bytes());
        let mut last_foreign = None;
        'send: for _ in 0..5 {
            self.hid.send_feature_report(&buf).context("send_feature_report")?;
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(5));
                let mut resp = [0u8; REPORT_LEN + 1];
                let n = self.hid.get_feature_report(&mut resp).context("get_feature_report")?;
                if n < REPORT_LEN {
                    bail!("short feature report ({n} bytes)");
                }
                let body: &[u8; REPORT_LEN] = resp[n - REPORT_LEN..n].try_into().unwrap();
                let r = Report::from_bytes(body);
                match r.status() {
                    Status::Busy => continue,
                    _ if !r.answers(req) => {
                        last_foreign = Some(r);
                        continue 'send;
                    }
                    Status::Success => return Ok(r),
                    s => bail!("device returned {s:?} for {req:?}"),
                }
            }
            bail!("device stayed busy");
        }
        bail!("response slot kept being taken by another client (last: {last_foreign:?}); is Synapse running?")
    }

    pub fn firmware(&self) -> Result<(u8, u8)> {
        let r = self.transact(&standard::get_firmware_version(self.txn()))?;
        Ok((r.args[0], r.args[1]))
    }

    pub fn serial(&self) -> Result<String> {
        let r = self.transact(&standard::get_serial(self.txn()))?;
        Ok(r.args[..22].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect())
    }

    pub fn dpi(&self) -> Result<(u16, u16)> {
        Ok(mouse::parse_dpi(&self.transact(&mouse::get_dpi(self.txn()))?))
    }

    pub fn set_dpi(&self, x: u16, y: u16) -> Result<()> {
        let max = self.model.max_dpi;
        if !(100..=max).contains(&x) || !(100..=max).contains(&y) {
            bail!("DPI must be 100..={max}");
        }
        self.transact(&mouse::set_dpi(self.txn(), x, y))?;
        Ok(())
    }

    pub fn polling_rate(&self) -> Result<u16> {
        let r = self.transact(&mouse::get_polling_rate(self.txn()))?;
        mouse::parse_polling_rate(&r).context("unknown polling rate code")
    }

    pub fn set_polling_rate(&self, hz: u16) -> Result<()> {
        let req = mouse::set_polling_rate(self.txn(), hz).context("rate must be 125, 500 or 1000")?;
        self.transact(&req)?;
        Ok(())
    }

    pub fn set_logo_effect(&self, effect: Effect) -> Result<()> {
        self.transact(&razer_proto::extended::set_effect(self.txn(), led::LOGO, effect))?;
        Ok(())
    }

    pub fn logo_brightness(&self) -> Result<u8> {
        let r = self.transact(&razer_proto::extended::get_brightness(self.txn(), led::LOGO))?;
        Ok(razer_proto::extended::parse_brightness(&r))
    }

    pub fn set_logo_brightness(&self, b: u8) -> Result<()> {
        self.transact(&razer_proto::extended::set_brightness(self.txn(), led::LOGO, b))?;
        Ok(())
    }
}
