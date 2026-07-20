#![no_main]
//! Fuzzes `decode_binary_frame` — the protocol v2 binary frame parser
//! (`[tag][id_len][id bytes][payload]`), the primary tunnel corruption/attack
//! surface. The parser must never panic on arbitrary bytes, and the decoded
//! frame id must always satisfy the ID-prefix invariant `id.len() <= 255`
//! (`id_len` is a single byte).

use libfuzzer_sys::fuzz_target;

#[path = "../../aperio-server/src/protocol.rs"]
mod protocol;

fuzz_target!(|data: &[u8]| {
    if let Some((_tag, id, _payload)) = protocol::decode_binary_frame(data) {
        assert!(
            id.len() <= 255,
            "frame id length invariant violated: {}",
            id.len()
        );
    }
});
