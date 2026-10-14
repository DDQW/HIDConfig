use std::{thread, time::Duration};

use anyhow::{Context, Result, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use razer_proto::{REPORT_LEN, Report, Status, standard};

const RAZER_VID: u16 = 0x1532;

fn main() -> Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "list".into());
    let api = HidApi::new()?;
    match cmd.as_str() {
        "list" => list(&api),
        "probe" => probe(&api),
        other => bail!("unknown command {other:?} (expected list | probe)"),
    }
}

fn razer_devices(api: &HidApi) -> Vec<&DeviceInfo> {
    api.device_list().filter(|d| d.vendor_id() == RAZER_VID).collect()
}

fn list(api: &HidApi) -> Result<()> {
    for d in razer_devices(api) {
        println!(
            "pid={:04x} if={} usage_page={:04x} usage={:04x} product={:?}\n    {}",
            d.product_id(),
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.product_string().unwrap_or(""),
            d.path().to_string_lossy()
        );
    }
    Ok(())
}

/// Sends one request and reads the response, retrying while the device is busy.
fn transact(dev: &HidDevice, req: &Report) -> Result<Report> {
    let mut buf = [0u8; REPORT_LEN + 1]; // byte 0 = HID report id
    buf[1..].copy_from_slice(&req.to_bytes());
    dev.send_feature_report(&buf).context("send_feature_report")?;
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(5));
        let mut resp = [0u8; REPORT_LEN + 1];
        let n = dev.get_feature_report(&mut resp).context("get_feature_report")?;
        if n < REPORT_LEN {
            bail!("short feature report ({n} bytes)");
        }
        let body: &[u8; REPORT_LEN] = resp[n - REPORT_LEN..n].try_into().unwrap();
        let r = Report::from_bytes(body);
        match r.status() {
            Status::Busy => continue,
            _ if !r.answers(req) => bail!("unexpected response {r:?}"),
            Status::Success => return Ok(r),
            s => bail!("device returned {s:?}"),
        }
    }
    bail!("device stayed busy")
}

/// Read-only: tries firmware + serial on every Razer HID collection.
fn probe(api: &HidApi) -> Result<()> {
    for d in razer_devices(api) {
        let label = format!(
            "pid={:04x} if={} page={:04x} usage={:04x}",
            d.product_id(),
            d.interface_number(),
            d.usage_page(),
            d.usage()
        );
        let dev = match d.open_device(api) {
            Ok(dev) => dev,
            Err(e) => {
                println!("{label}: open failed: {e}");
                continue;
            }
        };
        for txn in [0x1f, 0x3f, 0xff] {
            match transact(&dev, &standard::get_firmware_version(txn)) {
                Ok(fw) => {
                    println!("{label}: txn={txn:#04x} firmware v{}.{}", fw.args[0], fw.args[1]);
                    if let Ok(s) = transact(&dev, &standard::get_serial(txn)) {
                        let serial: String = s.args[..22]
                            .iter()
                            .take_while(|&&c| c != 0)
                            .map(|&c| c as char)
                            .collect();
                        println!("{label}: serial {serial}");
                    }
                    break;
                }
                Err(e) => println!("{label}: txn={txn:#04x} {e:#}"),
            }
        }
    }
    Ok(())
}
