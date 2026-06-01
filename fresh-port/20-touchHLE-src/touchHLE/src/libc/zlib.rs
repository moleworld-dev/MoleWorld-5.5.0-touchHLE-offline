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
use std::io::Read;

// zlib return codes
const Z_OK: i32 = 0;
const Z_BUF_ERROR: i32 = -5;
const Z_DATA_ERROR: i32 = -3;

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

pub const FUNCTIONS: FunctionExports = &[export_c_func!(uncompress(_, _, _, _))];
