//! Minimal STUN (RFC 5389) Binding. The relay tells a device the public address its packets
//! come from (that is all it learns); the device offers it as a candidate for a direct,
//! peer-to-peer connection. UDP port 3478 on the relay server.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const PORT: u16 = 3478;
const COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

pub fn binding_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    b[4..8].copy_from_slice(&COOKIE.to_be_bytes());
    b[8..20].copy_from_slice(txid);
    b
}

/// The transaction id of a Binding request, if `b` is one.
pub fn parse_request(b: &[u8]) -> Option<[u8; 12]> {
    if b.len() < 20 || b.len() > 548 || u16::from_be_bytes([b[0], b[1]]) != BINDING_REQUEST {
        return None;
    }
    if u32::from_be_bytes([b[4], b[5], b[6], b[7]]) != COOKIE {
        return None;
    }
    b[8..20].try_into().ok()
}

/// Binding success response carrying `addr` as XOR-MAPPED-ADDRESS.
pub fn binding_response(txid: &[u8; 12], addr: SocketAddr) -> Vec<u8> {
    let port = addr.port() ^ (COOKIE >> 16) as u16;
    let mut value = vec![0u8];
    match addr.ip() {
        IpAddr::V4(ip) => {
            value.push(0x01);
            value.extend_from_slice(&port.to_be_bytes());
            value.extend_from_slice(&(u32::from(ip) ^ COOKIE).to_be_bytes());
        }
        IpAddr::V6(ip) => {
            value.push(0x02);
            value.extend_from_slice(&port.to_be_bytes());
            let mut key = COOKIE.to_be_bytes().to_vec();
            key.extend_from_slice(txid);
            for (o, k) in ip.octets().iter().zip(key) {
                value.push(o ^ k);
            }
        }
    }
    let mut b = Vec::with_capacity(20 + 4 + value.len());
    b.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    b.extend_from_slice(&((4 + value.len()) as u16).to_be_bytes());
    b.extend_from_slice(&COOKIE.to_be_bytes());
    b.extend_from_slice(txid);
    b.extend_from_slice(&XOR_MAPPED_ADDRESS.to_be_bytes());
    b.extend_from_slice(&(value.len() as u16).to_be_bytes());
    b.extend_from_slice(&value);
    b
}

/// The public address in a Binding success response to `txid`.
pub fn parse_response(b: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if b.len() < 20 || u16::from_be_bytes([b[0], b[1]]) != BINDING_SUCCESS || &b[8..20] != txid {
        return None;
    }
    let mut attrs = &b[20..];
    while attrs.len() >= 4 {
        let kind = u16::from_be_bytes([attrs[0], attrs[1]]);
        let len = u16::from_be_bytes([attrs[2], attrs[3]]) as usize;
        let value = attrs.get(4..4 + len)?;
        if kind == XOR_MAPPED_ADDRESS && len >= 8 {
            let port = u16::from_be_bytes([value[2], value[3]]) ^ (COOKIE >> 16) as u16;
            return match value[1] {
                0x01 => {
                    let ip = u32::from_be_bytes([value[4], value[5], value[6], value[7]]) ^ COOKIE;
                    Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port))
                }
                0x02 if len >= 20 => {
                    let mut key = COOKIE.to_be_bytes().to_vec();
                    key.extend_from_slice(txid);
                    let mut o = [0u8; 16];
                    for (i, k) in key.iter().enumerate() {
                        o[i] = value[4 + i] ^ k;
                    }
                    Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), port))
                }
                _ => None,
            };
        }
        let padded = (len + 3) & !3;
        attrs = attrs.get(4 + padded..)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let txid: [u8; 12] = rand::random();
        let req = binding_request(&txid);
        assert_eq!(parse_request(&req), Some(txid));
        for addr in ["83.135.241.223:51234", "[2001:db8::7]:4000"] {
            let addr: SocketAddr = addr.parse().unwrap();
            let resp = binding_response(&txid, addr);
            assert_eq!(parse_response(&resp, &txid), Some(addr));
            assert_eq!(parse_response(&resp, &[0; 12]), None);
        }
    }
}
