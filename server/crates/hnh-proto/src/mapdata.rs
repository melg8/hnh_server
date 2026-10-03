//! MSG_MAPDATA assembly and other world-side wire builders.

use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;

use crate::message::MessageBuf;

/// Assembled (unfragmented) MAPDATA payload for one grid:
/// coord, minimap name, plot flavor table, zlib(10000 tile bytes + plots).
pub struct MapGridPayload {
    pub gc: (i32, i32),
    /// Stable per-grid identity string ("" = none).
    pub mnm: String,
    /// 100*100 tile ids, row-major over y then x (tiles[x][y] client-side).
    pub tiles: Vec<u8>,
    /// (pidx, pfl) pairs; pidx 255 terminates.
    pub plot_flags: Vec<(u8, u8)>,
    /// (pidx, type, c1x, c1y, c2x, c2y) records.
    pub plots: Vec<(u8, u8, u8, u8, u8, u8)>,
}

impl MapGridPayload {
    pub fn encode(&self) -> Vec<u8> {
        let mut m = MessageBuf::with_capacity(16 * 1024);
        m.coord(self.gc.0, self.gc.1);
        m.string(&self.mnm);
        for &(pidx, fl) in &self.plot_flags {
            m.uint8(pidx).uint8(fl);
        }
        m.uint8(255);
        // zlib-compressed: tiles then plot list.
        let mut z = ZlibEncoder::new(Vec::with_capacity(10_100), Compression::default());
        z.write_all(&self.tiles).expect("BUG: write to Vec cannot fail");
        for &(pidx, t, c1x, c1y, c2x, c2y) in &self.plots {
            z.write_all(&[pidx, t, c1x, c1y, c2x, c2y]).expect("BUG: write to Vec cannot fail");
        }
        z.write_all(&[255]).expect("BUG: write to Vec cannot fail");
        let compressed = z.finish().expect("BUG: finish cannot fail");
        m.bytes(&compressed);
        m.finish()
    }
}

/// Fragment an assembled payload into MSG_MAPDATA datagrams (MTU-bounded).
/// Each datagram: int32 pktid, uint16 off, uint16 total, chunk bytes.
pub fn fragment_payload(
    msg_type: u8,
    pktid: i32,
    payload: &[u8],
    mtu: usize,
) -> Vec<Vec<u8>> {
    let total = payload.len() as u16;
    let chunk_size = mtu.saturating_sub(9).max(256);
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < payload.len() || out.is_empty() {
        let end = (off + chunk_size).min(payload.len());
        let mut m = MessageBuf::with_capacity(end - off + 9);
        m.uint8(msg_type).int32(pktid).uint16(off as u16).uint16(total);
        if end > off {
            m.bytes(&payload[off..end]);
        }
        out.push(m.finish());
        off = end;
        if off >= payload.len() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::MSG_MAPDATA;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    #[test]
    fn mapdata_roundtrip_shape() {
        let p = MapGridPayload {
            gc: (-3, 7),
            mnm: "grid-test-1".into(),
            tiles: vec![13u8; 10_000],
            plot_flags: vec![(0, 0)],
            plots: vec![(0, 0, 1, 1, 4, 4)],
        };
        let bytes = p.encode();
        // Parse header back.
        let mut m = MessageBuf::from_slice(&bytes);
        assert_eq!(m.coord2().unwrap(), (-3, 7));
        assert_eq!(m.str().unwrap(), "grid-test-1");
        let mut flags = Vec::new();
        loop {
            let v = m.u8().unwrap();
            if v == 255 {
                break;
            }
            let fl = m.u8().unwrap();
            flags.push((v, fl));
        }
        assert_eq!(flags, vec![(0, 0)]);
        // Inflate the rest.
        let mut d = ZlibDecoder::new(m.rest());
        let mut out = Vec::new();
        d.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 10_000 + 6 + 1);
        assert_eq!(out[0], 13);
        assert_eq!(&out[10_000..10_006], &[0, 0, 1, 1, 4, 4]);
        assert_eq!(out[10_006], 255);
    }

    #[test]
    fn fragmentation_reassembles() {
        let payload: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let frags = fragment_payload(MSG_MAPDATA, 42, &payload, 1200);
        assert!(frags.len() > 1);
        let mut reassembled = Vec::new();
        for f in &frags {
            let mut m = MessageBuf::from_slice(&f[1..]);
            assert_eq!(m.i32().unwrap(), 42);
            let off = m.u16().unwrap() as usize;
            let total = m.u16().unwrap() as usize;
            assert_eq!(total, 5000);
            let chunk = m.rest().to_vec();
            assert_eq!(off, reassembled.len());
            reassembled.extend_from_slice(&chunk);
        }
        assert_eq!(reassembled, payload);
    }
}
