/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [MoleWorld iOS · 性能] 无依赖的 Fx 哈希(rustc 内部同款)。
//!
//! std 的 `HashMap` 默认用 SipHash-1-3(抗 HashDoS,但每次哈希几十纳秒)。本项目的 objc 对象表、
//! 方法表、方法缓存的键都是 32 位指针/整数,且不接收不可信输入——村里每秒 94 万条 objc 消息,
//! 每条沿超类链做多次查表,哈希本身就是可观开销。Fx 对一个 u32 只需一次乘法。

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut b = bytes;
        while b.len() >= 8 {
            self.add(u64::from_le_bytes(b[..8].try_into().unwrap()));
            b = &b[8..];
        }
        if b.len() >= 4 {
            self.add(u32::from_le_bytes(b[..4].try_into().unwrap()) as u64);
            b = &b[4..];
        }
        for &x in b {
            self.add(x as u64);
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) { self.add(i as u64) }
    #[inline]
    fn write_u16(&mut self, i: u16) { self.add(i as u64) }
    #[inline]
    fn write_u32(&mut self, i: u32) { self.add(i as u64) }
    #[inline]
    fn write_u64(&mut self, i: u64) { self.add(i) }
    #[inline]
    fn write_usize(&mut self, i: usize) { self.add(i as u64) }
    #[inline]
    fn finish(&self) -> u64 { self.hash }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;
pub type FxHashSet<K> = HashSet<K, FxBuildHasher>;
