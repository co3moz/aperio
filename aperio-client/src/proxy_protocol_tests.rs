//! What these pin down: that the bytes match the specification exactly, and
//! that anything we cannot state truthfully produces no header at all rather
//! than a header a receiver would act on.

use super::*;

fn addr(s: &str) -> SocketAddr {
  s.parse().unwrap()
}

#[test]
fn encodes_an_ipv4_header_byte_for_byte() {
  let header = header_v2(addr("203.0.113.7:51234"), addr("127.0.0.1:5432")).unwrap();
  assert_eq!(header.len(), 16 + 12);
  assert_eq!(&header[..12], &SIGNATURE);
  assert_eq!(header[12], 0x21, "version 2, command PROXY");
  assert_eq!(header[13], 0x11, "TCP over IPv4");
  assert_eq!(&header[14..16], &12u16.to_be_bytes());
  assert_eq!(&header[16..20], &[203, 0, 113, 7]);
  assert_eq!(&header[20..24], &[127, 0, 0, 1]);
  assert_eq!(&header[24..26], &51234u16.to_be_bytes());
  assert_eq!(&header[26..28], &5432u16.to_be_bytes());
}

#[test]
fn encodes_an_ipv6_header_byte_for_byte() {
  let header = header_v2(addr("[2001:db8::1]:443"), addr("[::1]:5432")).unwrap();
  assert_eq!(header.len(), 16 + 36);
  assert_eq!(header[13], 0x21, "TCP over IPv6");
  assert_eq!(&header[14..16], &36u16.to_be_bytes());
  assert_eq!(&header[48..50], &443u16.to_be_bytes());
  assert_eq!(&header[50..52], &5432u16.to_be_bytes());
}

#[test]
fn refuses_to_mix_address_families() {
  // Real case, not a hypothetical: a visitor on IPv6 reaching a backend
  // listening on IPv4. The wire format cannot say that, and a malformed
  // header is a protocol error the receiver closes the connection over, so
  // the connection is better off with no header.
  assert!(header_v2(addr("[2001:db8::1]:443"), addr("127.0.0.1:5432")).is_none());
  assert!(header_v2(addr("203.0.113.7:80"), addr("[::1]:5432")).is_none());
}

#[test]
fn no_visitor_address_means_no_header() {
  // The backend expects a header only because an operator said this tunnel
  // carries them. A header full of zeroes would be a lie it acts on.
  assert!(header_for(None, addr("127.0.0.1:5432")).is_none());
  assert!(header_for(Some(""), addr("127.0.0.1:5432")).is_none());
  assert!(header_for(Some("not-an-address"), addr("127.0.0.1:5432")).is_none());
  // An address with no port cannot fill the source port field.
  assert!(header_for(Some("203.0.113.7"), addr("127.0.0.1:5432")).is_none());
}

#[test]
fn a_usable_visitor_address_produces_the_same_bytes_as_the_encoder() {
  let local = addr("127.0.0.1:5432");
  assert_eq!(
    header_for(Some("203.0.113.7:51234"), local),
    header_v2(addr("203.0.113.7:51234"), local)
  );
}
