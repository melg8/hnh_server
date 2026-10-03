//! `hnh-proto` — byte-exact Haven & Hearth session wire protocol.

pub mod consts;
pub mod mapdata;
pub mod message;
pub mod rel;

pub use consts::*;
pub use mapdata::{fragment_payload, MapGridPayload};
pub use message::{ListArg, MessageBuf, MsgError};
pub use rel::{RelReceiver, RelSender, ACK_THRESH};

/// Auth-channel frame codec (AuthClient.java lines 127-153):
/// uint8 type, uint8 len, len bytes payload (max 255).
pub fn auth_frame(ty: u8, payload: &[u8]) -> Result<Vec<u8>, MsgError> {
    if payload.len() > 255 {
        return Err(MsgError::TooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.push(ty);
    out.push(payload.len() as u8);
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn parse_auth_frame(buf: &[u8]) -> Option<(u8, Vec<u8>)> {
    if buf.len() < 2 {
        return None;
    }
    let len = buf[1] as usize;
    if buf.len() < 2 + len {
        return None;
    }
    Some((buf[0], buf[2..2 + len].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_frame_roundtrip() {
        let f = auth_frame(AUTH_CMD_PASSWD, &[0u8; 32]).unwrap();
        let (ty, pl) = parse_auth_frame(&f).unwrap();
        assert_eq!(ty, AUTH_CMD_PASSWD);
        assert_eq!(pl.len(), 32);
        assert!(auth_frame(1, &[0; 256]).is_err());
    }
}
