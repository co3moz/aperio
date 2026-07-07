//! Local proxying of tunneled traffic: HTTP request forwarding and
//! WebSocket stream bridging to the local backend.

pub(crate) mod http;
pub(crate) mod ws;
