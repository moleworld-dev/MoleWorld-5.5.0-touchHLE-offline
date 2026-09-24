/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! MoleWorld 离线移植:删本地存档的共享实现(唯一一份删档清单)。
//!
//! [2026-09-16] 原来这里是标题页 LogoLayer 四个按钮(客服/换账号/换玩家/版本)的 osascript 确认 + 删档入口。
//! 5.5.0 的 LogoLayer 根本没有那四个选择子(methods.txt / selref 零命中),那条钩子永远不触发,osascript 也只在
//! macOS 存在,所以钩子和确认框一起删掉。删档本身改成共享函数:作弊菜单的「删本地存档并退出」和「整库重置」
//! 都调 [delete_local_saves],清单只维护这一份,不会再出现两处清单漂移(以前一处 8 个、一处 9 个)。
//! 二次确认、退出进程由调用方负责(菜单删完立即 exit(0),原因见 mole_menu 的 ResetLocalSave)。
//!
//! [2026-09-16] X4-01 删档改成「全部删掉才算成功」。F2-02 之后宿主删除失败不再 panic 而是返回 Err
//! (macOS 上 chflags uchg、属主不对,Windows 上文件被杀毒/同步软件占着句柄),原来这里只打日志、接着删其余文件,
//! 菜单照样 exit(0):userinfo.dat 删不掉时重开得到「旧等级/摩尔豆/贝壳 + 全新主村地图、岛档、vip.dat、活动档」的混合档,
//! 只有 mole_activity.dat 删不掉时新号继承旧号当天的签到/兑换状态。现在先把目标文件读进内存再逐个删,
//! 任一删不掉就把本轮已删的原样写回并返回 Err,菜单据此不退出、只提示。

use crate::fs::GuestPathBuf;
use crate::Environment;

/// 删档目标:沙盒 Documents 下的全部玩家存档。与 mole_dev 快照清单 SAVE_FILES 保持同一组文件。
/// - userinfo.dat / map.dat:主村存档;
/// - island_*.dat:黄金岛 4 份离线岛档,不删的话重置后岛上进度还留着;
/// - vip.dat(mole_items:VIP 三值/登录日与连续天数/累计在线毫秒)与 mole_activity.dat(mole_activity:签到/脚印兑换/
///   海底寻宝等,经 -[GameData pathForDataFile:]@0x75374 落在 Documents):不绑定用户 ID,不删的话新档会继承旧号的
///   VIP 等级、连续登录天数和当天已签到状态。
///
/// 刻意不删偏好 plist:菜单删档后会 synchronize 一次 NSUserDefaults(保住音量等偏好),删了也会被写回;
/// 主档已删时 -[GameData loadUserInfoData]@0x75704 读不到文件就在 0x7576e 直接返回,不校验 isEncrypt,
/// 不会弹 HACK_USERINFO_DATA_ERROR。
const SAVE_FILES: [&str; 8] = [
    "userinfo.dat",
    "map.dat",
    "island_map.dat",
    "island_userinfo.dat",
    "island_ships.dat",
    "island_fragments.dat",
    "vip.dat",
    "mole_activity.dat",
];

/// [2026-09-16] 删档时一并删掉 mole_activity.dat 的坏档备份。mole_activity 的 backup_corrupt_state 把校验失败的
/// mole_activity.dat 原样备份成 `mole_activity.dat.corrupt`,已存在时改用 `mole_activity.dat.corrupt-<unix秒>`,
/// 都在 Documents。里面是旧号的签到/兑换/海底寻宝状态,「删全部本地存档」之后不该还留在沙盒里。
/// 按 guest 目录树枚举:启动建树时已有的文件和本会话 write_atomic 新建的备份都在树里;启动之后才由外部放进宿主目录的文件
/// guest 看不到,不删。快照不收这些备份,所以不进 [SAVE_FILES](那份要与 mole_dev 的快照清单保持一致)。
/// vip.dat 与岛档的 .corrupt 隔离件目前仍保留,没有列进来。
const ACTIVITY_CORRUPT_BACKUP: &str = "mole_activity.dat.corrupt";

/// [2026-09-16] X4-01 删档没有完成。拿到它时存档不能当成已重置:调用方不能退出进程,也不能禁写 sidecar。
pub struct ResetFailure {
    /// 导致中止的文件与原因(如「userinfo.dat 删不掉」),给 toast 列出来;详细错误在日志 [RESET] 里。
    pub problems: Vec<String>,
    /// 本轮已经删掉、写回又失败的文件。非空时沙盒里的存档已不完整。
    pub not_restored: Vec<String>,
}

/// 已删(或待删)的文件:(文件名, guest 路径, 删除前读进内存的内容)。内容为 None 表示宿主上本来就读不到(见第 1 步)。
type Staged = (String, GuestPathBuf, Option<Vec<u8>>);

/// 删除 [SAVE_FILES] 与 mole_activity.dat 坏档备份中存在的文件,全部删掉才返回 Ok(删掉的个数)。
///
/// [2026-09-16] X4-01 分三步:
/// 1) 把存在的目标文件读进内存。宿主上已经读不到元数据的(运行中被外部删掉、guest 节点过期)不能 read:
///    Fs::open 经 handle_open_err 会直接 panic。这类记为「无内容」排到最后删,Fs::remove 对宿主 NotFound 按已删处理。
///    读取失败时还一个文件都没删,直接返回 Err。
/// 2) 逐个 env.fs.remove。任一失败就把本轮已删的文件用 env.fs.write_atomic 原样写回(宿主写失败时返回 Err、不 panic),
///    返回 Err。写回的内容逐字节相同,只是文件修改时间变成现在。
/// 3) 全部删掉之后才撤销「下次启动恢复快照」标记(F2-01:否则重开时 mole_dev::startup 在读档前把快照写回,删档被静默撤销)。
///    以前在删任何文件之前就无条件撤销,删档失败时玩家安排好的恢复也被悄悄取消。标记删不掉同样写回存档并返回 Err。
///    快照目录本身不动,仍可手动恢复。
///
/// 只用宿主 std::fs 与 guest 文件系统,不发 msg_send。
pub fn delete_local_saves(env: &mut Environment) -> Result<usize, ResetFailure> {
    let docs = env.fs.home_directory().join("Documents");
    let mut names: Vec<String> = SAVE_FILES.iter().map(|f| f.to_string()).collect();
    names.extend(activity_corrupt_backups(env, &docs));

    // 第 1 步:读进内存。
    let mut with_bytes: Vec<Staged> = Vec::new();
    let mut host_gone: Vec<Staged> = Vec::new();
    for name in names {
        let path = docs.join(&name);
        if !env.fs.is_file(&path) {
            continue;
        }
        // Fs::size 只读宿主元数据,失败返回 Err(()),不会 panic。
        if env.fs.size(&path).is_err() {
            host_gone.push((name, path, None));
            continue;
        }
        match env.fs.read(&path) {
            Ok(bytes) => with_bytes.push((name, path, Some(bytes))),
            Err(()) => {
                log!("[RESET] 读取存档 {} 失败,删档中止(还没删任何文件)", name);
                return Err(ResetFailure {
                    problems: vec![format!("{} 读不出来", name)],
                    not_restored: Vec::new(),
                });
            }
        }
    }

    // 第 2 步:逐个删,读进内存的在前,宿主上本来就读不到的在后。
    let mut removed: Vec<Staged> = Vec::new();
    for (name, path, bytes) in with_bytes.into_iter().chain(host_gone) {
        match env.fs.remove(&path) {
            Ok(()) => removed.push((name, path, bytes)),
            Err(e) => {
                log!(
                    "[RESET] 删除存档 {} 失败:{:?};删档中止,把本轮已删的 {} 个文件原样写回",
                    name,
                    e,
                    removed.len()
                );
                let not_restored = write_back(env, removed);
                return Err(ResetFailure {
                    problems: vec![format!("{} 删不掉", name)],
                    not_restored,
                });
            }
        }
    }

    // 第 3 步:存档全部删掉之后才撤销待恢复快照。
    if let Err(marker) = crate::mole_dev::cancel_pending_restore() {
        log!(
            "[RESET] 撤销待恢复快照标记 {} 失败;删档中止,把本轮已删的 {} 个文件原样写回",
            marker,
            removed.len()
        );
        let not_restored = write_back(env, removed);
        return Err(ResetFailure {
            problems: vec!["快照恢复标记 RESTORE_PENDING 删不掉".to_string()],
            not_restored,
        });
    }
    Ok(removed.len())
}

/// [2026-09-16] X4-01 把本轮已删的文件按删除的逆序原样写回,返回写回失败的文件名。
/// 目标节点已被 Fs::remove 摘掉,write_atomic 会在 Documents 下补建节点;宿主写失败时返回 Err、不 panic。
fn write_back(env: &mut Environment, removed: Vec<Staged>) -> Vec<String> {
    let mut failed = Vec::new();
    for (name, path, bytes) in removed.into_iter().rev() {
        // 删除前宿主上就读不到的文件(运行中被外部删掉)本来就不在,不用写回。
        let Some(bytes) = bytes else {
            continue;
        };
        match env.fs.write_atomic(&path, &bytes) {
            Ok(()) => {
                log!("[RESET] 已原样写回 {}({} 字节)", name, bytes.len());
            }
            Err(e) => {
                log!(
                    "[RESET] ⚠️ 写回 {} 失败:{:?}(这个文件已丢失,沙盒里的存档不完整)",
                    name,
                    e
                );
                failed.push(name);
            }
        }
    }
    failed
}

/// [2026-09-16] Documents 里 mole_activity.dat 的坏档备份文件名,按名字排序。
fn activity_corrupt_backups(env: &Environment, docs: &GuestPathBuf) -> Vec<String> {
    let Ok(entries) = env.fs.enumerate(docs) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter(|n| is_activity_corrupt_backup(n))
        .map(|n| n.to_string())
        .collect();
    names.sort();
    names
}

/// 只认 backup_corrupt_state 产生的两种名字:`mole_activity.dat.corrupt` 与 `mole_activity.dat.corrupt-<纯数字>`,
/// 不误删玩家自己放进 Documents 的其它文件。
fn is_activity_corrupt_backup(name: &str) -> bool {
    match name.strip_prefix(ACTIVITY_CORRUPT_BACKUP) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix('-')
            .is_some_and(|secs| !secs.is_empty() && secs.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_activity_corrupt_backup;

    #[test]
    fn activity_corrupt_backup_names() {
        assert!(is_activity_corrupt_backup("mole_activity.dat.corrupt"));
        assert!(is_activity_corrupt_backup(
            "mole_activity.dat.corrupt-1789000000"
        ));
        assert!(!is_activity_corrupt_backup("mole_activity.dat"));
        assert!(!is_activity_corrupt_backup("mole_activity.dat.corrupt-"));
        assert!(!is_activity_corrupt_backup("mole_activity.dat.corrupt-12a"));
        assert!(!is_activity_corrupt_backup("mole_activity.dat.corrupt.bak"));
        assert!(!is_activity_corrupt_backup(
            ".mole_activity.dat.corrupt.touchhle-tmp"
        ));
        assert!(!is_activity_corrupt_backup("vip.dat.corrupt"));
    }
}
