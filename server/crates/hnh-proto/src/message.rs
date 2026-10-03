//! Byte-stream message buffer mirroring `src/haven/Message.java`.
//!
//! All integers on the wire are LITTLE-ENDIAN; strings are UTF-8
//! NUL-terminated; coords are two int32. This is the single encode/decode
//! point for the entire session protocol so endianness bugs cannot spread
//! across call sites (see docs/mechanics/network/network-protocol.md).

use std::result;

#[derive(Debug, thiserror::Error)]
pub enum MsgError {
    #[error("unexpected end of message at offset {offset} of {len}")]
    Eom { offset: usize, len: usize },
    #[error("message buffer overflow: capacity {cap}, needed {needed}")]
    Overflow { cap: usize, needed: usize },
    #[error("invalid UTF-8 string payload")]
    InvalidUtf8,
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
}

pub type Result<T> = result::Result<T, MsgError>;

/// Upper bound for a single assembled message. The legacy client keeps a
/// 64 KiB receive buffer per datagram; everything we assemble or fragment
/// stays far below this.
pub const MAX_MESSAGE: usize = 1 << 20;

const LIST_END: u8 = 0;
const LIST_INT: u8 = 1;
const LIST_STR: u8 = 2;
const LIST_COORD: u8 = 3;
const LIST_COLOR: u8 = 6;

/// Growing write buffer with Haven primitive encodings.
#[derive(Debug, Default, Clone)]
pub struct MessageBuf {
    buf: Vec<u8>,
    read: usize,
}

impl MessageBuf {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
            read: 0,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            read: 0,
        }
    }

    pub fn from_slice(data: &[u8]) -> Self {
        Self {
            buf: data.to_vec(),
            read: 0,
        }
    }

    // ----- writers -----

    pub fn uint8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn int8(&mut self, v: i8) -> &mut Self {
        self.buf.push(v as u8);
        self
    }

    pub fn uint16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn int32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn uint32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn coord(&mut self, x: i32, y: i32) -> &mut Self {
        self.int32(x).int32(y)
    }

    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    /// UTF-8 string with NUL terminator (`Message.addstring`).
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        self
    }

    /// Raw UTF-8 without terminator (`Message.addstring2`).
    pub fn string_raw(&mut self, s: &str) -> &mut Self {
        self.buf.extend_from_slice(s.as_bytes());
        self
    }

    pub fn color(&mut self, r: u8, g: u8, b: u8, a: u8) -> &mut Self {
        self.uint8(r).uint8(g).uint8(b).uint8(a)
    }

    /// Typed list element: int.
    pub fn lint(&mut self, v: i32) -> &mut Self {
        self.uint8(LIST_INT).int32(v)
    }

    /// Typed list element: string.
    pub fn lstr(&mut self, v: &str) -> &mut Self {
        self.uint8(LIST_STR).string(v)
    }

    /// Typed list element: coord.
    pub fn lcoord(&mut self, x: i32, y: i32) -> &mut Self {
        self.uint8(LIST_COORD).coord(x, y)
    }

    /// Typed list element: color.
    pub fn lcolor(&mut self, r: u8, g: u8, b: u8, a: u8) -> &mut Self {
        self.uint8(LIST_COLOR).color(r, g, b, a)
    }

    /// Typed list terminator.
    pub fn lend(&mut self) -> &mut Self {
        self.uint8(LIST_END)
    }

    // ----- readers -----

    fn need(&self, n: usize) -> Result<()> {
        if self.read + n > self.buf.len() {
            Err(MsgError::Eom {
                offset: self.read,
                len: self.buf.len(),
            })
        } else {
            Ok(())
        }
    }

    pub fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.buf[self.read];
        self.read += 1;
        Ok(v)
    }

    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    pub fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.buf[self.read], self.buf[self.read + 1]]);
        self.read += 2;
        Ok(v)
    }

    pub fn i32(&mut self) -> Result<i32> {
        self.need(4)?;
        let b = &self.buf[self.read..self.read + 4];
        self.read += 4;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(self.i32()? as u32)
    }

    pub fn coord2(&mut self) -> Result<(i32, i32)> {
        let x = self.i32()?;
        let y = self.i32()?;
        Ok((x, y))
    }

    pub fn str(&mut self) -> Result<String> {
        let start = self.read;
        while self.read < self.buf.len() && self.buf[self.read] != 0 {
            self.read += 1;
        }
        if self.read >= self.buf.len() {
            self.read = start;
            return Err(MsgError::Eom {
                offset: start,
                len: self.buf.len(),
            });
        }
        let s = std::str::from_utf8(&self.buf[start..self.read])
            .map_err(|_| MsgError::InvalidUtf8)?
            .to_owned();
        self.read += 1; // skip terminator
        Ok(s)
    }

    pub fn blob(&mut self, n: usize) -> Result<&[u8]> {
        self.need(n)?;
        let s = &self.buf[self.read..self.read + n];
        self.read += n;
        Ok(s)
    }

    /// Remaining unread bytes (zero-copy view).
    pub fn rest(&self) -> &[u8] {
        &self.buf[self.read..]
    }

    /// Skip typed-list argument, returning it as owned values.
    pub fn list_arg(&mut self) -> Result<Option<ListArg>> {
        match self.u8()? {
            LIST_END => Ok(None),
            LIST_INT => Ok(Some(ListArg::Int(self.i32()?))),
            LIST_STR => Ok(Some(ListArg::Str(self.str()?))),
            LIST_COORD => {
                let (x, y) = self.coord2()?;
                Ok(Some(ListArg::Coord(x, y)))
            }
            LIST_COLOR => {
                let r = self.u8()?;
                let g = self.u8()?;
                let b = self.u8()?;
                let a = self.u8()?;
                Ok(Some(ListArg::Color(r, g, b, a)))
            }
            other => Err(MsgError::Eom {
                offset: self.read - 1,
                len: self.buf.len(),
            })
            .map_err(|_| MsgError::TooLarge(other as usize)),
        }
    }

    /// Read a full typed list until T_END.
    pub fn list(&mut self) -> Result<Vec<ListArg>> {
        let mut out = Vec::new();
        while let Some(arg) = self.list_arg()? {
            out.push(arg);
        }
        Ok(out)
    }

    pub fn eom(&self) -> bool {
        self.read >= self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListArg {
    Int(i32),
    Str(String),
    Coord(i32, i32),
    Color(u8, u8, u8, u8),
}

impl ListArg {
    pub fn as_int(&self) -> Option<i32> {
        match self {
            ListArg::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ListArg::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_coord(&self) -> Option<(i32, i32)> {
        match self {
            ListArg::Coord(x, y) => Some((*x, *y)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_primitives() {
        let mut m = MessageBuf::new();
        m.uint8(0xAB)
            .uint16(0xBEEF)
            .int32(-123456)
            .coord(-100, 250)
            .string("Haven");
        let mut r = MessageBuf::from_slice(m.as_slice());
        assert_eq!(r.u8().unwrap(), 0xAB);
        assert_eq!(r.u16().unwrap(), 0xBEEF);
        assert_eq!(r.i32().unwrap(), -123456);
        assert_eq!(r.coord2().unwrap(), (-100, 250));
        assert_eq!(r.str().unwrap(), "Haven");
        assert!(r.eom());
    }

    #[test]
    fn roundtrip_typed_list() {
        let mut m = MessageBuf::new();
        m.lint(-7)
            .lstr("play")
            .lcoord(1, 2)
            .lcolor(255, 0, 128, 32)
            .lend();
        let mut r = MessageBuf::from_slice(m.as_slice());
        let l = r.list().unwrap();
        assert_eq!(l[0], ListArg::Int(-7));
        assert_eq!(l[1], ListArg::Str("play".into()));
        assert_eq!(l[2], ListArg::Coord(1, 2));
        assert_eq!(l[3], ListArg::Color(255, 0, 128, 32));
    }

    #[test]
    fn eom_error() {
        let mut r = MessageBuf::from_slice(&[1, 2]);
        assert!(r.i32().is_err());
    }
}
