/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! 米米号 = QQ 号(联网私服)的客户端适配。原版淘米米米号最多 9 位,QQ 号有 10 位的(10 亿以上),
//! 其中大于 2147483647 的在 32 位有符号整数里是负数。这里改三处原版行为(离线也无害,一律打上):
//!
//! 1. 账号界面(切换账号 / 修改密码 / 找回密码)的米米号输入:输入时「长度 < 9 才能继续输」→ < 10;
//!    提交时「5~9 位」→ 5~10 位。提示文案「5~9位米米号」不改(资源里的提示只是说明,不影响校验)。
//! 2. 米米号显示改无符号(补丁表后两项 + 下面两处):好友资料栏「ID: %d」→「ID: %u」:否则 3000000002 显示成 -1294967294。这处直接改在游戏可执行文件
//!    (01-cracked/.../MoleWorld 文件偏移 0x92155f):touchHLE 加载时就把常量字符串读成对象,运行时改内存不生效。
//!    好友面板的「米米号：%d」在 Localizable.strings 资源里同步改成 %u。
//! 3. (在 ns_string.rs)`intValue` 超出 int32 但在 uint32 范围内时按位保留,不再得 0。
//!
//! 每处写入前先核对原版字节,对不上(别的版本二进制)就跳过并记日志,绝不盲写。

use crate::mem::{MutPtr, Ptr};
use crate::Environment;

/// (地址, 原版字节, 新字节, 说明)
const UID_PATCHES: &[(u32, &[u8], &[u8], &str)] = &[
    (0x4e6c2c, &[0x09, 0x28], &[0x0a, 0x28], "切换账号:输入上限 9→10 位"),
    (0x4e62a0, &[0x0a, 0x28], &[0x0b, 0x28], "切换账号:提交校验 5~9→5~10 位"),
    (0x4e2030, &[0x09, 0x28], &[0x0a, 0x28], "修改密码:输入上限 9→10 位"),
    (0x4ec76a, &[0x09, 0x28], &[0x0a, 0x28], "找回密码:输入上限 9→10 位"),
    (0x4ec0fc, &[0x0a, 0x28], &[0x0b, 0x28], "找回密码:提交校验 5~9→5~10 位"),
    // 自己的米米号显示:原版 [NSString stringWithFormat:@"%ld", userId],32 位下 long 有符号 → 改为加载现成的 @"%lu"
    // 常量(0xb0da88,原 @"%ld" 在 0xb0dad8,movt 不变只改 movw)。只改界面显示处;拼文件名/字典键的 %ld 前后一致,不动。
    (0xfca9e, &[0x41, 0xf2, 0x2e, 0x02], &[0x40, 0xf6, 0xde, 0x72], "换头像面板:米米号 %ld→%lu"),
    (0x19ce42, &[0x40, 0xf6, 0x86, 0x42], &[0x40, 0xf6, 0x36, 0x42], "好友列表(UIKit):米米号 %ld→%lu"),
];

/// 打补丁(幂等:已是新字节就跳过)。由 mole_cheats::apply_crack_patches 调用。
pub fn apply(env: &mut Environment) {
    let mut done = 0;
    for &(vaddr, vanilla, patched, what) in UID_PATCHES {
        let n = vanilla.len() as u32;
        let ptr: MutPtr<u8> = Ptr::from_bits(vaddr);
        let cur = env.mem.bytes_at(ptr.cast_const(), n).to_vec();
        if cur == patched {
            continue;
        }
        if cur != vanilla {
            log!("[MOLEUID] 跳过 {:#x}({}):字节不符 {:02x?}", vaddr, what, cur);
            continue;
        }
        env.mem.bytes_at_mut(ptr, n).copy_from_slice(patched);
        env.cpu.invalidate_cache_range(vaddr, n);
        done += 1;
    }
    if done > 0 {
        log!("[MOLEUID] 10 位米米号适配:已打 {} 处补丁", done);
    }
}
