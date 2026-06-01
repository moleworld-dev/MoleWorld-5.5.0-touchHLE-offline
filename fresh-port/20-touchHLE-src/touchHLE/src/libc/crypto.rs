/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CommonCrypto and friends

use crate::dyld::FunctionExports;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr};
use crate::{export_c_func, Environment};
use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{
    BlockCipherDecrypt, BlockCipherEncrypt, BlockModeDecrypt, BlockModeEncrypt, KeyInit, KeyIvInit,
};
use digest::Digest;
use md5::Md5;
use sha1::Sha1;

const AES_BS: usize = 16;

/// Encrypt/decrypt one 16-byte block in place. cipher 0.5 blocks are
/// `Array<u8, U16>` (hybrid-array), constructible from `[u8;16]` via `.into()`.
fn ecb_enc<C: BlockCipherEncrypt<BlockSize = aes::cipher::consts::U16>>(c: &C, chunk: &mut [u8]) {
    let mut a = [0u8; AES_BS];
    a.copy_from_slice(chunk);
    let mut blk = a.into();
    c.encrypt_block(&mut blk);
    let out: [u8; AES_BS] = blk.into();
    chunk.copy_from_slice(&out);
}
fn ecb_dec<C: BlockCipherDecrypt<BlockSize = aes::cipher::consts::U16>>(c: &C, chunk: &mut [u8]) {
    let mut a = [0u8; AES_BS];
    a.copy_from_slice(chunk);
    let mut blk = a.into();
    c.decrypt_block(&mut blk);
    let out: [u8; AES_BS] = blk.into();
    chunk.copy_from_slice(&out);
}

/// AES-ECB with optional PKCS7 padding, block-by-block over the raw block cipher
/// (the `cbc` crate only gives us CBC). Used by MoleWorld's immob (InMobi) SDK,
/// which decrypts its config blobs with AES-ECB; without this, CCCrypt returned
/// kCCParamError and the SDK computed a bogus (negative) buffer size.
fn aes_ecb<C>(cipher: &C, encrypt: bool, input: &[u8], padding: bool) -> Option<Vec<u8>>
where
    C: BlockCipherEncrypt<BlockSize = aes::cipher::consts::U16>
        + BlockCipherDecrypt<BlockSize = aes::cipher::consts::U16>,
{
    if encrypt {
        let mut data = input.to_vec();
        if padding {
            let pad = AES_BS - (data.len() % AES_BS);
            data.extend(std::iter::repeat(pad as u8).take(pad));
        } else if data.len() % AES_BS != 0 {
            return None;
        }
        for chunk in data.chunks_exact_mut(AES_BS) {
            ecb_enc(cipher, chunk);
        }
        Some(data)
    } else {
        if input.is_empty() || input.len() % AES_BS != 0 {
            return None;
        }
        let mut data = input.to_vec();
        for chunk in data.chunks_exact_mut(AES_BS) {
            ecb_dec(cipher, chunk);
        }
        if padding {
            let pad = *data.last()? as usize;
            if pad == 0 || pad > AES_BS || pad > data.len() {
                return None;
            }
            if data[data.len() - pad..].iter().any(|&b| b as usize != pad) {
                return None;
            }
            data.truncate(data.len() - pad);
        }
        Some(data)
    }
}

fn CC_MD5(env: &mut Environment, data: ConstVoidPtr, len: u32, md: MutPtr<u8>) -> MutPtr<u8> {
    let mut hasher = Md5::new();
    // Tolerate a null/zero-length input: hash the empty string rather than
    // null-page-faulting. Some code (e.g. TalkingData's TDGAUtility md5String:)
    // passes a null data pointer for empty input.
    if !data.is_null() && len != 0 {
        hasher.update(env.mem.bytes_at(data.cast(), len));
    }
    let digest = hasher.finalize();
    if !md.is_null() {
        env.mem.bytes_at_mut(md, 16).copy_from_slice(&digest[..]);
    }
    md
}

fn CC_SHA1(env: &mut Environment, data: ConstVoidPtr, len: u32, md: MutPtr<u8>) -> MutPtr<u8> {
    let mut hasher = Sha1::new();
    if !data.is_null() && len != 0 {
        hasher.update(env.mem.bytes_at(data.cast(), len));
    }
    let digest = hasher.finalize();
    if !md.is_null() {
        env.mem.bytes_at_mut(md, 20).copy_from_slice(&digest[..]);
    }
    md
}

// --- CommonCrypto CCCryptor (subset) ---
// Constants from <CommonCrypto/CommonCryptor.h>.
const KCC_ENCRYPT: u32 = 0;
const KCC_DECRYPT: u32 = 1;
const KCC_ALGORITHM_AES: u32 = 0; // kCCAlgorithmAES / kCCAlgorithmAES128
const KCC_OPTION_PKCS7PADDING: u32 = 0x0001;
const KCC_OPTION_ECB_MODE: u32 = 0x0002;
const KCC_SUCCESS: i32 = 0;
const KCC_PARAM_ERROR: i32 = -4300;
const KCC_BUFFER_TOO_SMALL: i32 = -4301;
const KCC_DECODE_ERROR: i32 = -4304;

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

enum AesKey {
    K128([u8; 16]),
    K256([u8; 32]),
}

/// `CCCryptorStatus CCCrypt(CCOperation op, CCAlgorithm alg, CCOptions options,
///     const void *key, size_t keyLength, const void *iv,
///     const void *dataIn, size_t dataInLength,
///     void *dataOut, size_t dataOutAvailable, size_t *dataOutMoved)`
///
/// One-shot symmetric encryption/decryption. We implement AES-128/256 in CBC
/// (and ECB) mode with optional PKCS7 padding, which covers what MoleWorld uses
/// to (de)crypt its packed resources: -[NSData immobViewAES256DecryptWithKey:]
/// calls CCCrypt(decrypt, AES, PKCS7, key=16 bytes, iv, ...) =>
/// AES-128-CBC/PKCS7 with key == iv == "39653543fa0d66aa".
#[allow(clippy::too_many_arguments)]
fn CCCrypt(
    env: &mut Environment,
    op: u32,
    alg: u32,
    options: u32,
    key: ConstVoidPtr,
    key_length: GuestUSize,
    iv: ConstVoidPtr,
    data_in: ConstVoidPtr,
    data_in_length: GuestUSize,
    data_out: MutVoidPtr,
    data_out_available: GuestUSize,
    data_out_moved: MutPtr<GuestUSize>,
) -> i32 {
    if alg != KCC_ALGORITHM_AES {
        log!("CCCrypt: unsupported algorithm {}, returning kCCParamError", alg);
        return KCC_PARAM_ERROR;
    }
    let ecb = options & KCC_OPTION_ECB_MODE != 0;
    let padding = options & KCC_OPTION_PKCS7PADDING != 0;

    let key_bytes = if key.is_null() || key_length == 0 {
        Vec::new()
    } else {
        env.mem.bytes_at(key.cast(), key_length).to_vec()
    };
    let aes_key = match key_length {
        16 => {
            let mut a = [0u8; 16];
            a.copy_from_slice(&key_bytes);
            AesKey::K128(a)
        }
        32 => {
            let mut a = [0u8; 32];
            a.copy_from_slice(&key_bytes);
            AesKey::K256(a)
        }
        _ => {
            log!("CCCrypt: unsupported AES key length {}, kCCParamError", key_length);
            return KCC_PARAM_ERROR;
        }
    };

    // IV: 16 bytes for CBC; null/ECB => all-zero.
    let mut iv16 = [0u8; 16];
    if !ecb && !iv.is_null() {
        iv16.copy_from_slice(env.mem.bytes_at(iv.cast(), 16));
    }

    let input = if data_in.is_null() || data_in_length == 0 {
        Vec::new()
    } else {
        env.mem.bytes_at(data_in.cast(), data_in_length).to_vec()
    };

    let out: Vec<u8> = match (op, padding, ecb, &aes_key) {
        // CBC + PKCS7 (the path MoleWorld actually uses)
        (KCC_ENCRYPT, true, false, AesKey::K128(k)) => {
            Aes128CbcEnc::new(k.into(), &iv16.into()).encrypt_padded_vec::<Pkcs7>(&input)
        }
        (KCC_DECRYPT, true, false, AesKey::K128(k)) => {
            match Aes128CbcDec::new(k.into(), &iv16.into()).decrypt_padded_vec::<Pkcs7>(&input) {
                Ok(v) => v,
                Err(_) => return KCC_DECODE_ERROR,
            }
        }
        (KCC_ENCRYPT, true, false, AesKey::K256(k)) => {
            Aes256CbcEnc::new(k.into(), &iv16.into()).encrypt_padded_vec::<Pkcs7>(&input)
        }
        (KCC_DECRYPT, true, false, AesKey::K256(k)) => {
            match Aes256CbcDec::new(k.into(), &iv16.into()).decrypt_padded_vec::<Pkcs7>(&input) {
                Ok(v) => v,
                Err(_) => return KCC_DECODE_ERROR,
            }
        }
        // ECB (used by the immob/InMobi SDK for its config blobs).
        (op_, pad_, true, AesKey::K128(k)) => {
            let cipher = aes::Aes128::new(k.into());
            match aes_ecb(&cipher, op_ == KCC_ENCRYPT, &input, pad_) {
                Some(v) => v,
                None => return KCC_DECODE_ERROR,
            }
        }
        (op_, pad_, true, AesKey::K256(k)) => {
            let cipher = aes::Aes256::new(k.into());
            match aes_ecb(&cipher, op_ == KCC_ENCRYPT, &input, pad_) {
                Some(v) => v,
                None => return KCC_DECODE_ERROR,
            }
        }
        // CBC without padding, or anything else not covered above.
        _ => {
            log!(
                "CCCrypt: unsupported combination op={} padding={} ecb={}, kCCParamError",
                op, padding, ecb
            );
            return KCC_PARAM_ERROR;
        }
    };

    if (out.len() as GuestUSize) > data_out_available {
        return KCC_BUFFER_TOO_SMALL;
    }
    if !data_out.is_null() && !out.is_empty() {
        env.mem
            .bytes_at_mut(data_out.cast(), out.len() as GuestUSize)
            .copy_from_slice(&out);
    }
    if !data_out_moved.is_null() {
        env.mem.write(data_out_moved, out.len() as GuestUSize);
    }
    KCC_SUCCESS
}

/// `void __assert_rtn(const char *func, const char *file, int line, const char *expr)`
///
/// Darwin's assertion-failure handler — what the C `assert()` macro calls when a
/// condition is false. touchHLE never implemented it, so a single failing guest
/// assert aborted the whole emulator ("Call to unimplemented function
/// ___assert_rtn"). We log which assertion failed (expr + file:line) and then
/// RETURN. For the standard `assert()` expansion `if (!cond) __assert_rtn(...)`,
/// returning here is equivalent to "pretend the assertion passed" and lets the
/// game continue — exactly the make-blocking-safe behaviour we want for porting.
fn __assert_rtn(
    env: &mut Environment,
    func: ConstPtr<u8>,
    file: ConstPtr<u8>,
    line: i32,
    expr: ConstPtr<u8>,
) {
    fn read_cstr(env: &Environment, ptr: ConstPtr<u8>) -> String {
        if ptr.is_null() {
            return "<null>".to_string();
        }
        let mut bytes = Vec::new();
        let mut addr = ptr.to_bits();
        for _ in 0..512 {
            let b: u8 = env.mem.read(ConstPtr::<u8>::from_bits(addr));
            if b == 0 {
                break;
            }
            bytes.push(b);
            addr += 1;
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
    let func_s = read_cstr(env, func);
    let file_s = read_cstr(env, file);
    let expr_s = read_cstr(env, expr);
    log!(
        "Guest assertion FAILED: ({}) in {}() at {}:{} — continuing anyway (treated as non-fatal).",
        expr_s,
        func_s,
        file_s,
        line
    );
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CC_MD5(_, _, _)),
    export_c_func!(CC_SHA1(_, _, _)),
    export_c_func!(CCCrypt(_, _, _, _, _, _, _, _, _, _, _)),
    export_c_func!(__assert_rtn(_, _, _, _)),
];
