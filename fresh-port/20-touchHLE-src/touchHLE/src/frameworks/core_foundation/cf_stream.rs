/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFReadStream` / `CFWriteStream` over a real host TCP socket — MoleWorld ONLINE mode.
//!
//! The game's main protocol connection is the classic RunLoop/CFStream `AsyncSocket`
//! (CocoaAsyncSocket): `-[NetworkManager establishConnection]` does
//! `CFStreamCreatePairWithSocketToHost` + `CFRead/WriteStreamSetClient` +
//! `...ScheduleWithRunLoop` + `...Open`, then drives reads/writes from the stream client
//! callbacks. touchHLE has no CFStream, so offline these are unresolved no-ops and the
//! game never connects.
//!
//! We back a read/write pair with ONE host `std::net::TcpStream` (non-blocking) and fire
//! the client callbacks from the NSRunLoop tick ([drive_streams]) when the socket
//! connects / has bytes / can accept bytes. Everything is gated on
//! `--allow-network-access`; offline (default) nothing here runs.

use super::cf_allocator::CFAllocatorRef;
use super::CFTypeRef;
use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::foundation::ns_string::to_rust_string;
use crate::mem::{ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{id, nil, objc_classes, ClassExports, HostObject};
use crate::Environment;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// CFStreamEventType bit flags.
const EV_OPEN: u32 = 1; // kCFStreamEventOpenCompleted
const EV_HASBYTES: u32 = 2; // kCFStreamEventHasBytesAvailable
const EV_CANACCEPT: u32 = 4; // kCFStreamEventCanAcceptBytes
const EV_ERROR: u32 = 8; // kCFStreamEventErrorOccurred
const EV_END: u32 = 16; // kCFStreamEventEndEncountered

// CFStreamStatus values.
const ST_NOTOPEN: i32 = 0;
const ST_OPENING: i32 = 1;
const ST_OPEN: i32 = 2;
const ST_ATEND: i32 = 5;
const ST_CLOSED: i32 = 6;
const ST_ERROR: i32 = 7;

/// One read/write pair sharing a single host TCP stream.
struct StreamPair {
    host: String,
    port: u16,
    tcp: Option<TcpStream>,
    opened: bool,
    connected: bool,
    error: bool,
    eof: bool,
    closed: bool,
    read_ref: id,
    write_ref: id,
    // client callback registration (raw guest fn ptr bits incl. thumb bit; 0 = none)
    read_cb: GuestUSize,
    read_flags: u32,
    read_info: MutVoidPtr,
    write_cb: GuestUSize,
    write_flags: u32,
    write_info: MutVoidPtr,
    scheduled: bool,
    open_fired_read: bool,
    open_fired_write: bool,
    end_fired: bool,
}

#[derive(Default)]
pub struct State {
    pairs: HashMap<u32, StreamPair>,
    next_id: u32,
}
impl State {
    fn get(env: &mut Environment) -> &mut State {
        &mut env.framework_state.core_foundation.cf_stream
    }
}

/// Host object for a CFReadStreamRef / CFWriteStreamRef. Both refs of a pair carry the
/// same `pair_id`; `is_write` selects the role.
struct CFStreamHostObject {
    pair_id: u32,
    is_write: bool,
}
impl HostObject for CFStreamHostObject {}

pub const CLASSES: ClassExports = objc_classes! {
(env, this, _cmd);
@implementation _touchHLE_CFStream: NSObject
@end
};

fn pair_id_of(env: &mut Environment, stream: id) -> Option<(u32, bool)> {
    if stream == nil {
        return None;
    }
    let ho = env.objc.borrow::<CFStreamHostObject>(stream);
    Some((ho.pair_id, ho.is_write))
}

/// Read the `info` pointer from a CFStreamClientContext* (`{ CFIndex version; void *info; ... }`).
fn context_info(env: &mut Environment, context: MutVoidPtr) -> MutVoidPtr {
    if context.is_null() {
        return Ptr::null();
    }
    env.mem.read(MutPtr::<MutVoidPtr>::from_bits(context.to_bits() + 4))
}

fn CFStreamCreatePairWithSocketToHost(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    host: id, // CFStringRef
    port: u32,
    read_stream_out: MutPtr<id>,
    write_stream_out: MutPtr<id>,
) {
    let host_str = if host == nil {
        String::new()
    } else {
        to_rust_string(env, host).to_string()
    };
    let id = {
        let st = State::get(env);
        let id = st.next_id;
        st.next_id += 1;
        id
    };
    let isa = env.objc.get_known_class("_touchHLE_CFStream", &mut env.mem);
    let read_ref = env.objc.alloc_object(
        isa,
        Box::new(CFStreamHostObject {
            pair_id: id,
            is_write: false,
        }),
        &mut env.mem,
    );
    let write_ref = env.objc.alloc_object(
        isa,
        Box::new(CFStreamHostObject {
            pair_id: id,
            is_write: true,
        }),
        &mut env.mem,
    );
    State::get(env).pairs.insert(
        id,
        StreamPair {
            host: host_str.clone(),
            port: port as u16,
            tcp: None,
            opened: false,
            connected: false,
            error: false,
            eof: false,
            closed: false,
            read_ref,
            write_ref,
            read_cb: 0,
            read_flags: 0,
            read_info: Ptr::null(),
            write_cb: 0,
            write_flags: 0,
            write_info: Ptr::null(),
            scheduled: false,
            open_fired_read: false,
            open_fired_write: false,
            end_fired: false,
        },
    );
    log!(
        "[CFStream] CFStreamCreatePairWithSocketToHost({:?}:{}) -> pair {} (read {:?} write {:?})",
        host_str,
        port,
        id,
        read_ref,
        write_ref
    );
    if !read_stream_out.is_null() {
        env.mem.write(read_stream_out, read_ref);
    }
    if !write_stream_out.is_null() {
        env.mem.write(write_stream_out, write_ref);
    }
}

fn CFReadStreamSetProperty(
    _env: &mut Environment,
    _stream: id,
    _property: id,
    _value: id,
) -> bool {
    // The only property AsyncSocket sets is kCFStreamPropertyShouldCloseNativeSocket=true,
    // which our pair already honours (we own + close the TcpStream). Accept + ignore.
    true
}
fn CFWriteStreamSetProperty(
    _env: &mut Environment,
    _stream: id,
    _property: id,
    _value: id,
) -> bool {
    true
}

fn CFReadStreamSetClient(
    env: &mut Environment,
    stream: id,
    flags: u32,
    callback: MutVoidPtr,
    context: MutVoidPtr,
) -> bool {
    let info = context_info(env, context);
    if let Some((id, _)) = pair_id_of(env, stream) {
        if let Some(p) = State::get(env).pairs.get_mut(&id) {
            p.read_cb = callback.to_bits();
            p.read_flags = flags;
            p.read_info = info;
        }
    }
    true
}
fn CFWriteStreamSetClient(
    env: &mut Environment,
    stream: id,
    flags: u32,
    callback: MutVoidPtr,
    context: MutVoidPtr,
) -> bool {
    let info = context_info(env, context);
    if let Some((id, _)) = pair_id_of(env, stream) {
        if let Some(p) = State::get(env).pairs.get_mut(&id) {
            p.write_cb = callback.to_bits();
            p.write_flags = flags;
            p.write_info = info;
        }
    }
    true
}

fn CFReadStreamScheduleWithRunLoop(env: &mut Environment, stream: id, _rl: id, _mode: id) {
    if let Some((id, _)) = pair_id_of(env, stream) {
        if let Some(p) = State::get(env).pairs.get_mut(&id) {
            p.scheduled = true;
        }
    }
}
fn CFWriteStreamScheduleWithRunLoop(env: &mut Environment, stream: id, _rl: id, _mode: id) {
    if let Some((id, _)) = pair_id_of(env, stream) {
        if let Some(p) = State::get(env).pairs.get_mut(&id) {
            p.scheduled = true;
        }
    }
}
fn CFReadStreamUnscheduleFromRunLoop(_env: &mut Environment, _stream: id, _rl: id, _mode: id) {}
fn CFWriteStreamUnscheduleFromRunLoop(_env: &mut Environment, _stream: id, _rl: id, _mode: id) {}

/// Resolve + connect (blocking, bounded) on first Open; both streams share the socket.
fn ensure_connected(env: &mut Environment, id: u32) {
    let (host, port, need) = {
        let Some(p) = State::get(env).pairs.get(&id) else {
            return;
        };
        (p.host.clone(), p.port, !p.connected && !p.error)
    };
    if !need {
        return;
    }
    let addr = (host.as_str(), port).to_socket_addrs().ok().and_then(|mut it| it.next());
    let result = match addr {
        Some(addr) => TcpStream::connect_timeout(&addr, Duration::from_secs(10)),
        None => {
            log!("[CFStream] pair {}: cannot resolve {}:{}", id, host, port);
            if let Some(p) = State::get(env).pairs.get_mut(&id) {
                p.error = true;
            }
            return;
        }
    };
    let Some(p) = State::get(env).pairs.get_mut(&id) else {
        return;
    };
    match result {
        Ok(tcp) => {
            tcp.set_nonblocking(true).ok();
            tcp.set_nodelay(true).ok();
            p.tcp = Some(tcp);
            p.connected = true;
            log!("[CFStream] pair {} connected to {}:{}", id, host, port);
        }
        Err(e) => {
            p.error = true;
            log!("[CFStream] pair {} connect to {}:{} failed: {}", id, host, port, e);
        }
    }
}

fn CFReadStreamOpen(env: &mut Environment, stream: id) -> bool {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return false;
    };
    ensure_connected(env, id);
    let p = State::get(env).pairs.get_mut(&id);
    match p {
        Some(p) => {
            p.opened = true;
            !p.error
        }
        None => false,
    }
}
fn CFWriteStreamOpen(env: &mut Environment, stream: id) -> bool {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return false;
    };
    ensure_connected(env, id);
    let p = State::get(env).pairs.get_mut(&id);
    match p {
        Some(p) => {
            p.opened = true;
            !p.error
        }
        None => false,
    }
}

fn close_pair(env: &mut Environment, id: u32) {
    if let Some(p) = State::get(env).pairs.get_mut(&id) {
        p.tcp = None;
        p.closed = true;
        p.connected = false;
    }
}
fn CFReadStreamClose(env: &mut Environment, stream: id) {
    if let Some((id, _)) = pair_id_of(env, stream) {
        close_pair(env, id);
    }
}
fn CFWriteStreamClose(env: &mut Environment, stream: id) {
    if let Some((id, _)) = pair_id_of(env, stream) {
        close_pair(env, id);
    }
}

fn CFReadStreamRead(
    env: &mut Environment,
    stream: id,
    buffer: MutVoidPtr,
    length: GuestUSize,
) -> i32 {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return -1;
    };
    let mut tmp = vec![0u8; length as usize];
    let n: i32 = {
        let Some(p) = State::get(env).pairs.get_mut(&id) else {
            return -1;
        };
        let Some(tcp) = p.tcp.as_mut() else {
            return -1;
        };
        match tcp.read(&mut tmp) {
            Ok(0) => {
                p.eof = true;
                0
            }
            Ok(n) => n as i32,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
            Err(_) => {
                p.error = true;
                -1
            }
        }
    };
    if n > 0 {
        env.mem
            .bytes_at_mut(buffer.cast(), n as GuestUSize)
            .copy_from_slice(&tmp[..n as usize]);
    }
    n
}

fn CFWriteStreamWrite(
    env: &mut Environment,
    stream: id,
    buffer: ConstVoidPtr,
    length: GuestUSize,
) -> i32 {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return -1;
    };
    let data = env.mem.bytes_at(buffer.cast(), length).to_vec();
    let Some(p) = State::get(env).pairs.get_mut(&id) else {
        return -1;
    };
    let Some(tcp) = p.tcp.as_mut() else {
        return -1;
    };
    match tcp.write(&data) {
        Ok(n) => n as i32,
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
        Err(_) => {
            p.error = true;
            -1
        }
    }
}

fn CFReadStreamHasBytesAvailable(env: &mut Environment, stream: id) -> bool {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return false;
    };
    let Some(p) = State::get(env).pairs.get(&id) else {
        return false;
    };
    let Some(tcp) = p.tcp.as_ref() else {
        return false;
    };
    let mut b = [0u8; 1];
    matches!(tcp.peek(&mut b), Ok(n) if n > 0)
}
fn CFWriteStreamCanAcceptBytes(env: &mut Environment, stream: id) -> bool {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return false;
    };
    State::get(env)
        .pairs
        .get(&id)
        .map(|p| p.connected && !p.error && !p.closed)
        .unwrap_or(false)
}

fn CFReadStreamGetStatus(env: &mut Environment, stream: id) -> i32 {
    stream_status(env, stream)
}
fn CFWriteStreamGetStatus(env: &mut Environment, stream: id) -> i32 {
    stream_status(env, stream)
}
fn stream_status(env: &mut Environment, stream: id) -> i32 {
    let Some((id, _)) = pair_id_of(env, stream) else {
        return ST_ERROR;
    };
    match State::get(env).pairs.get(&id) {
        Some(p) if p.error => ST_ERROR,
        Some(p) if p.closed => ST_CLOSED,
        Some(p) if p.eof => ST_ATEND,
        Some(p) if p.connected => ST_OPEN,
        Some(p) if p.opened => ST_OPENING,
        Some(_) => ST_NOTOPEN,
        None => ST_ERROR,
    }
}

fn CFReadStreamCopyError(_env: &mut Environment, _stream: id) -> CFTypeRef {
    Ptr::null()
}
fn CFWriteStreamCopyError(_env: &mut Environment, _stream: id) -> CFTypeRef {
    Ptr::null()
}

/// Invoke a guest CFStream client callback: `cb(stream, eventType, info)`.
fn fire(env: &mut Environment, cb_bits: GuestUSize, stream: id, event: u32, info: MutVoidPtr) {
    if cb_bits == 0 || stream == nil {
        return;
    }
    // [MOLE-READ-DIAG] log everything except the per-frame CanAcceptBytes spam, so we can see
    // OpenCompleted(1)/HasBytesAvailable(2)/Error(8)/End(16) actually reaching the AsyncSocket.
    if event != EV_CANACCEPT {
        log!(
            "[CFStream] fire event={} stream={:?} cb={:#x}",
            event,
            stream,
            cb_bits
        );
    }
    let f = GuestFunction::from_addr_with_thumb_bit(cb_bits);
    let _: () = f.call_from_host(env, (stream, event, info));
}

enum Peek {
    Bytes,
    Eof,
    Err,
    Nothing,
}

/// Pump every scheduled stream pair once. Called from the NSRunLoop tick. Fires
/// OpenCompleted, then HasBytesAvailable / CanAcceptBytes / End / Error as the underlying
/// non-blocking socket dictates. Must not hold a State borrow across a callback (the
/// callback re-enters CFRead/WriteStream* and mutates State).
pub fn drive_streams(env: &mut Environment) {
    let ids: Vec<u32> = {
        let st = State::get(env);
        if st.pairs.is_empty() {
            return;
        }
        st.pairs.keys().copied().collect()
    };
    for id in ids {
        // (a) read OpenCompleted
        if let Some((cb, r, info)) = {
            let st = State::get(env);
            match st.pairs.get_mut(&id) {
                Some(p)
                    if p.connected
                        && p.scheduled
                        && !p.open_fired_read
                        && (p.read_flags & EV_OPEN) != 0 =>
                {
                    p.open_fired_read = true;
                    Some((p.read_cb, p.read_ref, p.read_info))
                }
                _ => None,
            }
        } {
            fire(env, cb, r, EV_OPEN, info);
        }
        // (b) write OpenCompleted
        if let Some((cb, r, info)) = {
            let st = State::get(env);
            match st.pairs.get_mut(&id) {
                Some(p)
                    if p.connected
                        && p.scheduled
                        && !p.open_fired_write
                        && (p.write_flags & EV_OPEN) != 0 =>
                {
                    p.open_fired_write = true;
                    Some((p.write_cb, p.write_ref, p.write_info))
                }
                _ => None,
            }
        } {
            fire(env, cb, r, EV_OPEN, info);
        }
        // (c) read: HasBytesAvailable / EndEncountered / ErrorOccurred
        let peek = {
            let st = State::get(env);
            match st.pairs.get(&id) {
                Some(p)
                    if p.connected
                        && p.scheduled
                        && p.open_fired_read
                        && !p.error
                        && !p.eof
                        && !p.closed
                        && (p.read_flags & (EV_HASBYTES | EV_END | EV_ERROR)) != 0 =>
                {
                    match p.tcp.as_ref() {
                        Some(tcp) => {
                            let mut b = [0u8; 1];
                            match tcp.peek(&mut b) {
                                Ok(0) => Peek::Eof,
                                Ok(_) => Peek::Bytes,
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    Peek::Nothing
                                }
                                Err(_) => Peek::Err,
                            }
                        }
                        None => Peek::Nothing,
                    }
                }
                _ => Peek::Nothing,
            }
        };
        match peek {
            Peek::Bytes => {
                let (cb, r, info, flags) = read_client(env, id);
                if flags & EV_HASBYTES != 0 {
                    fire(env, cb, r, EV_HASBYTES, info);
                }
            }
            Peek::Eof => {
                let (cb, r, info, flags) = read_client(env, id);
                if let Some(p) = State::get(env).pairs.get_mut(&id) {
                    p.eof = true;
                    p.end_fired = true;
                }
                if flags & EV_END != 0 {
                    fire(env, cb, r, EV_END, info);
                }
            }
            Peek::Err => {
                let (cb, r, info, flags) = read_client(env, id);
                if let Some(p) = State::get(env).pairs.get_mut(&id) {
                    p.error = true;
                }
                if flags & EV_ERROR != 0 {
                    fire(env, cb, r, EV_ERROR, info);
                }
            }
            Peek::Nothing => {}
        }
        // (d) write CanAcceptBytes (level: connected socket can almost always accept;
        //     AsyncSocket writes its queued packet here, or no-ops if nothing pending)
        if let Some((cb, r, info)) = {
            let st = State::get(env);
            match st.pairs.get(&id) {
                Some(p)
                    if p.connected
                        && p.scheduled
                        && p.open_fired_write
                        && !p.error
                        && !p.closed
                        && (p.write_flags & EV_CANACCEPT) != 0 =>
                {
                    Some((p.write_cb, p.write_ref, p.write_info))
                }
                _ => None,
            }
        } {
            fire(env, cb, r, EV_CANACCEPT, info);
        }
    }
}

fn read_client(env: &mut Environment, id: u32) -> (GuestUSize, id, MutVoidPtr, u32) {
    match State::get(env).pairs.get(&id) {
        Some(p) => (p.read_cb, p.read_ref, p.read_info, p.read_flags),
        None => (0, nil, Ptr::null(), 0),
    }
}

pub const CONSTANTS: ConstantExports = &[(
    "_kCFStreamPropertyShouldCloseNativeSocket",
    HostConstant::NSString("kCFStreamPropertyShouldCloseNativeSocket"),
)];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFStreamCreatePairWithSocketToHost(_, _, _, _, _)),
    export_c_func!(CFReadStreamSetProperty(_, _, _)),
    export_c_func!(CFWriteStreamSetProperty(_, _, _)),
    export_c_func!(CFReadStreamSetClient(_, _, _, _)),
    export_c_func!(CFWriteStreamSetClient(_, _, _, _)),
    export_c_func!(CFReadStreamScheduleWithRunLoop(_, _, _)),
    export_c_func!(CFWriteStreamScheduleWithRunLoop(_, _, _)),
    export_c_func!(CFReadStreamUnscheduleFromRunLoop(_, _, _)),
    export_c_func!(CFWriteStreamUnscheduleFromRunLoop(_, _, _)),
    export_c_func!(CFReadStreamOpen(_)),
    export_c_func!(CFWriteStreamOpen(_)),
    export_c_func!(CFReadStreamClose(_)),
    export_c_func!(CFWriteStreamClose(_)),
    export_c_func!(CFReadStreamRead(_, _, _)),
    export_c_func!(CFWriteStreamWrite(_, _, _)),
    export_c_func!(CFReadStreamHasBytesAvailable(_)),
    export_c_func!(CFWriteStreamCanAcceptBytes(_)),
    export_c_func!(CFReadStreamGetStatus(_)),
    export_c_func!(CFWriteStreamGetStatus(_)),
    export_c_func!(CFReadStreamCopyError(_)),
    export_c_func!(CFWriteStreamCopyError(_)),
];
