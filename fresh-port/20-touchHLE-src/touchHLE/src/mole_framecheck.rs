/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [2026-10-03] 诊断:「帧栈上不发宿主消息」运行时检测器(环境变量 MOLE_FRAMECHECK=1 打开,默认关闭)。
//!
//! 规则来历:帧栈(CCDirector mainLoop → drawScene → CCScheduler tick)里,移植层的钩子若就地发宿主 msg_send,
//! 曾饿死运行循环、触发调度器重入活锁。第五~七波都是读代码逐个找违规点;这里改成跑测试时自动记下证据:
//! - 帧:主线程上 cocos2d 驱动帧的定时器回调期间——CADisplayLink(touchHLE 用 NSTimer 实现,选择子
//!   `_touchHLE_displayLinkTimerDidFire:`)或 CCDirector 的 NSTimer 版 `mainLoop`。其它普通 NSTimer 回调不算帧。
//! - 钩子:objc/messages.rs 里 mole_cheats::intercept(及它转给的 mole_activity / mole_items / mole_dev 等)执行期间。
//! - 违规:帧中、钩子执行期间,又有一条由宿主发出的消息进入派发(message_type_info 非空)。按
//!   (钩子类.选择子, 调用方 LR, 目标类, 目标选择子) 去重,每种只记一行 `[FRAMECHECK]` 日志。
//! 只读线程局部状态,不改任何寄存器与游戏状态;关闭时每条消息只多一次原子读。
//!
//! 2026-10-03 首次全量扫描(全量回归六组 + 第五轮十组 + r8)共 5 个调用点:进岛 state1 布局注入已移到运行循环
//! (mole_cheats::island_inject_poll);其余 4 个审查后保留,列在 REVIEWED 里(第十一轮又登记了 VIP 侧档首次读取注入的 2 个调用点),日志标「已审查例外」、不再展开明细。
//! 新冒出来的调用点照常详细记录。
use crate::objc::{id, SEL};
use crate::Environment;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};

/// 0 = 未读环境变量,1 = 关,2 = 开。
static STATE: AtomicU8 = AtomicU8::new(0);

pub fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = matches!(
                std::env::var("MOLE_FRAMECHECK").as_deref(),
                Ok("1") | Ok("on") | Ok("true")
            );
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            if on {
                log!("[FRAMECHECK] 已开启:帧中(显示链路/mainLoop 定时器回调)钩子里发宿主消息会记一行日志(按调用点去重)");
            }
            on
        }
    }
}

#[derive(Default)]
struct Ctx {
    /// 帧定时器嵌套深度(> 0 = 帧中)。
    frame_depth: u32,
    /// 当前帧定时器的选择子(只用于日志)。
    frame_sel: String,
    /// 正在执行的钩子栈:(类, 选择子, 调用方 LR)。
    hooks: Vec<(String, String, u32)>,
    /// 已报告过的钩子调用点 (类, 选择子, LR)。
    sites: HashSet<(String, String, u32)>,
    /// 已报告过的「调用点 → 游戏方法」(会执行游戏代码,风险最高)。
    guest_targets: HashSet<(String, String, u32, String, String)>,
}

thread_local! {
    static CTX: RefCell<Ctx> = RefCell::new(Ctx::default());
}

/// 已审查、有意保留的调用点:(钩子类, 选择子, 调用方 LR, 理由)。
const REVIEWED: [(&str, &str, u32, &str); 6] = [
    (
        "GameData",
        "loadUserInfoData",
        0x12e425,
        "读档前兜底(启动时 -[LoadingLayer updateLoadingProcess:] 调):必须赶在原版读档之前修偏好/隔离截断档,原版读档本身就在同一栈上读文件",
    ),
    (
        "GameData",
        "loadUserInfoData",
        0x7ca3d,
        "读档前兜底(-[GameData loadFromLocal] 调):同上",
    ),
    (
        "LoadingHoliday",
        "updateLoading:",
        0x2de89b,
        "进岛 state2 判 mapData 前只读一次条数(每次进岛只判一次);条数为 0 时的补注入必须当场完成,否则原版带空地图继续",
    ),
    (
        "ObjectManager",
        "checkMapExtendError",
        0x1a319,
        "整体接管原版扩地自检,原版本身就在这个栈上读桥位、修 mapExtend",
    ),
    // [2026-10-07 第十一轮 R11-E-1] VIP 侧档首次读取注入(mole_items::vip_hook 的 getter 臂):读档时地里有枯萎作物、
    // 或逛完好友村回家时,第一次读 VIP 等级落在分步加载地图的调度器回调里。只写 VIP 三值、不碰调度器、寄存器已恢复;
    // 不能改成「只置标志、延后注入」:读档那一拍读到 0,VIP4 的作物会被判枯萎,偏离原版。
    (
        "UserVIPInfoData",
        "vipLevelWithNewType",
        0x49341,
        "VIP 侧档首次读取注入:-[Farm createCropForMapData:] 0x4933c 读档时判枯萎(VIP≥4 不枯萎),必须当场注入",
    ),
    (
        "UserVIPInfoData",
        "vipLevelWithNewType",
        0x4a117,
        "VIP 侧档首次读取注入:-[Farm cropWitherHandler:] 0x4a112 运行中到枯萎点判 VIP,必须当场注入",
    ),
];

fn reviewed_reason(class: &str, sel: &str, lr: u32) -> Option<&'static str> {
    REVIEWED
        .iter()
        .find(|&&(c, s, l, _)| c == class && s == sel && l == lr)
        .map(|&(_, _, _, why)| why)
}

/// 是否把这个定时器选择子当作帧。
fn is_frame_timer(sel: &str) -> bool {
    sel == "_touchHLE_displayLinkTimerDidFire:" || sel == "mainLoop" || sel == "mainLoop:"
}

/// ns_timer::handle_timer 发定时器消息之前调用;返回是否进入了帧(调用方据此在回调后调 frame_exit)。
pub fn frame_enter(env: &Environment, selector: SEL) -> bool {
    if !enabled() || env.current_thread != 0 {
        return false;
    }
    let sel = selector.as_str(&env.mem);
    if !is_frame_timer(sel) {
        return false;
    }
    CTX.with(|c| {
        let mut c = c.borrow_mut();
        if c.frame_depth == 0 {
            c.frame_sel = sel.to_string();
        }
        c.frame_depth += 1;
    });
    true
}

pub fn frame_exit() {
    CTX.with(|c| {
        let mut c = c.borrow_mut();
        c.frame_depth = c.frame_depth.saturating_sub(1);
    });
}

/// objc/messages.rs 调 mole_cheats::intercept 之前调用;返回是否压了栈(调用方据此在之后调 hook_exit)。
pub fn hook_enter(env: &Environment, class: &str, sel: &str) -> bool {
    if !enabled() {
        return false;
    }
    let lr = env.cpu.regs()[14];
    CTX.with(|c| {
        c.borrow_mut()
            .hooks
            .push((class.to_string(), sel.to_string(), lr))
    });
    true
}

pub fn hook_exit() {
    CTX.with(|c| {
        c.borrow_mut().hooks.pop();
    });
}

/// objc_msgSend_inner 里对每条宿主发出的消息调用(已解析出接收者类与类名)。帧中且有钩子在执行时记日志:
/// 每个钩子调用点首次违规记一行;此后该调用点每调到一个新的游戏(guest)实现方法再记一行(这些会在帧栈上执行游戏代码)。
pub fn host_message(
    env: &Environment,
    receiver: id,
    class: crate::objc::Class,
    class_name: &str,
    selector: SEL,
) {
    if !enabled() {
        return;
    }
    CTX.with(|c| {
        let mut c = c.borrow_mut();
        if c.frame_depth == 0 {
            return;
        }
        let Some((hc, hs, lr)) = c.hooks.last().cloned() else {
            return;
        };
        let reviewed = reviewed_reason(&hc, &hs, lr);
        let first = c.sites.insert((hc.clone(), hs.clone(), lr));
        if let Some(why) = reviewed {
            if first {
                log!(
                    "[FRAMECHECK] 已审查例外:帧中钩子 {} {}(调用方 LR {:#x})发宿主消息 —— {}",
                    hc,
                    hs,
                    lr,
                    why
                );
            }
            return;
        }
        let sel = selector.as_str(&env.mem).to_string();
        let guest = env.objc.class_method_is_guest(class, selector) == Some(true);
        let new_guest = guest
            && c
                .guest_targets
                .insert((hc.clone(), hs.clone(), lr, class_name.to_string(), sel.clone()));
        if first {
            log!(
                "[FRAMECHECK] 帧中(定时器 {})钩子 {} {}(调用方 LR {:#x})发了宿主消息,首条 [{} {}]({})recv={:?} 钩子栈深 {}",
                c.frame_sel,
                hc,
                hs,
                lr,
                class_name,
                sel,
                if guest { "游戏方法" } else { "宿主方法" },
                receiver,
                c.hooks.len()
            );
        } else if new_guest {
            log!(
                "[FRAMECHECK]   └ 同一调用点 {} {}(LR {:#x})又调到游戏方法 [{} {}]",
                hc,
                hs,
                lr,
                class_name,
                sel
            );
        }
    });
}
