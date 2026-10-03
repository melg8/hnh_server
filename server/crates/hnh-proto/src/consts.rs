//! Protocol constants mirroring `src/haven/Session.java` and
//! `src/haven/Message.java` (docs/mechanics/network/network-protocol.md).

// ----- session message types (first byte of every datagram) -----
pub const MSG_SESS: u8 = 0;
pub const MSG_REL: u8 = 1;
pub const MSG_ACK: u8 = 2;
pub const MSG_BEAT: u8 = 3;
pub const MSG_MAPREQ: u8 = 4;
pub const MSG_MAPDATA: u8 = 5;
pub const MSG_OBJDATA: u8 = 6;
pub const MSG_OBJACK: u8 = 7;
pub const MSG_CLOSE: u8 = 8;

// ----- reliable sub-message types -----
pub const RMSG_NEWWDG: u8 = 0;
pub const RMSG_WDGMSG: u8 = 1;
pub const RMSG_DSTWDG: u8 = 2;
pub const RMSG_MAPIV: u8 = 3;
pub const RMSG_GLOBLOB: u8 = 4;
pub const RMSG_PAGINAE: u8 = 5;
pub const RMSG_RESID: u8 = 6;
pub const RMSG_PARTY: u8 = 7;
pub const RMSG_SFX: u8 = 8;
pub const RMSG_CATTR: u8 = 9;
pub const RMSG_MUSIC: u8 = 10;
pub const RMSG_TILES: u8 = 11;
pub const RMSG_BUFF: u8 = 12;

// ----- object delta sub-messages -----
pub const OD_REM: u8 = 0;
pub const OD_MOVE: u8 = 1;
pub const OD_RES: u8 = 2;
pub const OD_LINBEG: u8 = 3;
pub const OD_LINSTEP: u8 = 4;
pub const OD_SPEECH: u8 = 5;
pub const OD_LAYERS: u8 = 6;
pub const OD_DRAWOFF: u8 = 7;
pub const OD_LUMIN: u8 = 8;
pub const OD_AVATAR: u8 = 9;
pub const OD_FOLLOW: u8 = 10;
pub const OD_HOMING: u8 = 11;
pub const OD_OVERLAY: u8 = 12;
pub const OD_HEALTH: u8 = 14;
pub const OD_BUDDY: u8 = 15;
pub const OD_END: u8 = 255;

// ----- session error codes (MSG_SESS reply) -----
pub const SESSERR_AUTH: u8 = 1;
pub const SESSERR_BUSY: u8 = 2;
pub const SESSERR_CONN: u8 = 3;
pub const SESSERR_PVER: u8 = 4;
pub const SESSERR_EXPR: u8 = 5;

/// Client protocol version carried in MSG_SESS.
pub const PVER: u16 = 2;

/// Auth channel commands (AuthClient.java lines 36-39).
pub const AUTH_CMD_USR: u8 = 1;
pub const AUTH_CMD_PASSWD: u8 = 2;
pub const AUTH_CMD_GETTOKEN: u8 = 3;
pub const AUTH_CMD_USETOKEN: u8 = 4;

// ----- GLOBLOB record tags -----
pub const GMSG_TIME: u8 = 0;
pub const GMSG_ASTRO: u8 = 1;
pub const GMSG_LIGHT: u8 = 2;

/// Party record tags (Party.java lines 35-37).
pub const PD_LIST: u8 = 0;
pub const PD_LEADER: u8 = 1;
pub const PD_MEMBER: u8 = 2;
