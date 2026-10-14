//! Razer USB control protocol (the 90-byte feature report used by OpenRazer).
//!
//! Layout, per OpenRazer's `razercommon.h`:
//! `status | txn id | remaining packets (BE u16) | protocol type | data size |
//!  command class | command id | args[80] | crc | reserved`

pub const REPORT_LEN: usize = 90;
pub const ARGS_LEN: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    New,
    Busy,
    Success,
    Failure,
    Timeout,
    NotSupported,
    Unknown(u8),
}

impl From<u8> for Status {
    fn from(b: u8) -> Self {
        match b {
            0x00 => Status::New,
            0x01 => Status::Busy,
            0x02 => Status::Success,
            0x03 => Status::Failure,
            0x04 => Status::Timeout,
            0x05 => Status::NotSupported,
            other => Status::Unknown(other),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Report {
    pub status: u8,
    pub transaction_id: u8,
    pub remaining_packets: u16,
    pub protocol_type: u8,
    pub data_size: u8,
    pub command_class: u8,
    pub command_id: u8,
    pub args: [u8; ARGS_LEN],
    pub crc: u8,
    pub reserved: u8,
}

impl std::fmt::Debug for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = (self.data_size as usize).min(ARGS_LEN);
        f.debug_struct("Report")
            .field("status", &Status::from(self.status))
            .field("txn", &format_args!("{:#04x}", self.transaction_id))
            .field("class", &format_args!("{:#04x}", self.command_class))
            .field("id", &format_args!("{:#04x}", self.command_id))
            .field("size", &self.data_size)
            .field("args", &format_args!("{:02x?}", &self.args[..n]))
            .finish()
    }
}

impl Report {
    /// A new request. `data_size` is how many argument bytes the command uses
    /// (for "get" commands, how many the device should fill in).
    pub fn new(transaction_id: u8, command_class: u8, command_id: u8, data_size: u8) -> Self {
        Report {
            status: 0,
            transaction_id,
            remaining_packets: 0,
            protocol_type: 0,
            data_size,
            command_class,
            command_id,
            args: [0; ARGS_LEN],
            crc: 0,
            reserved: 0,
        }
    }

    pub fn with_args(mut self, args: &[u8]) -> Self {
        self.args[..args.len()].copy_from_slice(args);
        self
    }

    pub fn status(&self) -> Status {
        Status::from(self.status)
    }

    pub fn to_bytes(&self) -> [u8; REPORT_LEN] {
        let mut b = [0u8; REPORT_LEN];
        b[0] = self.status;
        b[1] = self.transaction_id;
        b[2..4].copy_from_slice(&self.remaining_packets.to_be_bytes());
        b[4] = self.protocol_type;
        b[5] = self.data_size;
        b[6] = self.command_class;
        b[7] = self.command_id;
        b[8..88].copy_from_slice(&self.args);
        b[88] = crc(&b);
        b[89] = self.reserved;
        b
    }

    pub fn from_bytes(b: &[u8; REPORT_LEN]) -> Self {
        let mut args = [0u8; ARGS_LEN];
        args.copy_from_slice(&b[8..88]);
        Report {
            status: b[0],
            transaction_id: b[1],
            remaining_packets: u16::from_be_bytes([b[2], b[3]]),
            protocol_type: b[4],
            data_size: b[5],
            command_class: b[6],
            command_id: b[7],
            args,
            crc: b[88],
            reserved: b[89],
        }
    }

    /// True if `self` is the response to `request` (same class/id) and its CRC is valid.
    pub fn answers(&self, request: &Report) -> bool {
        self.command_class == request.command_class
            && self.command_id == request.command_id
            && self.crc == crc(&self.to_raw())
    }

    fn to_raw(&self) -> [u8; REPORT_LEN] {
        let mut b = self.to_bytes();
        b[88] = self.crc;
        b
    }
}

/// XOR of bytes 2..88.
pub fn crc(b: &[u8; REPORT_LEN]) -> u8 {
    b[2..88].iter().fold(0, |acc, x| acc ^ x)
}

/// Standard commands shared by all Chroma devices (`razerchromacommon.c`).
pub mod standard {
    use super::Report;

    pub fn get_firmware_version(txn: u8) -> Report {
        Report::new(txn, 0x00, 0x81, 0x02)
    }

    pub fn get_serial(txn: u8) -> Report {
        Report::new(txn, 0x00, 0x82, 0x16)
    }
}

/// Variable store: write to RAM only, or persist across power cycles.
pub const NOSTORE: u8 = 0x00;
pub const VARSTORE: u8 = 0x01;

/// LED ids (`razercommon.h`).
pub mod led {
    pub const SCROLL_WHEEL: u8 = 0x01;
    pub const LOGO: u8 = 0x04;
    pub const BACKLIGHT: u8 = 0x05;
}

/// Mouse commands (`razerchromacommon.c` misc section).
pub mod mouse {
    use super::{Report, VARSTORE};

    pub fn get_dpi(txn: u8) -> Report {
        Report::new(txn, 0x04, 0x85, 0x07).with_args(&[VARSTORE])
    }

    pub fn set_dpi(txn: u8, x: u16, y: u16) -> Report {
        let [xh, xl] = x.to_be_bytes();
        let [yh, yl] = y.to_be_bytes();
        Report::new(txn, 0x04, 0x05, 0x07).with_args(&[VARSTORE, xh, xl, yh, yl, 0, 0])
    }

    /// `(x, y)` from a `get_dpi` response.
    pub fn parse_dpi(r: &Report) -> (u16, u16) {
        (
            u16::from_be_bytes([r.args[1], r.args[2]]),
            u16::from_be_bytes([r.args[3], r.args[4]]),
        )
    }

    pub fn get_polling_rate(txn: u8) -> Report {
        Report::new(txn, 0x00, 0x85, 0x01)
    }

    /// `hz` must be 125, 500 or 1000.
    pub fn set_polling_rate(txn: u8, hz: u16) -> Option<Report> {
        let code = match hz {
            1000 => 0x01,
            500 => 0x02,
            125 => 0x08,
            _ => return None,
        };
        Some(Report::new(txn, 0x00, 0x05, 0x01).with_args(&[code]))
    }

    pub fn parse_polling_rate(r: &Report) -> Option<u16> {
        match r.args[0] {
            0x01 => Some(1000),
            0x02 => Some(500),
            0x08 => Some(125),
            _ => None,
        }
    }
}

/// Extended matrix effects (class 0x0F), used by the Viper and most newer devices.
pub mod extended {
    use super::{Report, VARSTORE};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Effect {
        Off,
        Static([u8; 3]),
        Breathing([u8; 3]),
        Spectrum,
    }

    pub fn set_effect(txn: u8, led: u8, effect: Effect) -> Report {
        match effect {
            Effect::Off => Report::new(txn, 0x0F, 0x02, 0x06).with_args(&[VARSTORE, led, 0x00]),
            Effect::Static([r, g, b]) => Report::new(txn, 0x0F, 0x02, 0x09)
                .with_args(&[VARSTORE, led, 0x01, 0, 0, 0x01, r, g, b]),
            Effect::Breathing([r, g, b]) => Report::new(txn, 0x0F, 0x02, 0x09)
                .with_args(&[VARSTORE, led, 0x02, 0x01, 0, 0x01, r, g, b]),
            Effect::Spectrum => {
                Report::new(txn, 0x0F, 0x02, 0x06).with_args(&[VARSTORE, led, 0x03])
            }
        }
    }

    pub fn get_brightness(txn: u8, led: u8) -> Report {
        Report::new(txn, 0x0F, 0x84, 0x03).with_args(&[VARSTORE, led])
    }

    pub fn set_brightness(txn: u8, led: u8, brightness: u8) -> Report {
        Report::new(txn, 0x0F, 0x04, 0x03).with_args(&[VARSTORE, led, brightness])
    }

    pub fn parse_brightness(r: &Report) -> u8 {
        r.args[2]
    }
}

/// Message frame used by the Hammerhead V3 HyperSpeed dongle (feature report 0x02,
/// 63 bytes after the report id). See `docs/hammerhead-v3-hyperspeed.md`.
pub mod hyperspeed {
    pub const REPORT_ID: u8 = 0x02;
    /// Report id + 63 bytes.
    pub const FRAME_LEN: usize = 64;
    const HEADER: [u8; 5] = [0x05, 0x60, 0x00, 0x00, 0x00];
    const LEN: usize = 6;
    const CRC: usize = 62;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Message {
        pub cmd: u8,
        pub sub: u8,
        pub data: Vec<u8>,
    }

    pub fn checksum(f: &[u8; FRAME_LEN]) -> u8 {
        f[LEN..CRC].iter().fold(0, |a, b| a ^ b)
    }

    pub fn encode(m: &Message) -> [u8; FRAME_LEN] {
        let mut f = [0u8; FRAME_LEN];
        f[0] = REPORT_ID;
        f[1..6].copy_from_slice(&HEADER);
        f[LEN] = m.data.len() as u8;
        f[7] = m.cmd;
        f[8] = m.sub;
        f[9..9 + m.data.len()].copy_from_slice(&m.data);
        f[CRC] = checksum(&f);
        f
    }

    /// `None` for an empty buffer, a foreign header or a bad checksum.
    pub fn decode(f: &[u8; FRAME_LEN]) -> Option<Message> {
        if f[1..6] != HEADER || f[CRC] != checksum(f) {
            return None;
        }
        let n = (f[LEN] as usize).min(CRC - 9);
        Some(Message { cmd: f[7], sub: f[8], data: f[9..9 + n].to_vec() })
    }

    /// Like OpenRazer, a get request carries the `0x80 | id` sub-command its reply
    /// echoes; the bare id is presumably the set form. The reply observed on the wire
    /// is `07 80 <left%> <right%>`.
    pub fn get_battery() -> Message {
        Message { cmd: 0x07, sub: 0x80, data: vec![] }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn frame(body: &[u8], crc: u8) -> [u8; FRAME_LEN] {
            let mut f = [0u8; FRAME_LEN];
            f[..6].copy_from_slice(&[0x02, 0x05, 0x60, 0, 0, 0]);
            f[6..6 + body.len()].copy_from_slice(body);
            f[CRC] = crc;
            f
        }

        /// Frames captured from the real dongle.
        #[test]
        fn captured_frames_decode() {
            let cases: &[(&[u8], u8)] = &[
                (&[0x02, 0x18, 0x9f, 0x00], 0x85),
                (&[0x02, 0x18, 0x9f, 0x01], 0x84),
                (&[0x02, 0x07, 0x80, 0x63, 0x63], 0x85),
                (&[0x03, 0x18, 0x84, 0x01, 0x02, 0x01], 0x9d),
                (&[0x03, 0x18, 0x9a, 0x01, 0x03, 0x03], 0x80),
            ];
            for (body, crc) in cases {
                let m = decode(&frame(body, *crc)).expect("valid frame");
                assert_eq!(encode(&m), frame(body, *crc));
            }
            let battery = decode(&frame(&[0x02, 0x07, 0x80, 0x63, 0x63], 0x85)).unwrap();
            assert_eq!(battery, Message { cmd: 0x07, sub: 0x80, data: vec![0x63, 0x63] });
            assert_eq!(decode(&[0; FRAME_LEN]), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_request_bytes() {
        let b = standard::get_firmware_version(0x1f).to_bytes();
        assert_eq!(&b[..8], &[0x00, 0x1f, 0x00, 0x00, 0x00, 0x02, 0x00, 0x81]);
        // crc = 0x02 ^ 0x81
        assert_eq!(b[88], 0x83);
    }

    #[test]
    fn set_dpi_bytes() {
        let b = mouse::set_dpi(0x1f, 1600, 800).to_bytes();
        assert_eq!(&b[5..15], &[0x07, 0x04, 0x05, 0x01, 0x06, 0x40, 0x03, 0x20, 0, 0]);
    }

    #[test]
    fn static_logo_bytes() {
        let b = extended::set_effect(0x1f, led::LOGO, extended::Effect::Static([0xff, 0x10, 0]))
            .to_bytes();
        assert_eq!(
            &b[5..17],
            &[0x09, 0x0f, 0x02, 0x01, 0x04, 0x01, 0, 0, 0x01, 0xff, 0x10, 0x00]
        );
    }

    #[test]
    fn roundtrip() {
        let r = Report::new(0x3f, 0x04, 0x05, 0x07).with_args(&[1, 2, 3]);
        let b = r.to_bytes();
        let back = Report::from_bytes(&b);
        assert_eq!(back.to_bytes(), b);
        assert!(back.answers(&r));
    }
}
