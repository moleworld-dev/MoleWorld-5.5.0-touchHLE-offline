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

/// 删除 [SAVE_FILES] 里存在的存档文件,返回实际删掉的个数。
/// 先撤销「下次启动恢复快照」标记(F2-01):否则先安排了快照恢复、再删档的话,重开时 mole_dev::startup 会在读档前
/// 把快照写回 Documents,删档被静默撤销,和「重开即为全新存档」的承诺相反。快照目录本身不动,仍可手动恢复。
/// 只用宿主 std::fs 与 guest 文件系统,不发 msg_send。
pub fn delete_local_saves(env: &mut Environment) -> usize {
    crate::mole_dev::cancel_pending_restore();
    let docs = env.fs.home_directory().join("Documents");
    let mut n = 0;
    for f in SAVE_FILES {
        let p = docs.join(f);
        if !env.fs.is_file(&p) {
            continue;
        }
        match env.fs.remove(&p) {
            Ok(()) => n += 1,
            Err(e) => {
                log!("[RESET] 删除存档 {} 失败:{:?}", f, e);
            }
        }
    }
    n
}
