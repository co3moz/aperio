#![no_main]
//! Fuzzes the two JSON/zlib decode paths a peer feeds untrusted bytes into:
//! `decompress_frame` (zlib-inflate a compressed JSON frame, output-bounded)
//! and `TunnelMessage` JSON deserialization. Neither may panic on arbitrary
//! input, and inflation must stay within the output cap.

use libfuzzer_sys::fuzz_target;

#[path = "../../aperio-server/src/protocol.rs"]
mod protocol;

const MAX_OUT: usize = 1 << 20; // 1 MiB inflate cap, as the server uses.

fuzz_target!(|data: &[u8]| {
    // zlib frame decode: must not panic, and any output respects the cap.
    if let Some(inflated) = protocol::decompress_frame(data, MAX_OUT) {
        assert!(inflated.len() <= MAX_OUT, "inflate exceeded the output cap");
        // The inflated text is then parsed as a tunnel message.
        let _ = serde_json::from_str::<protocol::TunnelMessage>(&inflated);
    }
    // Direct JSON decode of the raw bytes (uncompressed frame path).
    let _ = serde_json::from_slice::<protocol::TunnelMessage>(data);
});
