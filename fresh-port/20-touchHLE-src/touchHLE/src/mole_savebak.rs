/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [2026-10-04 第八轮 R8-D1] 主村主档(Documents/userinfo.dat、map.dat)的上一代备份与启动自检。
//!
//! 原版遇到坏档会删档换新号,指望联网后服务器补档:
//! - -[GameData loadUserInfoData]@0x75704 的 CheckUserInfoData:(尾 16 字节 md5)失败 → 0x75a26/0x75a66 删两份档,
//!   弹 HACK_USERINFO_DATA_ERROR「…存档将在您联网后恢复」;
//! - -[GameData loadMapData]@0x79054 解档失败、根字典键数 <2,或等级 ≥6 时键数 ≤3(0x79248/0x7927e)→ 0x79370
//!   -[GameData resetUserGameData]@0x7de50 删 map.dat(0x7dec8)、userinfo.dat(0x7df0e),静默按新号进村。
//! 离线没有服务器,这一删就是永久清档。用户拍板:每次存档保留上一代好档,启动按原版同一判据自检,坏了只把坏的那份换回。
//!
//! - 轮换(before_replace):Fs::write_atomic 写好临时文件、rename 覆盖之前调用。目标是沙盒 Documents 下的这两份档、
//!   且「当前盘上那份」按原版判据合格,就把它复制到备份目录;当前那份不合格就不轮换(坏档永远盖不掉好备份)。
//!   [2026-10-06 第十轮 R10-B1] 更正:备份并不「早已落盘」——每次存档都先轮换备份再改名主档,两份是同一时刻
//!   写的。现在主档与备份的临时文件都先落盘再改名(fs::sync_before_rename:Apple 用 F_BARRIERFSYNC 只加写入
//!   屏障,不等磁盘缓存清空,不卡模拟线程),断电时主档不会变成 0 字节或半截,备份同理;「上一代」语义不变。
//! - 启动自检(startup_check):lib.rs 在 env.run() 之前调用,早于任何 guest 代码与读档。两份档各自判:存在但不合格、
//!   且备份合格 → 坏档改名 <名>.corrupt(已存在加时间戳)留底,备份内容原子写回;两份不一起回滚(本来就不是同一次写入);
//!   档不存在(新号/删档后)不动;备份也不合格就不动,交原版处理。在线模式不自检(存档以服务器为准)。
//! - 备份目录 touchHLE_sandbox/<bundle id>/mole_save_bak/:与 Documents 同级,不在 guest 目录树里,游戏看不到。
//!   删档(save_reset)与快照恢复(mole_dev)成功后清掉(forget_backups),免得旧备份与新状态混用。
//! 全部是宿主侧文件操作与纯 Rust 校验,不发任何 msg_send,不碰寄存器。
use crate::Environment;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use digest::Digest;
use md5::Md5;
use std::path::{Path, PathBuf};

const BAK_DIR: &str = "mole_save_bak";
const USERINFO: &str = "userinfo.dat";
const MAP: &str = "map.dat";
/// -[GameData saveUserInfoData]@0x754f8 追加的 16 字节盐(二进制 VA 0xb3acbc)。
const SALT: [u8; 16] = [
    0x01, 0xee, 0x5e, 0x1d, 0x8b, 0xf7, 0x81, 0x57, 0x67, 0x54, 0xbe, 0x70, 0x93, 0x01, 0xff, 0xe9,
];
/// 全版本通用的数据 AES-128-ECB 密钥(+[CryptUtils getEncrypKey] 前 16 字节)。
const KEY: &[u8; 16] = b"39653543fa0d66aa";

/// userinfo.dat 原版判据:-[GameData checkUserinfoMd5:]@0x753b8 把尾 16 字节换成盐再 md5,与原尾 16 字节比。
/// 长度不足 20(写一半被截断)原版会下溢,这里一律算不合格。
pub fn userinfo_ok(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    let n = bytes.len();
    let mut h = Md5::new();
    h.update(&bytes[..n - 16]);
    h.update(SALT);
    h.finalize().as_slice() == &bytes[n - 16..]
}

/// NSKeyedArchiver 根对象(字典):$top.root 指向 $objects 里的那一项。
fn archive_root(v: &plist::Value) -> Option<(&plist::Dictionary, &Vec<plist::Value>)> {
    let d = v.as_dictionary()?;
    let objects = d.get("$objects")?.as_array()?;
    let root = d.get("$top")?.as_dictionary()?.get("root")?;
    let idx = match root {
        plist::Value::Uid(u) => u.get() as usize,
        _ => return None,
    };
    Some((objects.get(idx)?.as_dictionary()?, objects))
}

/// 从合格的 userinfo.dat 解出等级(AES-128-ECB 解密、去 PKCS7、解归档取 curLevel);解不出返回 None。
fn userinfo_level(bytes: &[u8]) -> Option<i64> {
    if !userinfo_ok(bytes) {
        return None;
    }
    let enc = &bytes[..bytes.len() - 20];
    if enc.is_empty() || enc.len() % 16 != 0 {
        return None;
    }
    let cipher = aes::Aes128::new_from_slice(KEY).ok()?;
    let mut data = enc.to_vec();
    for chunk in data.chunks_exact_mut(16) {
        let mut a = [0u8; 16];
        a.copy_from_slice(chunk);
        let mut blk = a.into();
        cipher.decrypt_block(&mut blk);
        let out: [u8; 16] = blk.into();
        chunk.copy_from_slice(&out);
    }
    let pad = *data.last()? as usize;
    if (1..=16).contains(&pad)
        && data.len() >= pad
        && data[data.len() - pad..].iter().all(|&b| b as usize == pad)
    {
        data.truncate(data.len() - pad);
    }
    let v = plist::Value::from_reader(std::io::Cursor::new(&data)).ok()?;
    let (root, objects) = archive_root(&v)?;
    match root.get("curLevel")? {
        plist::Value::Integer(i) => i.as_signed(),
        plist::Value::Uid(u) => objects.get(u.get() as usize)?.as_signed_integer(),
        _ => None,
    }
}

/// map.dat 根字典(NSDictionary 归档为 NS.keys/NS.objects)的键数;解不开返回 None。
fn map_root_count(bytes: &[u8]) -> Option<usize> {
    let v = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let (root, _) = archive_root(&v)?;
    Some(root.get("NS.keys")?.as_array()?.len())
}

/// map.dat 原版判据(-[GameData loadMapData]@0x79054):解得开,根字典键数 ≥2,等级 ≥6 时还要 >3。
/// level 取同目录 userinfo.dat 解出的等级(原版用的是已读进内存的等级,读档顺序正是先 userinfo 后 map);解不出按 0。
pub fn map_ok(bytes: &[u8], level: Option<i64>) -> bool {
    let Some(count) = map_root_count(bytes) else {
        return false;
    };
    if count < 2 {
        return false;
    }
    !(count <= 3 && level.unwrap_or(0) >= 6)
}

fn file_ok(name: &str, bytes: &[u8], docs: &Path) -> bool {
    if name == USERINFO {
        userinfo_ok(bytes)
    } else {
        let level = std::fs::read(docs.join(USERINFO))
            .ok()
            .and_then(|b| userinfo_level(&b));
        map_ok(bytes, level)
    }
}

/// Fs::write_atomic 在 rename 覆盖目标之前调用(target 是宿主路径)。只管沙盒 Documents 下的两份主档。
pub fn before_replace(target: &Path) {
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    if name != USERINFO && name != MAP {
        return;
    }
    let Some(docs) = target.parent() else {
        return;
    };
    if docs.file_name().and_then(|n| n.to_str()) != Some("Documents") {
        return;
    }
    let Ok(cur) = std::fs::read(target) else {
        return; // 目标还不存在(新号第一次存):没有上一代可备份
    };
    if !file_ok(name, &cur, docs) {
        log!(
            "[SAVEBAK] 盘上当前的 {} 不合格,本次不轮换(备份保持上一代好档)",
            name
        );
        return;
    }
    let Some(sandbox) = docs.parent() else {
        return;
    };
    let dir = sandbox.join(BAK_DIR);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log!("[SAVEBAK] 建备份目录 {:?} 失败:{}", dir, e);
        return;
    }
    let tmp = dir.join(format!(".{}.tmp", name));
    let result = crate::fs::write_tmp_durable(&tmp, &cur).and_then(|_| std::fs::rename(&tmp, dir.join(name)));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        log!("[SAVEBAK] 备份 {} 失败:{}(不影响本次存档)", name, e);
    } else {
        log_dbg!("[SAVEBAK] 已把上一代 {} 存为备份", name);
    }
}

/// [2026-10-04 第八轮收尾] Fs 删除文件成功后调用(removed 是宿主路径):游戏自己删掉主村主档时,清掉上一代备份。
/// 原版删主档只有三处,删完之后的新档都已不是备份里那一局:
/// - -[GameData loadUserInfoData] 校验失败 0x75a26/0x75a66 删两份档(之前 0x759fe 把登录身份 setUserId:0),弹框退出,
///   下次启动按新号;
/// - -[GameData resetUserGameData]@0x7de50(0x7dec8 map.dat、0x7df0e userinfo.dat),调用者是读地图失败 0x7936a、
///   设置里「重新开始」-[OptionLayer onRestartYesRestart] 0x14f68c、登录账号与本地档不同
///   -[MainMenuScene onLoginMainMenuCommandReceived:] 0xb6bb8。
/// 留着旧备份的话,新档一旦写坏,启动自检会把旧档换回来:「重新开始」被撤销,或新旧两份拼成不配对的一对。
/// 两份主档是一局的两半,任一被删就整个清掉。原版平时存档不走删除(移植层 write_atomic 用 rename 覆盖),不会误清。
/// 只做宿主文件操作,不发消息。
pub fn on_main_save_removed(removed: &Path) {
    let Some(name) = removed.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    if name != USERINFO && name != MAP {
        return;
    }
    let Some(docs) = removed.parent() else {
        return;
    };
    if docs.file_name().and_then(|n| n.to_str()) != Some("Documents") {
        return;
    }
    let Some(sandbox) = docs.parent() else {
        return;
    };
    let dir = sandbox.join(BAK_DIR);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            log!(
                "[SAVEBAK] 游戏删除了主档 {}(坏档删档 / 重新开始 / 换号),已清掉上一代备份,免得之后把旧档换回新档",
                name
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            log!("[SAVEBAK] 清备份目录 {:?} 失败:{}", dir, e);
        }
    }
}

/// [2026-10-05 v0.0.8 P0] 主村地图档 Documents/map.dat 在盘上是否存在(宿主侧 stat,不发消息)。
pub fn main_map_exists(env: &Environment) -> bool {
    sandbox_dir(env).join("Documents").join(MAP).is_file()
}

/// [2026-10-05 v0.0.8 P0] 主村用户档 Documents/userinfo.dat 在盘上是否存在(宿主侧 stat,不发消息)。
pub fn main_userinfo_exists(env: &Environment) -> bool {
    sandbox_dir(env).join("Documents").join(USERINFO).is_file()
}

fn sandbox_dir(env: &Environment) -> PathBuf {
    // 联机模式是单独的 `<bundle id>-online` 沙盒(见 paths::sandbox_dir)。
    crate::paths::sandbox_dir(env.bundle.bundle_identifier())
}

/// 启动自检:lib.rs 在 env.run() 之前调用(早于任何 guest 代码与读档)。在线模式不做。
pub fn startup_check(env: &mut Environment) {
    if env.options.network_access {
        return;
    }
    let sandbox = sandbox_dir(env);
    let docs = sandbox.join("Documents");
    let dir = sandbox.join(BAK_DIR);
    // 先 userinfo 后 map:map 的判据要用 userinfo 解出的等级(恢复后的那份)。
    for name in [USERINFO, MAP] {
        check_one(&docs, &dir, name);
    }
}

fn check_one(docs: &Path, dir: &Path, name: &str) {
    let cur_path = docs.join(name);
    let Ok(cur) = std::fs::read(&cur_path) else {
        return; // 不存在:新号或删档后,交原版
    };
    if file_ok(name, &cur, docs) {
        return;
    }
    let bak_path = dir.join(name);
    let Ok(bak) = std::fs::read(&bak_path) else {
        log!(
            "[SAVEBAK] ⚠️ {} 按原版判据不合格,但没有上一代备份,交原版处理(原版会删档换新号)",
            name
        );
        return;
    };
    // 备份按同一判据核一遍;map 的等级取「当前 docs 里的 userinfo」(此时 userinfo 若坏已先被换回)。
    if !file_ok(name, &bak, docs) {
        log!(
            "[SAVEBAK] ⚠️ {} 不合格,上一代备份也不合格,不动,交原版处理",
            name
        );
        return;
    }
    let mut corrupt = docs.join(format!("{}.corrupt", name));
    if corrupt.exists() {
        corrupt = docs.join(format!(
            "{}.corrupt-{}",
            name,
            crate::libc::time::host_now_unix_secs()
        ));
    }
    if let Err(e) = std::fs::rename(&cur_path, &corrupt) {
        log!(
            "[SAVEBAK] ⚠️ {} 不合格,但改名留底失败({}),不动,交原版处理",
            name,
            e
        );
        return;
    }
    let tmp = docs.join(format!(".{}.savebak-tmp", name));
    let result = crate::fs::write_tmp_durable(&tmp, &bak).and_then(|_| std::fs::rename(&tmp, &cur_path));
    match result {
        Ok(()) => {
            log!(
                "[SAVEBAK] {} 按原版判据不合格(原版会删档换新号),已改名留底为 {:?},并用上一代备份恢复({} 字节)",
                name,
                corrupt.file_name().unwrap_or_default(),
                bak.len()
            );
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // 写回失败:把坏档挪回去,保持原状交原版处理(总比两份都没有强)。
            let _ = std::fs::rename(&corrupt, &cur_path);
            log!(
                "[SAVEBAK] ⚠️ 用备份恢复 {} 失败({}),已把原文件放回,交原版处理",
                name,
                e
            );
        }
    }
}

/// 删档、快照恢复成功后调用:清掉备份目录,免得旧备份与新状态混用。
pub fn forget_backups(env: &Environment) {
    let dir = sandbox_dir(env).join(BAK_DIR);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            log!("[SAVEBAK] 已清掉主档备份目录 {:?}", dir);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            log!("[SAVEBAK] 清备份目录 {:?} 失败:{}", dir, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_md5_rule() {
        // 造一份「密文 16 字节 + 00 00 01 00 + md5」的档,校验应通过;改一字节应失败。
        let mut f = vec![7u8; 16];
        f.extend_from_slice(&[0, 0, 1, 0]);
        let mut h = Md5::new();
        h.update(&f);
        h.update(SALT);
        f.extend_from_slice(h.finalize().as_slice());
        assert!(userinfo_ok(&f));
        f[3] ^= 1;
        assert!(!userinfo_ok(&f));
        assert!(!userinfo_ok(&[0u8; 10]));
    }

    #[test]
    fn map_rule_unparsable() {
        assert!(!map_ok(b"not a plist", Some(39)));
        assert!(!map_ok(&[], None));
    }
}
