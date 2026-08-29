/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `netdb.h`

use crate::dyld::FunctionExports;
use crate::export_c_func;
use crate::libc::sys::socket::{sockaddr, AF_INET, SOCK_DGRAM, SOCK_STREAM};
use crate::mem::{guest_size_of, ConstPtr, MutPtr, Ptr, SafeRead};
use crate::Environment;

const AI_PASSIVE: i32 = 0x1;

pub const IPPROTO_TCP: i32 = 6;
pub const IPPROTO_UDP: i32 = 17;

const EAI_FAIL: i32 = 4;

#[allow(non_camel_case_types)]
pub type socklen_t = u32;

// TODO: struct definition
#[allow(non_camel_case_types)]
struct hostent {}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
#[allow(non_camel_case_types)]
pub struct addrinfo {
    ai_flags: i32,
    ai_family: i32,
    ai_socktype: i32,
    ai_protocol: i32,
    ai_addrlen: socklen_t,
    ai_canonname: MutPtr<u8>,
    ai_addr: MutPtr<sockaddr>,
    ai_next: MutPtr<addrinfo>,
}
unsafe impl SafeRead for addrinfo {}

fn getaddrinfo(
    env: &mut Environment,
    node_name: MutPtr<u8>,
    serv_name: MutPtr<u8>,
    hints: ConstPtr<addrinfo>,
    res: MutPtr<MutPtr<addrinfo>>,
) -> i32 {
    if !env.options.network_access {
        log_dbg!(
            "Network access is disabled, getaddrinfo({:?}, {:?}, {:?}, {:?}) -> EAI_FAIL",
            node_name,
            serv_name,
            hints,
            res
        );
        return EAI_FAIL;
    }

    // Online mode: client-side name resolution (node_name set). GCDAsyncSocket's
    // lookup:host:port: calls getaddrinfo("host", "port", &hints); resolve it for real
    // via the host resolver so hostnames like login.moleworld.net work (use the domain).
    if !node_name.is_null() {
        let host = match env.mem.cstr_at_utf8(node_name) {
            Ok(h) => h.to_string(),
            Err(_) => return EAI_FAIL,
        };
        let port: u16 = if serv_name.is_null() {
            0
        } else {
            env.mem
                .cstr_at_utf8(serv_name)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        };
        let (socktype, protocol) = if hints.is_null() {
            (SOCK_STREAM, IPPROTO_TCP)
        } else {
            let h = env.mem.read(hints);
            let st = if h.ai_socktype == 0 { SOCK_STREAM } else { h.ai_socktype };
            let pr = if h.ai_protocol == 0 { IPPROTO_TCP } else { h.ai_protocol };
            (st, pr)
        };
        use std::net::{SocketAddr, ToSocketAddrs};
        let v4 = match (host.as_str(), port).to_socket_addrs() {
            Ok(it) => it
                .filter_map(|a| match a {
                    SocketAddr::V4(v4) => Some(v4),
                    _ => None,
                })
                .next(),
            Err(e) => {
                log!("getaddrinfo(\"{}\":{}) resolve error: {}", host, port, e);
                return EAI_FAIL;
            }
        };
        let v4 = match v4 {
            Some(v4) => v4,
            None => {
                log!("getaddrinfo(\"{}\":{}) -> no IPv4 address", host, port);
                return EAI_FAIL;
            }
        };
        let addr = sockaddr::from_ipv4_parts(v4.ip().octets(), port);
        let tmp_addr = env.mem.alloc_and_write(addr);
        let addr_info = addrinfo {
            ai_flags: 0,
            ai_family: AF_INET,
            ai_socktype: socktype,
            ai_protocol: protocol,
            ai_addrlen: guest_size_of::<sockaddr>(),
            ai_canonname: Ptr::null(),
            ai_addr: tmp_addr,
            ai_next: Ptr::null(),
        };
        let tmp_addr_info = env.mem.alloc_and_write(addr_info);
        env.mem.write(res, tmp_addr_info);
        log!("getaddrinfo resolved \"{}\":{} -> {}", host, port, v4.ip());
        return 0;
    }

    let hint = env.mem.read(hints);
    let ai_flags = hint.ai_flags;
    assert_eq!(ai_flags, AI_PASSIVE);
    let ai_family = hint.ai_family;
    assert_eq!(ai_family, AF_INET);
    assert!(hint.ai_socktype == SOCK_STREAM || hint.ai_socktype == SOCK_DGRAM);
    assert!(
        hint.ai_protocol == IPPROTO_TCP || hint.ai_protocol == IPPROTO_UDP || hint.ai_protocol == 0
    );
    let ai_addrlen = hint.ai_addrlen;
    assert_eq!(ai_addrlen, 0);
    assert!(hint.ai_canonname.is_null());
    assert!(hint.ai_addr.is_null());
    assert!(hint.ai_next.is_null());

    let mut addr_info = hint;
    let port: u16 = env.mem.cstr_at_utf8(serv_name).unwrap().parse().unwrap();
    log_dbg!("getaddrinfo: port {}", port);
    let addr = sockaddr::from_ipv4_parts([0; 4], port);

    let tmp_addr = env.mem.alloc_and_write(addr);
    addr_info.ai_addr = tmp_addr;
    addr_info.ai_addrlen = guest_size_of::<sockaddr>();

    let tmp_addr_info = env.mem.alloc_and_write(addr_info);
    env.mem.write(res, tmp_addr_info);

    0 // Success
}

fn freeaddrinfo(env: &mut Environment, addrinfo: MutPtr<addrinfo>) {
    let addrinfo_val = env.mem.read(addrinfo);
    assert!(addrinfo_val.ai_next.is_null()); // TODO
    let ai_addrlen = addrinfo_val.ai_addrlen;
    assert_eq!(ai_addrlen, guest_size_of::<sockaddr>());
    env.mem.free(addrinfo_val.ai_addr.cast());
    env.mem.free(addrinfo.cast());
}

fn gethostbyname(env: &mut Environment, name: ConstPtr<u8>) -> MutPtr<hostent> {
    log!(
        "TODO: gethostbyname({:?} \"{}\") => NULL",
        name,
        env.mem.cstr_at_utf8(name).unwrap()
    );
    // TODO: set h_errno
    Ptr::null()
}

fn gethostent(_env: &mut Environment) -> MutPtr<hostent> {
    log!("TODO: gethostent() => NULL");
    // TODO: set h_errno
    Ptr::null()
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(getaddrinfo(_, _, _, _)),
    export_c_func!(freeaddrinfo(_)),
    export_c_func!(gethostbyname(_)),
    export_c_func!(gethostent()),
];
