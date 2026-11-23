//! Minimal hand-written protobuf encoding for the few Takion messages we exchange
//! (see chiaki-ng `lib/protobuf/takion.proto`).

pub mod msg_type {
    pub const BIG: u64 = 0;
    pub const BANG: u64 = 1;
    pub const HEARTBEAT: u64 = 3;
    pub const DISCONNECT: u64 = 8;
    pub const STREAMINFO: u64 = 13;
    pub const STREAMINFOACK: u64 = 14;
    pub const CONTROLLERCONNECTION: u64 = 21;
}

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    fn varint(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(b);
                break;
            }
            self.buf.push(b | 0x80);
        }
    }

    fn key(&mut self, field: u32, wire: u8) {
        self.varint(((field as u64) << 3) | wire as u64);
    }

    pub fn uint(&mut self, field: u32, v: u64) -> &mut Self {
        self.key(field, 0);
        self.varint(v);
        self
    }

    pub fn bytes(&mut self, field: u32, v: &[u8]) -> &mut Self {
        self.key(field, 2);
        self.varint(v.len() as u64);
        self.buf.extend_from_slice(v);
        self
    }

    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

pub enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64,
    Fixed32,
}

/// Iterates over the top level fields of a message. Stops at the first malformed field.
pub fn fields(mut buf: &[u8]) -> Vec<(u32, Value<'_>)> {
    fn varint(buf: &mut &[u8]) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = buf.split_first()?;
            *buf = rest;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    let mut out = Vec::new();
    while !buf.is_empty() {
        let Some(key) = varint(&mut buf) else { break };
        let field = (key >> 3) as u32;
        let value = match key & 7 {
            0 => match varint(&mut buf) {
                Some(v) => Value::Varint(v),
                None => break,
            },
            1 => {
                if buf.len() < 8 {
                    break;
                }
                buf = &buf[8..];
                Value::Fixed64
            }
            2 => {
                let Some(len) = varint(&mut buf) else { break };
                let len = len as usize;
                if buf.len() < len {
                    break;
                }
                let (v, rest) = buf.split_at(len);
                buf = rest;
                Value::Bytes(v)
            }
            5 => {
                if buf.len() < 4 {
                    break;
                }
                buf = &buf[4..];
                Value::Fixed32
            }
            _ => break,
        };
        out.push((field, value));
    }
    out
}

pub fn get_varint(fields: &[(u32, Value<'_>)], field: u32) -> Option<u64> {
    fields.iter().find_map(|(f, v)| match v {
        Value::Varint(x) if *f == field => Some(*x),
        _ => None,
    })
}

pub fn get_bytes<'a>(fields: &[(u32, Value<'a>)], field: u32) -> Option<&'a [u8]> {
    fields.iter().find_map(|(f, v)| match v {
        Value::Bytes(x) if *f == field => Some(*x),
        _ => None,
    })
}

/// A message consisting of only its type.
pub fn simple(msg_type: u64) -> Vec<u8> {
    Writer::new().uint(1, msg_type).finish()
}
