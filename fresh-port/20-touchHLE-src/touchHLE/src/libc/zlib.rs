/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! zlib (`libz`) — just enough of the high-level API for apps that link
//! `/usr/lib/libz.1.dylib`.
//!
//! MoleWorld (and cocos2d-iphone's `ZipUtils`) call `uncompress()` to inflate
//! `.pvr.ccz` texture atlases (a `CCZ!` header wrapping a zlib stream). Without
//! this, those atlases never decompress and every sprite drawn from them (e.g.
//! the main-menu buttons) renders as garbage.

use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{ConstPtr, MutPtr};
use crate::Environment;
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};

// zlib return codes
const Z_OK: i32 = 0;
const Z_STREAM_END: i32 = 1;
const Z_STREAM_ERROR: i32 = -2;
const Z_BUF_ERROR: i32 = -5;
const Z_DATA_ERROR: i32 = -3;

// z_stream field byte offsets (32-bit / armv7 layout).
const OFF_NEXT_IN: u32 = 0;
const OFF_AVAIL_IN: u32 = 4;
const OFF_TOTAL_IN: u32 = 8;
const OFF_NEXT_OUT: u32 = 12;
const OFF_AVAIL_OUT: u32 = 16;
const OFF_TOTAL_OUT: u32 = 20;
const OFF_MSG: u32 = 24;
const OFF_STATE: u32 = 28;

/// Per-z_stream inflate state. The game's NSData `gzipInflate` sets `next_in` to the WHOLE
/// compressed blob once, then loops `inflate()` growing its output buffer until Z_STREAM_END. We
/// therefore decompress the entire input on the first `inflate()` call and then stream the result
/// out across the loop iterations.
struct InflateState {
    out: Vec<u8>,
    pos: usize,
    window_bits: i32,
    started: bool,
}

fn inflate_states() -> std::sync::MutexGuard<'static, HashMap<u32, InflateState>> {
    static STATES: OnceLock<Mutex<HashMap<u32, InflateState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap()
}

fn rd_u32(env: &Environment, strm: u32, off: u32) -> u32 {
    env.mem.read(ConstPtr::<u32>::from_bits(strm + off))
}
fn wr_u32(env: &mut Environment, strm: u32, off: u32, v: u32) {
    env.mem.write(MutPtr::<u32>::from_bits(strm + off), v)
}

/// Inflate a complete deflate/zlib/gzip stream into a Vec, auto-detecting the wrapper from the
/// magic bytes (with `windowBits` as a hint) and falling back across formats. Returns None if no
/// decoder succeeds.
fn inflate_all(input: &[u8], window_bits: i32) -> Option<Vec<u8>> {
    use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
    let is_gzip = input.len() >= 2 && input[0] == 0x1f && input[1] == 0x8b;
    let is_zlib = !is_gzip && !input.is_empty() && (input[0] & 0x0f) == 8;
    let try_order: [u8; 3] = if is_gzip || window_bits == 31 || window_bits == 47 {
        [b'g', b'z', b'r']
    } else if is_zlib || (8..=15).contains(&window_bits) {
        [b'z', b'g', b'r']
    } else {
        [b'r', b'g', b'z']
    };
    for fmt in try_order {
        let mut out = Vec::new();
        let ok = match fmt {
            b'g' => GzDecoder::new(input).read_to_end(&mut out).is_ok(),
            b'z' => ZlibDecoder::new(input).read_to_end(&mut out).is_ok(),
            _ => DeflateDecoder::new(input).read_to_end(&mut out).is_ok(),
        };
        if ok {
            return Some(out);
        }
    }
    None
}

/// `int inflateInit2_(z_streamp strm, int windowBits, const char *version, int stream_size)`
/// (the `inflateInit2` macro expands to this). Sets up our host-side stream state.
fn inflateInit2_(
    env: &mut Environment,
    strm: MutPtr<u8>,
    window_bits: i32,
    _version: ConstPtr<u8>,
    _stream_size: i32,
) -> i32 {
    let s = strm.to_bits();
    inflate_states().insert(
        s,
        InflateState { out: Vec::new(), pos: 0, window_bits, started: false },
    );
    wr_u32(env, s, OFF_STATE, 1); // non-null marker (zlib refuses to inflate if state==NULL)
    wr_u32(env, s, OFF_MSG, 0);
    wr_u32(env, s, OFF_TOTAL_IN, 0);
    wr_u32(env, s, OFF_TOTAL_OUT, 0);
    Z_OK
}

/// `int inflateInit_(z_streamp strm, const char *version, int stream_size)` — zlib-format default.
fn inflateInit_(
    env: &mut Environment,
    strm: MutPtr<u8>,
    version: ConstPtr<u8>,
    stream_size: i32,
) -> i32 {
    inflateInit2_(env, strm, 15, version, stream_size)
}

/// `int inflate(z_streamp strm, int flush)` — decompress, streaming the result across calls.
fn inflate(env: &mut Environment, strm: MutPtr<u8>, _flush: i32) -> i32 {
    let s = strm.to_bits();
    let next_in = rd_u32(env, s, OFF_NEXT_IN);
    let avail_in = rd_u32(env, s, OFF_AVAIL_IN);

    // First call (with the full input): pull it out of guest memory and decompress everything.
    let input = if avail_in > 0 {
        env.mem
            .bytes_at(ConstPtr::<u8>::from_bits(next_in), avail_in)
            .to_vec()
    } else {
        Vec::new()
    };
    {
        let mut states = inflate_states();
        let Some(st) = states.get_mut(&s) else {
            return Z_STREAM_ERROR;
        };
        if !st.started {
            st.started = true;
            match inflate_all(&input, st.window_bits) {
                Some(out) => {
                    log!(
                        "[MOLECHEAT] inflate: {} bytes in -> {} bytes out (windowBits={})",
                        input.len(),
                        out.len(),
                        st.window_bits
                    );
                    st.out = out;
                }
                None => {
                    log!("[MOLECHEAT] inflate: FAILED to decompress {} bytes", input.len());
                    return Z_DATA_ERROR;
                }
            }
        }
    }
    // Mark the input consumed.
    if avail_in > 0 {
        let total_in = rd_u32(env, s, OFF_TOTAL_IN);
        wr_u32(env, s, OFF_NEXT_IN, next_in + avail_in);
        wr_u32(env, s, OFF_AVAIL_IN, 0);
        wr_u32(env, s, OFF_TOTAL_IN, total_in + avail_in);
    }

    // Write as much pending output as fits in next_out/avail_out.
    let next_out = rd_u32(env, s, OFF_NEXT_OUT);
    let avail_out = rd_u32(env, s, OFF_AVAIL_OUT);
    let total_out = rd_u32(env, s, OFF_TOTAL_OUT);
    let (chunk, done) = {
        let mut states = inflate_states();
        let Some(st) = states.get_mut(&s) else {
            return Z_STREAM_ERROR;
        };
        let remaining = st.out.len() - st.pos;
        let n = remaining.min(avail_out as usize);
        let chunk = st.out[st.pos..st.pos + n].to_vec();
        st.pos += n;
        (chunk, st.pos >= st.out.len())
    };
    if !chunk.is_empty() {
        let n = chunk.len() as u32;
        env.mem
            .bytes_at_mut(MutPtr::<u8>::from_bits(next_out), n)
            .copy_from_slice(&chunk);
        wr_u32(env, s, OFF_NEXT_OUT, next_out + n);
        wr_u32(env, s, OFF_AVAIL_OUT, avail_out - n);
        wr_u32(env, s, OFF_TOTAL_OUT, total_out + n);
    }
    if done {
        Z_STREAM_END
    } else if chunk.is_empty() && avail_out > 0 {
        // No progress possible and output buffer has room: nothing left to produce.
        Z_STREAM_END
    } else {
        Z_OK
    }
}

/// `int inflateEnd(z_streamp strm)`
fn inflateEnd(_env: &mut Environment, strm: MutPtr<u8>) -> i32 {
    inflate_states().remove(&strm.to_bits());
    Z_OK
}

/// `int inflateReset(z_streamp strm)`
fn inflateReset(env: &mut Environment, strm: MutPtr<u8>) -> i32 {
    let s = strm.to_bits();
    if let Some(st) = inflate_states().get_mut(&s) {
        st.out.clear();
        st.pos = 0;
        st.started = false;
    }
    wr_u32(env, s, OFF_TOTAL_IN, 0);
    wr_u32(env, s, OFF_TOTAL_OUT, 0);
    Z_OK
}

// ===== deflate(压缩)族 —— 对称镜像 inflate =====
// 庄园持久化命门:游戏 -[NSData gzipDeflate]@0x2fb810 经 deflateInit2_(windowBits=31=gzip)+deflate
// +deflateEnd 把存档地图压成 gzip 上传(cmd 1019)。touchHLE 原只实现 inflate(下行解压),deflate
// 是 no-op stub→gzipDeflate 返空→地图整图发 0B→服务端存不下→退出重进布局没了。补全即闭环。

/// Per-z_stream deflate state(镜像 InflateState)。游戏一次性把整段待压数据塞 next_in 后循环
/// deflate(Z_FINISH) 取输出;故首次 deflate() 一次性压完整输入,再跨循环把结果流式吐出。
struct DeflateState {
    out: Vec<u8>,
    pos: usize,
    window_bits: i32,
    level: i32,
    started: bool,
}

fn deflate_states() -> std::sync::MutexGuard<'static, HashMap<u32, DeflateState>> {
    static STATES: OnceLock<Mutex<HashMap<u32, DeflateState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap()
}

/// 把整段输入压成 gzip/zlib/raw deflate(按 windowBits 选 wrapper),对称于 inflate_all。
/// windowBits:>15(如 31=16+15)→gzip;8..=15→zlib;<0→raw deflate。level<0/>9 取默认 6。
fn deflate_all(input: &[u8], window_bits: i32, level: i32) -> Vec<u8> {
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use flate2::Compression;
    use std::io::Write;
    let comp = Compression::new(if (0..=9).contains(&level) { level as u32 } else { 6 });
    let mut out = Vec::new();
    if window_bits > 15 {
        let mut e = GzEncoder::new(&mut out, comp);
        let _ = e.write_all(input);
        let _ = e.finish();
    } else if window_bits < 0 {
        let mut e = DeflateEncoder::new(&mut out, comp);
        let _ = e.write_all(input);
        let _ = e.finish();
    } else {
        let mut e = ZlibEncoder::new(&mut out, comp);
        let _ = e.write_all(input);
        let _ = e.finish();
    }
    out
}

/// `int deflateInit2_(z_streamp strm, int level, int method, int windowBits, int memLevel,
///                    int strategy, const char *version, int stream_size)`(deflateInit2 宏展开)。
fn deflateInit2_(
    env: &mut Environment,
    strm: MutPtr<u8>,
    level: i32,
    _method: i32,
    window_bits: i32,
    _mem_level: i32,
    _strategy: i32,
    _version: ConstPtr<u8>,
    _stream_size: i32,
) -> i32 {
    let s = strm.to_bits();
    deflate_states().insert(
        s,
        DeflateState { out: Vec::new(), pos: 0, window_bits, level, started: false },
    );
    wr_u32(env, s, OFF_STATE, 1); // 非空标记
    wr_u32(env, s, OFF_MSG, 0);
    wr_u32(env, s, OFF_TOTAL_IN, 0);
    wr_u32(env, s, OFF_TOTAL_OUT, 0);
    Z_OK
}

/// `int deflateInit_(z_streamp strm, int level, const char *version, int stream_size)` — zlib 默认。
fn deflateInit_(
    env: &mut Environment,
    strm: MutPtr<u8>,
    level: i32,
    version: ConstPtr<u8>,
    stream_size: i32,
) -> i32 {
    deflateInit2_(env, strm, level, 8, 15, 8, 0, version, stream_size)
}

/// `int deflate(z_streamp strm, int flush)` — 首次拉全部输入压完,再跨调流式吐出(镜像 inflate)。
fn deflate(env: &mut Environment, strm: MutPtr<u8>, _flush: i32) -> i32 {
    let s = strm.to_bits();
    let next_in = rd_u32(env, s, OFF_NEXT_IN);
    let avail_in = rd_u32(env, s, OFF_AVAIL_IN);

    let input = if avail_in > 0 {
        env.mem
            .bytes_at(ConstPtr::<u8>::from_bits(next_in), avail_in)
            .to_vec()
    } else {
        Vec::new()
    };
    {
        let mut states = deflate_states();
        let Some(st) = states.get_mut(&s) else {
            return Z_STREAM_ERROR;
        };
        if !st.started {
            st.started = true;
            let out = deflate_all(&input, st.window_bits, st.level);
            log!(
                "[MOLECHEAT] deflate: {} bytes in -> {} bytes out (windowBits={})",
                input.len(),
                out.len(),
                st.window_bits
            );
            st.out = out;
        }
    }
    if avail_in > 0 {
        let total_in = rd_u32(env, s, OFF_TOTAL_IN);
        wr_u32(env, s, OFF_NEXT_IN, next_in + avail_in);
        wr_u32(env, s, OFF_AVAIL_IN, 0);
        wr_u32(env, s, OFF_TOTAL_IN, total_in + avail_in);
    }

    let next_out = rd_u32(env, s, OFF_NEXT_OUT);
    let avail_out = rd_u32(env, s, OFF_AVAIL_OUT);
    let total_out = rd_u32(env, s, OFF_TOTAL_OUT);
    let (chunk, done) = {
        let mut states = deflate_states();
        let Some(st) = states.get_mut(&s) else {
            return Z_STREAM_ERROR;
        };
        let remaining = st.out.len() - st.pos;
        let n = remaining.min(avail_out as usize);
        let chunk = st.out[st.pos..st.pos + n].to_vec();
        st.pos += n;
        (chunk, st.pos >= st.out.len())
    };
    if !chunk.is_empty() {
        let n = chunk.len() as u32;
        env.mem
            .bytes_at_mut(MutPtr::<u8>::from_bits(next_out), n)
            .copy_from_slice(&chunk);
        wr_u32(env, s, OFF_NEXT_OUT, next_out + n);
        wr_u32(env, s, OFF_AVAIL_OUT, avail_out - n);
        wr_u32(env, s, OFF_TOTAL_OUT, total_out + n);
    }
    if done {
        Z_STREAM_END
    } else if chunk.is_empty() && avail_out > 0 {
        Z_STREAM_END
    } else {
        Z_OK
    }
}

/// `int deflateEnd(z_streamp strm)`
fn deflateEnd(_env: &mut Environment, strm: MutPtr<u8>) -> i32 {
    deflate_states().remove(&strm.to_bits());
    Z_OK
}

/// `int uncompress(Bytef *dest, uLongf *destLen, const Bytef *source, uLong sourceLen)`
///
/// Inflates a complete zlib stream. `destLen` is in/out: on entry the capacity
/// of `dest`, on return the number of bytes actually written.
fn uncompress(
    env: &mut Environment,
    dest: MutPtr<u8>,
    dest_len: MutPtr<u32>,
    source: ConstPtr<u8>,
    source_len: u32,
) -> i32 {
    let cap = env.mem.read(dest_len);

    // Copy the compressed input out of guest memory first (decompression
    // borrows env.mem immutably; we then need it mutably to write the output).
    let input = env.mem.bytes_at(source, source_len).to_vec();

    let mut decoder = flate2::read::ZlibDecoder::new(&input[..]);
    let mut output: Vec<u8> = Vec::new();
    if let Err(e) = decoder.read_to_end(&mut output) {
        log!("uncompress: zlib inflate failed: {}", e);
        return Z_DATA_ERROR;
    }

    if (output.len() as u64) > (cap as u64) {
        log!(
            "uncompress: output {} bytes exceeds dest capacity {} bytes",
            output.len(),
            cap
        );
        return Z_BUF_ERROR;
    }

    let out_len = output.len() as u32;
    env.mem.bytes_at_mut(dest, out_len).copy_from_slice(&output);
    env.mem.write(dest_len, out_len);
    Z_OK
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(uncompress(_, _, _, _)),
    export_c_func!(inflateInit2_(_, _, _, _)),
    export_c_func!(inflateInit_(_, _, _)),
    export_c_func!(inflate(_, _)),
    export_c_func!(inflateEnd(_)),
    export_c_func!(inflateReset(_)),
    export_c_func!(deflateInit2_(_, _, _, _, _, _, _, _)),
    export_c_func!(deflateInit_(_, _, _, _)),
    export_c_func!(deflate(_, _)),
    export_c_func!(deflateEnd(_)),
];
