//! Simple Voice Chat client support (off unless the player turns it on): the protocol layer
//! (gcm, proto, session: pure logic), the UDP socket (net), and the glue to the game.
pub mod client;
pub mod gcm;
#[cfg(target_os = "macos")]
pub mod mac_mic;
pub mod net;
pub mod proto;
pub mod session;
