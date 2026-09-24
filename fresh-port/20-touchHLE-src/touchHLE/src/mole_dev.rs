/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [扫描修 2026-09-15] 开发者工具:数值寄存器、时间/任务/剧情/天气/倍速/FPS/地图格线/时间旅行/
//! 存档快照/建设商店/相机回中/选择子跟踪。菜单(mole_menu.rs)只调这里的函数,
//! 钩子由 mole_cheats::intercept 统一调度。
//!
//! 调用上下文:除 `wants` / `intercept` / `trace_*` / `startup` 外,这里的函数只在菜单点击(UIKit 事件分派)
//! 里调用,不在 drawScene/mainLoop 帧栈上,可以安全地发宿主 msg_send。
//! 地址与 ivar 偏移都用 re.py 在 5.5.0 香草二进制上核实过;ivar 偏移优先读运行时 `_OBJC_IVAR` 槽里的值
//! (非脆弱 ivar 修正后会写回槽里),槽值不合理时才退回静态偏移。

use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::objc::{id, msg_send, nil, release, retain, SEL};
use crate::Environment;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::OnceLock;

const O: Ordering = Ordering::Relaxed;

/// 开发工具统一返回:Ok(给 toast 的成功文案) / Err(失败原因)。
pub type DevResult = Result<String, String>;

/// 任务链族。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuestFamily {
    Main,
    Time,
    Vip,
    Island,
}

// ───────────────────────── 已核实的地址与偏移 ─────────────────────────

/// TestLayer.time_(类型 i,单位分钟):ivar 槽 0xb04ae0,静态偏移 +276。
/// -[TestLayer updateTime]@0x146204 在 0x14624a 读它,0x1462ac 乘 60 换算成秒后对全部对象 setAccTime:。
const TESTLAYER_TIME_SLOT: u32 = 0xb04ae0;
const TESTLAYER_TIME_OFF: u32 = 276;
/// Story.storyLayer(@"StoryLayer"):槽 0xb04744,+236。
const STORY_LAYER_SLOT: u32 = 0xb04744;
const STORY_LAYER_OFF: u32 = 236;
/// [复核修 2026-09-15] R6-1 StoryLayer.isStandby(c):槽 0xb04768,+251,没有 getter,直接读 ivar。
/// -[StoryLayer beginStory]@0x114624 遇到 isLocked(顶层面板挡着)时只置 isStandby 就返回、不置 isOpen;
/// -[StoryLayer unlock]@0x115690 再看 isStandby 补发 beginStory;-[StoryLayer reset]@0x1158d4 清零。
const STORYLAYER_STANDBY_SLOT: u32 = 0xb04768;
const STORYLAYER_STANDBY_OFF: u32 = 251;
/// CCScheduler.timeScale_(f):槽 0xb0709c,+4。-[CCScheduler setTimeScale:]@0x2e073c 就是 `str r2,[r0,#4]`。
const SCHED_TIMESCALE_SLOT: u32 = 0xb0709c;
const SCHED_TIMESCALE_OFF: u32 = 4;
/// CCDirector.displayFPS_(c):槽 0xb06d0c,+24。-[CCDirectorIOS drawScene] 在 0x2f3fae 读它决定是否 showFPS。
const DIRECTOR_DISPLAYFPS_SLOT: u32 = 0xb06d0c;
const DIRECTOR_DISPLAYFPS_OFF: u32 = 24;
/// -[Map draw]@0x28b38 原字节 `70 47 00 bf`(bx lr; nop),紧接着 0x28b3c 就是从未被调用的 -[Map debugDraw]
/// 的 push 序言(xref/selref 均为 0)。把 bx lr 改成 nop 即从 draw 落进 debugDraw,r0=self、lr=调用者都正确。
/// 包内二进制文件偏移 0x24b38 实测字节 70 47 00 bf。
const MAP_DRAW_ADDR: u32 = 0x28b38;
const MAP_DRAW_VANILLA: [u8; 2] = [0x70, 0x47];
const MAP_DRAW_PATCHED: [u8; 2] = [0x00, 0xbf];
const MAP_DRAW_TAIL: [u8; 2] = [0x00, 0xbf];
/// -[Story nextStep] 在 0x1142ee 用 32 位 blx 发 `[userInfoData setNextStorySectionId:curSection+1]`,
/// 调用方返回地址 = 0x1142f2(比较前清 Thumb 位)。这是「一段剧情播完」的唯一推进点,紧接着 [self activate]。
const STORY_NEXTSTEP_SET_NEXT_LR: u32 = 0x1142f2;

/// 时间快进一次的上限(分钟)。updateTime 在 32 位里算 time_×60,超过约 3579 万分钟会溢出;
/// 另外一次跳太久会让作物直接枯萎,取 30 天足够调试用。
const TIME_SKIP_MAX_MINUTES: i64 = 43_200;
/// 时间旅行一次的上限(小时)。
const TIME_TRAVEL_MAX_HOURS: i64 = 8_760;

// ───────────────────────── 通用小工具 ─────────────────────────

fn sel(env: &mut Environment, name: &str) -> SEL {
    env.objc
        .register_host_selector(name.to_string(), &mut env.mem)
}

fn singleton(env: &mut Environment, class_name: &str, shared: &str) -> id {
    let cls = env.objc.get_known_class(class_name, &mut env.mem);
    if cls == nil {
        return nil;
    }
    let s = sel(env, shared);
    msg_send(env, (cls, s))
}

/// 读 ivar 槽里的运行时偏移;读到 0 或离谱的值就用静态偏移。
fn ivar_offset(env: &Environment, slot: u32, expected: u32) -> u32 {
    let slot_ptr: ConstPtr<u32> = Ptr::from_bits(slot);
    let v: u32 = env.mem.read(slot_ptr);
    if v == 0 || v > 0x1000 {
        expected
    } else {
        v
    }
}

fn read_id_at(env: &Environment, addr: u32) -> id {
    let p: ConstPtr<u32> = Ptr::from_bits(addr);
    let v: u32 = env.mem.read(p);
    Ptr::from_bits(v)
}

/// `[obj isKindOfClass:NSClassFromString(class_name)]`(宿主 NSObject 实现,签名 bool/Class)。
fn is_kind_of(env: &mut Environment, obj: id, class_name: &str) -> bool {
    if obj == nil {
        return false;
    }
    let cls = env.objc.get_known_class(class_name, &mut env.mem);
    if cls == nil {
        return false;
    }
    let s = sel(env, "isKindOfClass:");
    msg_send(env, (obj, s, cls))
}

fn running_scene(env: &mut Environment) -> id {
    let director = singleton(env, "CCDirector", "sharedDirector");
    if director == nil {
        return nil;
    }
    let s = sel(env, "runningScene");
    msg_send(env, (director, s))
}

/// 当前正在显示的主村 VillageLayer;不在主村(标题、过场、黄金岛、节日村等)返回 nil。
/// 取法:-[InGameScene init]@0x186a8 用 `addChild:VillageLayer z:0 tag:0` 挂村庄层,所以从运行中场景
/// getChildByTag:0 取,再核对类型。刻意不用 [GameManager villageLayer]:离开主村后那个 ivar 可能是悬空指针。
fn main_village_layer(env: &mut Environment) -> id {
    let scene = running_scene(env);
    if !is_kind_of(env, scene, "InGameScene") {
        return nil;
    }
    let s = sel(env, "getChildByTag:");
    let layer: id = msg_send(env, (scene, s, 0i32));
    if is_kind_of(env, layer, "VillageLayer") {
        layer
    } else {
        nil
    }
}

fn user_info_data(env: &mut Environment) -> id {
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return nil;
    }
    let s = sel(env, "userInfoData");
    msg_send(env, (gd, s))
}

/// `[[GameData sharedInstance] <selector>]`(无参、无返回值,如 saveUserInfoData / saveMapData)。
fn game_data_call(env: &mut Environment, selector: &str) {
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd != nil {
        let s = sel(env, selector);
        let _: () = msg_send(env, (gd, s));
    }
}

// ───────────────────────── 钩子(剧情回放保护) ─────────────────────────

/// 剧情回放进行中:要把 nextStep 播完时推进 nextStorySectionId 的那一次调用吞掉。
static STORY_REPLAY: AtomicBool = AtomicBool::new(false);
/// [复核修 2026-09-15] R6-1 正在回放的段号。-[Story nextStep] 在 0x1142e2..0x1142ec 发的是 `curSection+1`,
/// curSection 就是 -[Story nextSection:] 在 0x113dc4 写入的参数,所以回放那一次推进的参数必然等于本值+1。
static STORY_REPLAY_SECTION: AtomicI32 = AtomicI32::new(0);

/// 见 mole_activity::wants。热路径:先读原子变量,平时恒为 false,不做字符串比较。
pub fn wants(class_name: &str, sel_name: &str) -> bool {
    STORY_REPLAY.load(O) && sel_name == "setNextStorySectionId:" && class_name == "UserInfoData"
}

/// 见 mole_activity::intercept。
/// [扫描修 2026-09-15] F4-2/F7-7 剧情回放:[Story nextSection:N] 播完后,-[Story nextStep] 会
/// `setNextStorySectionId:N+1` 再立刻 `[self activate]`。事后回写会与 activate 连播下一段抢时序,
/// 所以在回放期间只把「nextStep 那一个调用点」发来的这次 setter 吞掉(按 LR 精确匹配,其它调用者
/// ——读档/存档/限时剧情等——一律放行),吞掉后立即清标志。这里不发任何宿主 msg_send,无需恢复寄存器。
pub fn intercept(env: &mut Environment, class_name: &str, sel_name: &str) -> Option<bool> {
    if !wants(class_name, sel_name) {
        return None;
    }
    let lr = env.cpu.regs()[14] & !1u32;
    if lr != STORY_NEXTSTEP_SET_NEXT_LR {
        return None;
    }
    STORY_REPLAY.store(false, O);
    let attempted = env.cpu.regs()[2] as i32;
    // [复核修 2026-09-15] R6-1 段号校验:「解锁交互」在剧情播放/排队中不再清标志,标志会活得更久;
    // 回放若被 -[Story reset]@0x11449c 之类打断而标志没清,之后真实待播段 P 播完时 nextStep 也从同一调用点
    // 发 setNextStorySectionId:P+1,只看 LR 会把真实进度吞掉。推进目标不是「回放段+1」就说明不是回放那一次:
    // 清标志并放行。这里仍然只读寄存器和原子变量,不发 msg_send(nextStep 可能在 update: 帧栈上)。
    let expected = STORY_REPLAY_SECTION.load(O).wrapping_add(1);
    if attempted != expected {
        log!(
            "[MOLEDEV] 剧情回放保护作废:nextStep 推进到 {},不是回放段的下一段 {}(回放已中断),清除保护并放行",
            attempted,
            expected
        );
        return None;
    }
    log!(
        "[MOLEDEV] 剧情回放结束:吞掉 nextStep 的 setNextStorySectionId:{},存档里的剧情进度保持不变",
        attempted
    );
    Some(true)
}

// ───────────────────────── 启动期:恢复快照 ─────────────────────────

static STARTUP_DONE: AtomicBool = AtomicBool::new(false);

/// mole_cheats::intercept 第一次被调用时调用一次(早于游戏读档):处理「下次启动恢复快照」等。
/// [扫描修 2026-09-15] F7-10 时机核实:游戏 main(0xe8aa 起)只建 NSAutoreleasePool 就调 UIApplicationMain;
/// 宿主 UIApplicationMain 先建 UIApplication/载 nib,随后第一批发给 iMoleVillageAppDelegate 的消息
/// (该类在 mole_cheats::intercept_wants 白名单里)就会进 intercept → 这里。宿主代码只在退出时和
/// CFPreferences 里才碰 NSUserDefaults,游戏自己读偏好在 didFinishLaunching 之后,所以此刻偏好 plist
/// 和存档都还没被读进内存,直接改写沙盒文件就是完整的回滚。
pub fn startup(env: &mut Environment) {
    if STARTUP_DONE.swap(true, O) {
        return;
    }
    let root = snapshots_root();
    let marker = root.join(RESTORE_MARKER);
    let Ok(raw) = std::fs::read_to_string(&marker) else {
        return;
    };
    let name = raw.trim().to_string();
    if env.options.network_access {
        log!(
            "[MOLEDEV] 检测到待恢复快照 {},但当前是在线模式(存档以服务器为准),本次不恢复,标记保留到下次离线启动",
            name
        );
        return;
    }
    let dir = root.join(&name);
    if !valid_snapshot_name(&name) || !dir.is_dir() {
        log!(
            "[MOLEDEV] 待恢复快照 {:?} 不存在或名字不合法,已删除恢复标记",
            name
        );
        let _ = std::fs::remove_file(&marker);
        return;
    }
    match restore_snapshot_files(env, &dir) {
        Ok(n) => {
            log!(
                "[MOLEDEV] 已从快照 {} 恢复 {} 个文件(游戏尚未读档)",
                name,
                n
            );
            // 只在恢复成功时删标记。
            if let Err(e) = std::fs::remove_file(&marker) {
                log!(
                    "[MOLEDEV] 删除恢复标记 {} 失败:{}(不删的话下次启动会再回滚一次,请退出游戏后手动删除)",
                    marker.display(),
                    e
                );
            }
        }
        Err(e) => {
            // [复核修 2026-09-15] R6-4 失败时保留恢复标记,下次离线启动重试。原来成功失败都删标记:写回中途失败时
            // 沙盒停在「部分快照 dat + 当前偏好 plist」的混合状态且再也不重试,isEncrypt 不配套会弹「存档损坏」。
            // restore_snapshot_files 现在先全部写成临时文件、全部成功才逐个替换,错误信息里写明沙盒有没有被改动。
            log!(
                "[MOLEDEV] 恢复快照 {} 失败:{}。恢复标记已保留,下次离线启动会重试;不想再恢复的话,退出游戏后删除 {}",
                name,
                e,
                marker.display()
            );
        }
    }
}

// ───────────────────────── 数值寄存器 ─────────────────────────

/// [扫描修 2026-09-15] F7-2 数值寄存器:绝对值 + 符号分开存,这样「先按 ± 再输数字」也能得到负数。
static REG_ABS: AtomicU64 = AtomicU64::new(0);
static REG_NEG: AtomicBool = AtomicBool::new(false);
/// 最多 12 位十进制。
const REG_MAX_ABS: u64 = 999_999_999_999;

/// 数值寄存器当前值(菜单数字键盘输入)。
pub fn register_value() -> i64 {
    let a = REG_ABS.load(O).min(REG_MAX_ABS) as i64;
    if REG_NEG.load(O) {
        -a
    } else {
        a
    }
}
pub fn register_push_digit(d: u8) {
    if d > 9 {
        return;
    }
    let a = REG_ABS.load(O);
    let n = a.saturating_mul(10).saturating_add(d as u64);
    if n > REG_MAX_ABS {
        return; // 已满 12 位,忽略
    }
    REG_ABS.store(n, O);
}
pub fn register_backspace() {
    let a = REG_ABS.load(O) / 10;
    REG_ABS.store(a, O);
    if a == 0 {
        REG_NEG.store(false, O);
    }
}
pub fn register_clear() {
    REG_ABS.store(0, O);
    REG_NEG.store(false, O);
}
pub fn register_negate() {
    let neg = REG_NEG.load(O);
    REG_NEG.store(!neg, O);
}

// ───────────────────────── 时间快进(原版 TestLayer) ─────────────────────────

thread_local! {
    /// 专供时间快进的离屏 TestLayer(不挂场景、常驻)。和菜单自己的 ghost 分开,避免互相改 ivar。
    static DEV_TEST_LAYER: Cell<id> = const { Cell::new(nil) };
    /// 当前天气粒子层(retain 持有;换天气或清除时 removeFromParentAndCleanup: 后 release)。
    static WEATHER_NODE: Cell<id> = const { Cell::new(nil) };
}

fn dev_test_layer(env: &mut Environment) -> id {
    let existing = DEV_TEST_LAYER.with(|c| c.get());
    if existing != nil {
        return existing;
    }
    let cls = env.objc.get_known_class("TestLayer", &mut env.mem);
    if cls == nil {
        return nil;
    }
    let s_alloc = sel(env, "alloc");
    let obj: id = msg_send(env, (cls, s_alloc));
    if obj == nil {
        return nil;
    }
    let s_init = sel(env, "init");
    let obj: id = msg_send(env, (obj, s_init));
    if obj != nil {
        // alloc/init 返回 +1,这份引用由本模块永久持有,不 release。
        DEV_TEST_LAYER.with(|c| c.set(obj));
    }
    obj
}

/// [扫描修 2026-09-15] F7-1/F6-1 时间快进:复刻原版 GM 面板「时间」按钮。
/// 根因:菜单原来只发 onButtonTimePlus:(只把 time_ +10 再 updateUI),真正生效的是
/// onButtonTimeTouched:@0x146d18 → updateTime@0x146204(对全部 Building/SpacialObject/Farm 发 setAccTime:,
/// 并推进 npcs 冷却与 ActorManager 的 NPC/动物)。updateTime 不会把 time_ 清零,所以这里写入**绝对**分钟数,
/// 应用完立刻写回 0,连点也不会叠加。
pub fn apply_time_minutes(env: &mut Environment, minutes: i64) -> DevResult {
    if env.options.network_access {
        return Err("在线模式下作物和冷却时间以服务器为准,不能快进".to_string());
    }
    if minutes <= 0 {
        return Err("快进的分钟数必须是正数".to_string());
    }
    if minutes > TIME_SKIP_MAX_MINUTES {
        return Err(format!(
            "一次最多快进 {} 分钟(30 天)",
            TIME_SKIP_MAX_MINUTES
        ));
    }
    if crate::mole_cheats::island_session_active() {
        // 岛上对象的计时走 NewSceneTimer,updateTime 在岛上的效果未核实,先只开放主村。
        return Err("黄金岛上暂不支持时间快进,请回主村使用".to_string());
    }
    if main_village_layer(env) == nil {
        return Err("请先进入主村再快进时间".to_string());
    }
    let layer = dev_test_layer(env);
    if layer == nil {
        return Err("创建 TestLayer 失败,无法快进".to_string());
    }
    let off = ivar_offset(env, TESTLAYER_TIME_SLOT, TESTLAYER_TIME_OFF);
    let slot: MutPtr<u32> = Ptr::from_bits(layer.to_bits() + off);
    env.mem.write(slot, minutes as u32);
    let s = sel(env, "onButtonTimeTouched:");
    let _: () = msg_send(env, (layer, s, nil));
    env.mem.write(slot, 0u32);
    // NPC 冷却时间戳在 userInfoData 里,落一次盘;地图对象的时间由游戏自己的存图流程带走。
    game_data_call(env, "saveUserInfoData");
    log!("[MOLEDEV] 时间快进 {} 分钟(TestLayer updateTime)", minutes);
    Ok(format!(
        "已快进 {} 分钟:作物、建筑、NPC 和动物冷却都按原版 GM 面板逻辑推进",
        minutes
    ))
}

// ───────────────────────── 任务跳转 ─────────────────────────

/// [扫描修 2026-09-15] F7-4 任务跳转:四族 quickStart:。
/// 核实:-[Quest quickStart:]@0x128500 做的是 questState=0、setCurQuestId:0、setNextQuestId:N,
/// 要等下一次 Quest activate: 才真正变成任务 N,所以主线跳转后补发 [GameManager activateStoryQuest]
/// (@0x1a218 → Story activate → Quest activate:)。限时/VIP 的进度存在 map 里的 timeQuestDataInMap/
/// vipQuestDataInMap,由 -[GameData saveMapData:] 里的 saveTimeQuestDataInDir/saveVipQuestDataInDir 落盘。
/// 黄金岛 -[NewSceneQuest quickStart:]@0x32b510 自己会调 saveUserinfoBothInLocalAndRemote。
/// 任务号范围照原版 onButton*QuestPlus: 的封顶:1..对应数据表 count。
pub fn quest_jump(env: &mut Environment, family: QuestFamily, quest_id: i64) -> DevResult {
    if env.options.network_access {
        return Err("在线模式下任务进度由服务器同步,不能跳转".to_string());
    }
    let on_island = crate::mole_cheats::island_session_active();
    let (data_class, data_shared, data_sel, quest_class, quest_shared, label) = match family {
        QuestFamily::Main => (
            "GameData",
            "sharedInstance",
            "questData",
            "Quest",
            "instance",
            "主线",
        ),
        QuestFamily::Time => (
            "GameData",
            "sharedInstance",
            "timeQuestData",
            "TimeQuest",
            "instance",
            "限时",
        ),
        QuestFamily::Vip => (
            "GameData",
            "sharedInstance",
            "vipQuestData",
            "VipQuest",
            "instance",
            "VIP",
        ),
        QuestFamily::Island => (
            "NewSceneData",
            "sharedInstance",
            "questData",
            "NewSceneQuest",
            "sharedInstance",
            "黄金岛",
        ),
    };
    if family == QuestFamily::Island {
        if !on_island {
            return Err("黄金岛任务只能在岛上跳转".to_string());
        }
    } else {
        if on_island {
            return Err("请先回主村再跳转主村任务".to_string());
        }
        if main_village_layer(env) == nil {
            return Err("请先进入主村再跳转任务".to_string());
        }
    }
    let holder = singleton(env, data_class, data_shared);
    if holder == nil {
        return Err(format!("{} 还没初始化", data_class));
    }
    let s = sel(env, data_sel);
    let data: id = msg_send(env, (holder, s));
    if data == nil {
        return Err(format!("{}任务表还没加载", label));
    }
    let s = sel(env, "count");
    let count: u32 = msg_send(env, (data, s));
    if count == 0 {
        return Err(format!("{}任务表是空的", label));
    }
    if quest_id < 1 || quest_id > count as i64 {
        return Err(format!(
            "{}任务号必须在 1..{} 之间(当前输入 {})",
            label, count, quest_id
        ));
    }
    let quest = singleton(env, quest_class, quest_shared);
    if quest == nil {
        return Err(format!("{} 单例不存在", quest_class));
    }
    let s = sel(env, "quickStart:");
    let _: () = msg_send(env, (quest, s, quest_id as i32));
    match family {
        QuestFamily::Main => {
            let gm = singleton(env, "GameManager", "sharedManager");
            if gm != nil {
                let s = sel(env, "activateStoryQuest");
                let _: () = msg_send(env, (gm, s));
            }
            game_data_call(env, "saveUserInfoData");
        }
        QuestFamily::Time => {
            game_data_call(env, "saveUserInfoData");
            game_data_call(env, "saveMapData");
            // [2026-09-16] A2-05 限时跳转后补发激活,与主线补发 activateStoryQuest 同理:quickStart: 只写 nextQuestId,
            // 要等 -[TimeQuest activate:] 才真正变成任务 N。-[GameManager activateTimeStoryQuest]@0x1a434 就是
            // [[TimeQuest instance] activate:0](0x1a464)+ [[TimeQuest instance] checkLastTimeState](0x1a47c),
            // 原版 ActorManager touchEnd:/UserInfoData checkUpgrade 也发 activate:。activate: 自己的门照原版执行:
            // 0x1d88ee 语言门(currentUserLanguange:1 = zh-Hans 为真即放行,默认 --preferred-languages=zh-Hans 不受限)、
            // GameManager.gameMode 不为 0/6(0x1d892a/0x1d893e)、checkCanActivate(questState/timeQuestData:/等级≥needLevel)。
            // 这里是菜单点击回调,不在帧栈上,可以发宿主 msg_send;不在 intercept 里,不需要恢复 r0-r3。
            let gm = singleton(env, "GameManager", "sharedManager");
            if gm != nil {
                let s = sel(env, "activateTimeStoryQuest");
                let _: () = msg_send(env, (gm, s));
            }
        }
        QuestFamily::Vip => {
            game_data_call(env, "saveUserInfoData");
            game_data_call(env, "saveMapData");
        }
        QuestFamily::Island => {}
    }
    log!(
        "[MOLEDEV] 任务跳转 {} → {}(表内共 {} 条)",
        label,
        quest_id,
        count
    );
    // [2026-09-16] A2-05 删掉原来「简体中文下限时/VIP 任务受原版语言门限制」的附注:与反汇编相反。
    // -[VipQuest activate:]@0x38722c 没有语言门;-[TimeQuest activate:] 的语言门在 zh-Hans 下放行(见上)。
    let note = match family {
        // 文案保持短:菜单还会在后面追加激活条件,toast 只有 992 宽。
        QuestFamily::Time => ";已补发激活",
        _ => "",
    };
    Ok(format!(
        "已跳到{}任务 {}(被跳过任务的奖励和前置条件不会补发){}",
        label, quest_id, note
    ))
}

// ───────────────────────── 剧情回放 ─────────────────────────

/// [扫描修 2026-09-15] F7-7/F4-2 剧情回放:直接 [[Story instance] nextSection:N]。
/// 核实:-[Story nextSection:]@0x113d10 只有等级门(triggerLevel≥1 且 curLevel<triggerLevel 时静默返回,
/// 0x113d9a/0x113daa),没有语言门和任务门;通过后 setIsInteractEnabled:NO 并 [storyLayer beginStory]。
/// 「是否正在播」看 [storyLayer isOpen](beginStory@0x11478a 置位、endStory@0x1147f4 清零)——
/// 不能用 -[Story isRunning],它转发的是 CCNode 的 isRunning(节点在场景里),不是播放状态。
/// 进度保护:见 `intercept`,回放期间吞掉 nextStep 那一次 setNextStorySectionId:。
/// 如果 N 恰好就是存档里待播的那一段,就不保护,让原版自然推进。
pub fn story_play(env: &mut Environment, section: i64) -> DevResult {
    if section < 1 || section > i32::MAX as i64 {
        return Err("剧情段号必须是正整数".to_string());
    }
    if crate::mole_cheats::island_session_active() {
        return Err("黄金岛剧情是另一套(NewSceneStory),这里只回放主村剧情".to_string());
    }
    if main_village_layer(env) == nil {
        return Err("请在主村空闲时回放剧情".to_string());
    }
    let story = singleton(env, "Story", "instance");
    if story == nil {
        return Err("Story 单例不存在".to_string());
    }
    let off = ivar_offset(env, STORY_LAYER_SLOT, STORY_LAYER_OFF);
    let story_layer = read_id_at(env, story.to_bits() + off);
    if story_layer == nil {
        // storyLayer 为 nil 时 nextSection: 仍会先 setIsInteractEnabled:NO,beginStory 发给 nil 什么都不做 → 交互锁死。
        return Err("剧情层还没就绪,请在主村空闲时使用".to_string());
    }
    let s = sel(env, "isOpen");
    let open: u8 = msg_send(env, (story_layer, s));
    if open != 0 {
        return Err("已经有剧情在播放,请先看完".to_string());
    }
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return Err("GameData 还没初始化".to_string());
    }
    let s = sel(env, "storySection:");
    let section_data: id = msg_send(env, (gd, s, section as i32));
    if section_data == nil {
        return Err(format!("剧情段 {} 不存在", section));
    }
    let s = sel(env, "triggerLevel");
    let trigger: i32 = msg_send(env, (section_data, s));
    let ui = user_info_data(env);
    if ui == nil {
        return Err("玩家数据还没加载".to_string());
    }
    let s = sel(env, "curLevel");
    let level: i32 = msg_send(env, (ui, s));
    if trigger >= 1 && level < trigger {
        return Err(format!(
            "剧情段 {} 需要 {} 级才能播放(当前 {} 级)",
            section, trigger, level
        ));
    }
    let s = sel(env, "nextStorySectionId");
    let pending: i32 = msg_send(env, (ui, s));
    let protect = pending != section as i32;
    // [复核修 2026-09-15] R6-1 先记回放段号再置标志:intercept 按「回放段+1」认出回放那一次推进。
    STORY_REPLAY_SECTION.store(section as i32, O);
    STORY_REPLAY.store(protect, O);
    let s = sel(env, "nextSection:");
    let _: () = msg_send(env, (story, s, section as i32));
    log!(
        "[MOLEDEV] 回放剧情段 {}(待播段 {},进度保护={})",
        section,
        pending,
        protect
    );
    Ok(format!(
        "开始回放剧情段 {};看完后剧情进度仍停在第 {} 段。若画面卡住不能点,请用「解锁交互」",
        section, pending
    ))
}

/// [复核修 2026-09-15] R6-1 主村剧情是否「已开始、还没播完」:storyLayer 正在显示(isOpen,beginStory@0x11478a
/// 置位、endStory@0x1147f4 清零),或被顶层面板挡着排队待播(isStandby,见 STORYLAYER_STANDBY_SLOT)。
/// 取 Story/storyLayer 的写法与 story_play 相同;不在主村时不碰 storyLayer(离开主村后那个 ivar 可能悬空),
/// 直接视为没有剧情在进行。-[Story nextStep] 在 0x113ec8 先 endStory、再到 0x1142ee 推进进度,同一次调用内完成,
/// 菜单点击插不进中间,所以两个标志都为 0 时,回放那一次推进要么已经发生,要么再也不会发生。
fn story_in_progress(env: &mut Environment) -> bool {
    if main_village_layer(env) == nil {
        return false;
    }
    let story = singleton(env, "Story", "instance");
    if story == nil {
        return false;
    }
    let off = ivar_offset(env, STORY_LAYER_SLOT, STORY_LAYER_OFF);
    let story_layer = read_id_at(env, story.to_bits() + off);
    if story_layer == nil {
        return false;
    }
    let s = sel(env, "isOpen");
    let open: u8 = msg_send(env, (story_layer, s));
    let standby_off = ivar_offset(env, STORYLAYER_STANDBY_SLOT, STORYLAYER_STANDBY_OFF);
    let standby_ptr: ConstPtr<u8> = Ptr::from_bits(story_layer.to_bits() + standby_off);
    let standby: u8 = env.mem.read(standby_ptr);
    open != 0 || standby != 0
}

/// [扫描修 2026-09-15] F7-7 兜底:剧情异常时交互被 setIsInteractEnabled:NO 锁死,手动解开。
/// 同时清掉回放保护标志(剧情没正常播完,就不该再吞之后真实的进度推进)。
/// [复核修 2026-09-15] R6-1 原来无条件清标志:回放剧情还开着时按「解锁交互」(story_play 的 toast 就这么提示),
/// 回放播完后 nextStep@0x1142ee 的 setNextStorySectionId:N+1 不再被吞,存档待播段被改成 N+1,
/// -[Story activate]@0x113c36 读 nextStorySectionId 就从 N+1 起连播旧剧情、重发奖励。
/// 改为:剧情仍在播放或排队(story_in_progress)时保留标志、只恢复交互;确认没有剧情在进行才清。
pub fn unlock_interaction(env: &mut Environment) -> DevResult {
    let keep_protect = STORY_REPLAY.load(O) && story_in_progress(env);
    if !keep_protect {
        STORY_REPLAY.store(false, O);
    }
    let gm = singleton(env, "GameManager", "sharedManager");
    if gm == nil {
        return Err("GameManager 还没初始化".to_string());
    }
    let s = sel(env, "setIsInteractEnabled:");
    let _: () = msg_send(env, (gm, s, true));
    log!(
        "[MOLEDEV] 手动恢复交互 setIsInteractEnabled:YES(剧情回放保护:{})",
        if keep_protect {
            "保留,回放剧情还没播完"
        } else {
            "已清除"
        }
    );
    if keep_protect {
        Ok("已恢复村庄交互;回放的剧情还没播完,看完后剧情进度仍保持不变".to_string())
    } else {
        Ok("已恢复村庄交互".to_string())
    }
}

// ───────────────────────── 天气粒子 ─────────────────────────

fn weather_name(kind: i64) -> Option<&'static str> {
    match kind {
        0 => Some("下雪"),
        1 => Some("下雨"),
        3 => Some("云雾"),
        4 => Some("烟雾"),
        5 => Some("水面涟漪"),
        6 => Some("气泡"),
        7 => Some("大量气泡"),
        9 => Some("浓雾"),
        11 => Some("烧烤烟"),
        12 => Some("月光喷泉"),
        _ => None,
    }
}

fn remove_weather_node(env: &mut Environment) -> bool {
    let prev = WEATHER_NODE.with(|c| c.get());
    if prev == nil {
        return false;
    }
    let s = sel(env, "removeFromParentAndCleanup:");
    let _: () = msg_send(env, (prev, s, true));
    release(env, prev);
    WEATHER_NODE.with(|c| c.set(nil));
    true
}

/// [扫描修 2026-09-15] F7-6 天气:[ParticalManager node] + showWeather:kind。
/// 核实:showWeather:@0x19adec 是 tbb 跳转表(r2≤13):0 雪 1 雨 2 风 3 云 4 烟 5 涟漪 6 气泡 7 多气泡 9 雾
/// 11 烧烤烟 12 月光喷泉 13 占卜星;8/10 直接跳到函数尾,2 号 showWind@0x196a80 只有 4 字节(空实现)。
/// 挂载点按复核意见改成运行中场景(屏幕空间,和 FishingGame 一样),不挂会平移缩放的 villageLayer;
/// z=1 与 InGameScene 里原版 FireworkLayer 同级(村庄层 z0 之上、VillageMenuLayer z2 之下)。
/// 约定外扩展:kind < 0 表示清除当前天气。
pub fn weather(env: &mut Environment, kind: i64) -> DevResult {
    if kind < 0 {
        return if remove_weather_node(env) {
            Ok("已清除天气".to_string())
        } else {
            Ok("当前没有天气效果".to_string())
        };
    }
    if kind > 12 {
        return Err("天气编号范围是 0..12".to_string());
    }
    let Some(name) = weather_name(kind) else {
        return Err(format!("天气编号 {} 在原版里是空实现,没有效果", kind));
    };
    let scene = running_scene(env);
    if scene == nil {
        return Err("当前没有运行中的场景".to_string());
    }
    remove_weather_node(env);
    let pm = singleton(env, "ParticalManager", "node");
    if pm == nil {
        return Err("创建 ParticalManager 失败".to_string());
    }
    // +node 返回 autoreleased,自己 retain 一份,清除时配对 release。
    retain(env, pm);
    let s = sel(env, "showWeather:");
    let _: () = msg_send(env, (pm, s, kind as i32));
    let s = sel(env, "addChild:z:");
    let _: () = msg_send(env, (scene, s, pm, 1i32));
    WEATHER_NODE.with(|c| c.set(pm));
    log!("[MOLEDEV] 天气 {}({})挂到运行中场景 z=1", kind, name);
    Ok(format!("天气:{}(只是观赏效果,切换场景后需要重新开)", name))
}

// ───────────────────────── 动画倍速 ─────────────────────────

/// [扫描修 2026-09-15] F6-5 动画倍速:直接写 CCScheduler.timeScale_(f32)。
/// 核实:-[CCScheduler tick:] 读 timeScale_ 乘到 dt 上;作物成熟用 CFAbsoluteTimeGetCurrent 与 beginTime
/// 比较(-[Farm innerupdate:]),不吃 dt,所以这只影响动画、走路、调度节拍,不是时间作弊。
/// CCScheduler 是全局单例,倍率跨场景一直生效,菜单要提供「还原 ×1」。
pub fn set_time_scale(env: &mut Environment, scale: f32) -> DevResult {
    if !(0.25f32..=4.0f32).contains(&scale) {
        return Err("动画倍速范围是 0.25 到 4".to_string());
    }
    let sched = singleton(env, "CCScheduler", "sharedScheduler");
    if sched == nil {
        return Err("CCScheduler 还没初始化".to_string());
    }
    let off = ivar_offset(env, SCHED_TIMESCALE_SLOT, SCHED_TIMESCALE_OFF);
    let p: MutPtr<f32> = Ptr::from_bits(sched.to_bits() + off);
    env.mem.write(p, scale);
    log!("[MOLEDEV] CCScheduler timeScale = {}", scale);
    Ok(format!(
        "动画倍速 ×{}(只影响动画和调度节拍,作物成熟仍按真实时间)",
        scale
    ))
}

// ───────────────────────── 原版 FPS 显示 ─────────────────────────

/// [扫描修 2026-09-15] F6-2/F7-8 原版 FPS 显示:切换 CCDirector.displayFPS_。
/// 核实:setDisplayFPS:@0x2c8728 只是 strb 到 +24;FPS 标签(CCLabelAtlas + fps_images.png)在
/// setGLDefaultValues@0x2c7d38 无条件创建,置位后 drawScene 每帧 showFPS。菜单点击不在帧栈上,msg_send 安全。
/// 已知限制:内存数字来自 touchHLE task_info 的写死值(它的 TODO 提示已降为 log_dbg!,不会刷屏)。
pub fn toggle_fps(env: &mut Environment) -> DevResult {
    let director = singleton(env, "CCDirector", "sharedDirector");
    if director == nil {
        return Err("CCDirector 还没初始化".to_string());
    }
    let off = ivar_offset(env, DIRECTOR_DISPLAYFPS_SLOT, DIRECTOR_DISPLAYFPS_OFF);
    let p: ConstPtr<u8> = Ptr::from_bits(director.to_bits() + off);
    let cur: u8 = env.mem.read(p);
    let turn_on = cur == 0;
    let s = sel(env, "setDisplayFPS:");
    let _: () = msg_send(env, (director, s, turn_on));
    log!("[MOLEDEV] CCDirector displayFPS = {}", turn_on);
    if turn_on {
        Ok("原版 FPS 显示:开(画面左下角;内存数字是模拟器占位值)".to_string())
    } else {
        Ok("原版 FPS 显示:关".to_string())
    }
}

// ───────────────────────── 地图格线 ─────────────────────────

/// [扫描修 2026-09-15] F6-6 地图格线:-[Map draw]@0x28b38 的 `70 47`(bx lr)↔ `00 bf`(nop)。
/// 写前校验 4 个字节(前两字节是香草或已补丁、后两字节必须是 nop),不符就拒绝,防止二进制不同版本时乱写。
/// 刻意不进 mole_cheats 的 CRACK_PATCHES(那张表是自动生成的,且会在破解开关变化时整表重写)。
/// 代价:debugDraw 每帧 36×185 格立即模式画线,只当调试开关用。
pub fn toggle_map_grid(env: &mut Environment) -> DevResult {
    let rp: ConstPtr<u8> = Ptr::from_bits(MAP_DRAW_ADDR);
    let mut cur = [0u8; 4];
    cur.copy_from_slice(env.mem.bytes_at(rp, 4));
    if cur[2..4] != MAP_DRAW_TAIL {
        return Err(format!(
            "0x{:x} 处字节 {:02x} {:02x} {:02x} {:02x} 与预期不符,拒绝打补丁",
            MAP_DRAW_ADDR, cur[0], cur[1], cur[2], cur[3]
        ));
    }
    let (new_bytes, on) = if cur[0..2] == MAP_DRAW_VANILLA {
        (MAP_DRAW_PATCHED, true)
    } else if cur[0..2] == MAP_DRAW_PATCHED {
        (MAP_DRAW_VANILLA, false)
    } else {
        return Err(format!(
            "0x{:x} 处字节 {:02x} {:02x} 既不是原版也不是补丁,拒绝打补丁",
            MAP_DRAW_ADDR, cur[0], cur[1]
        ));
    };
    let wp: MutPtr<u8> = Ptr::from_bits(MAP_DRAW_ADDR);
    env.mem.bytes_at_mut(wp, 2).copy_from_slice(&new_bytes);
    env.cpu.invalidate_cache_range(MAP_DRAW_ADDR, 2);
    log!("[MOLEDEV] 地图格线(-[Map debugDraw])= {}", on);
    if on {
        Ok("地图格线:开(调试用,每帧大量画线会掉帧)".to_string())
    } else {
        Ok("地图格线:关".to_string())
    }
}

// ───────────────────────── 时间旅行 ─────────────────────────

/// [扫描修 2026-09-15] F7-5 时间旅行:只往前拨,调 libc::time 的全局墙钟偏移(W13 负责让各时间源加上它)。
pub fn time_travel_hours(env: &mut Environment, hours: i64) -> DevResult {
    if env.options.network_access {
        return Err("在线模式以服务器时间为准,不能时间旅行".to_string());
    }
    if hours <= 0 {
        return Err("只能往前拨,请输入正整数小时".to_string());
    }
    if hours > TIME_TRAVEL_MAX_HOURS {
        return Err(format!("一次最多前进 {} 小时(一年)", TIME_TRAVEL_MAX_HOURS));
    }
    crate::libc::time::add_time_offset_secs(hours * 3600);
    let total_hours = crate::libc::time::time_offset_secs() / 3600;
    log!(
        "[MOLEDEV] 时间旅行 +{} 小时,累计偏移 {} 小时",
        hours,
        total_hours
    );
    // [2026-09-16] X4-02 成功文案补一句活动中心的限制,与菜单确认文案一致:旅行期间活动侧档只写内存(F2-05),
    // 付费操作的扣款却照常进主档,所以这些操作被禁用(拦截在 mole_activity.rs);旅行中拍的快照活动档仍是旅行前的。
    Ok(format!(
        "已前进 {} 小时(不可回退),本次运行累计 {} 小时。偏移不跨重启保存:重启后时间回到现实,期间存下的\"未来\"时间要等现实追上。旅行期间活动中心付费操作禁用,此时拍的快照活动数据与主档不一致",
        hours, total_hours
    ))
}

// ───────────────────────── 存档快照 ─────────────────────────

/// 快照要复制的沙盒 Documents 文件,与 save_reset.rs 的删档清单是同一组文件(改一处要同步另一处)。
/// 快照另外带偏好 plist(见 snapshot_save),删档不删偏好 plist。
/// [2026-09-16] 删档另外会删 mole_activity.dat 的坏档备份(.corrupt / .corrupt-<秒>),那不是游戏进度,快照不收。
/// [复核修 2026-09-15] R6-2 补 mole_activity.dat:签到/脚印兑换/海底寻宝/烟花去重这些原本在服务器上的状态
/// 存在这个旁路档(mole_activity.rs STATE_FILE,经 -[GameData pathForDataFile:]@0x75374 =
/// NSSearchPathForDirectoriesInDomains(NSDocumentDirectory) 落在 Documents),漏掉它快照回滚后 userinfo 回到旧状态、
/// 活动状态却停在最新,该领的奖励领不到或重复发。vip.dat 由 mole_items.rs SIDE_FILE 写,已在清单里。
/// 刻意不收游戏自己的 3.dat(GameData.inappPurchaseInfo_ 内购交易记录)与 purchasereceipt.dat(购买凭证):
/// 那是内购记账不是玩法进度,回滚它们只会让交易记录与贝壳数对不上。
const SAVE_FILES: [&str; 12] = [
    "userinfo.dat",
    "map.dat",
    "island_map.dat",
    "island_userinfo.dat",
    "island_ships.dat",
    "island_fragments.dat",
    // [2026-09-24 第四轮骨架] 四份新岛侧档(仓库/咖啡馆/贝壳树/成就与小游戏),与另一份清单同步。
    "island_storage.dat",
    "island_cafe.dat",
    "island_shelltree.dat",
    "island_misc.dat",
    "vip.dat",
    "mole_activity.dat",
];
const SNAPSHOT_DIR: &str = "snapshots";
const RESTORE_MARKER: &str = "RESTORE_PENDING";

fn snapshots_root() -> PathBuf {
    crate::paths::user_data_base_path().join(SNAPSHOT_DIR)
}

/// 快照目录名只允许「数字和连字符」(yyyyMMdd-HHmmss 或带 -2 之类的后缀),防止标记文件被改成 ../ 路径。
fn valid_snapshot_name(name: &str) -> bool {
    name.len() >= 15 && name.chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn latest_snapshot(root: &Path) -> Option<String> {
    let rd = std::fs::read_dir(root).ok()?;
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_snapshot_name(n))
        .max()
}

/// 公历日期换算(Howard Hinnant 算法):1970-01-01 起的天数 → (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 本地时间 yyyyMMdd-HHmmss(用宿主真实时间命名,不受时间旅行偏移影响)。
fn local_timestamp() -> String {
    let now = crate::libc::time::host_now_unix_secs();
    let local = now + crate::libc::time::local_utc_offset_at(now) as i64;
    let days = local.div_euclid(86_400);
    let sod = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        y,
        m,
        d,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// [扫描修 2026-09-15] F7-10 保存快照(运行时做)。
/// 先让游戏把内存里的最新状态落盘(只在主村已加载时做:标题画面 userInfoData 还没读档,
/// 那时 saveUserInfoData 可能把空档写回去),再 synchronize 偏好,然后把沙盒存档与偏好 plist
/// 复制到 <user_data>/snapshots/<时间戳>/。黄金岛上不做:岛档的完整落盘入口 island_flush 是
/// mole_cheats 的私有函数(不归本包,不能调),而离岛时它会自动跑,所以要求先回主村。
pub fn snapshot_save(env: &mut Environment) -> DevResult {
    if crate::mole_cheats::island_session_active() {
        return Err("请先回到主村再保存快照(离岛时岛档会自动完整落盘)".to_string());
    }
    let flushed = if main_village_layer(env) != nil {
        game_data_call(env, "saveUserInfoData");
        game_data_call(env, "saveMapData");
        true
    } else {
        false
    };
    let defaults = singleton(env, "NSUserDefaults", "standardUserDefaults");
    if defaults != nil {
        let s = sel(env, "synchronize");
        let _: bool = msg_send(env, (defaults, s));
    }

    let root = snapshots_root();
    let stamp = local_timestamp();
    let mut name = stamp.clone();
    let mut dir = root.join(&name);
    let mut k = 2;
    while dir.exists() {
        name = format!("{}-{}", stamp, k);
        dir = root.join(&name);
        k += 1;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建快照目录 {} 失败:{}", dir.display(), e))?;

    let docs = env.fs.home_directory().join("Documents");
    let prefs_name = format!("{}.plist", env.bundle.bundle_identifier());
    let prefs_path = env
        .fs
        .home_directory()
        .join("Library")
        .join("Preferences")
        .join(&prefs_name);

    let result = (|| -> Result<(Vec<String>, bool), String> {
        let mut copied = Vec::new();
        for f in SAVE_FILES {
            let gp = docs.join(f);
            if !env.fs.is_file(&gp) {
                continue;
            }
            let data = env
                .fs
                .read(&gp)
                .map_err(|_| format!("读取存档 {} 失败", f))?;
            std::fs::write(dir.join(f), &data)
                .map_err(|e| format!("写入快照文件 {} 失败:{}", f, e))?;
            copied.push(f.to_string());
        }
        if copied.is_empty() {
            return Err("没有找到任何存档文件,当前没有可快照的存档".to_string());
        }
        let mut has_prefs = false;
        if env.fs.is_file(&prefs_path) {
            let data = env
                .fs
                .read(&prefs_path)
                .map_err(|_| "读取偏好 plist 失败".to_string())?;
            std::fs::write(dir.join(&prefs_name), &data)
                .map_err(|e| format!("写入快照偏好 plist 失败:{}", e))?;
            has_prefs = true;
        }
        Ok((copied, has_prefs))
    })();

    let (copied, has_prefs) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let readme = format!(
        "摩尔庄园存档快照 {}\n保存前让游戏落盘:{}\n存档文件:{}\n偏好文件:{}\n恢复:作弊菜单「下次启动恢复快照」恢复最新一份;\n或退出游戏后手动把 .dat 拷回沙盒 Documents、把 .plist 拷回 Library/Preferences。\n",
        name,
        if flushed { "是" } else { "否(不在主村,直接复制磁盘上的存档)" },
        copied.join(", "),
        if has_prefs { prefs_name.as_str() } else { "无" }
    );
    let _ = std::fs::write(dir.join("README.txt"), readme);
    log!(
        "[MOLEDEV] 快照已保存 {}:{} 个存档 + 偏好={}(落盘={})",
        dir.display(),
        copied.len(),
        has_prefs,
        flushed
    );
    Ok(format!(
        "已保存快照 {}({} 个存档文件{}){}",
        name,
        copied.len(),
        if has_prefs { " + 偏好" } else { "" },
        if flushed {
            ""
        } else {
            ";不在主村,复制的是磁盘上的存档"
        }
    ))
}

/// [扫描修 2026-09-15] F7-10 安排「下次启动恢复最新快照」。
/// 复核意见:运行中把文件拷回去没用——内存里的 GameData/NSUserDefaults 还是旧的,下次存档就会把恢复
/// 的内容覆盖掉,还可能因 isEncrypt 与 userinfo 不配套弹「存档损坏」。所以这里只写标记文件,
/// 真正的恢复在 `startup`(游戏读档之前)做。
pub fn snapshot_restore_on_next_launch(env: &mut Environment) -> DevResult {
    if env.options.network_access {
        return Err("在线模式下存档以服务器为准,不支持回滚快照".to_string());
    }
    let root = snapshots_root();
    let Some(latest) = latest_snapshot(&root) else {
        return Err("还没有快照,请先「保存快照」".to_string());
    };
    std::fs::write(root.join(RESTORE_MARKER), latest.as_bytes())
        .map_err(|e| format!("写恢复标记失败:{}", e))?;
    log!("[MOLEDEV] 已安排下次启动恢复快照 {}", latest);
    Ok(format!(
        "已安排恢复快照 {}:请现在退出游戏再重新打开,启动时会自动回滚(退出前的自动存档不影响恢复)",
        latest
    ))
}

/// [2026-09-16] F2-01 删档时撤销「下次启动恢复快照」:删掉 snapshots_root()/RESTORE_PENDING。
/// 根因:startup 在读档前只要看到标记就把快照写回 Documents,删档路径原来都不碰标记,「先安排恢复、再删档」重开后
/// 拿到的是旧快照而不是承诺的全新存档,界面上没有任何提示。快照目录本身不动,玩家仍可再次手动安排恢复。
/// [2026-09-16] X4-01 返回值改成 Ok(true)=标记存在并已删掉、Ok(false)=本来就没有、Err(标记路径)=删不掉。
/// 原来删不掉只打日志返回 false,调用方分不清「没有标记」和「删不掉」,删档照样退出,重开仍被快照回滚。
/// 现在 save_reset::delete_local_saves 在存档全部删掉之后才调它,拿到 Err 就把存档原样写回、菜单不退出。
/// 只用宿主 std::fs,不发 msg_send,可以放在 exit(0) 之前调用。
pub fn cancel_pending_restore() -> Result<bool, String> {
    let marker = snapshots_root().join(RESTORE_MARKER);
    if !marker.exists() {
        return Ok(false);
    }
    let name = std::fs::read_to_string(&marker)
        .map(|raw| raw.trim().to_string())
        .unwrap_or_default();
    match std::fs::remove_file(&marker) {
        Ok(()) => {
            log!("[MOLEDEV] 删档时撤销待恢复快照 {}", name);
            Ok(true)
        }
        // exists 与 remove_file 之间被外部删掉:结果同样是「没有标记」。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => {
            log!(
                "[MOLEDEV] 删档时撤销待恢复快照 {} 失败:{}(删档中止、存档写回;请退出游戏后手动删除 {} 再删档)",
                name,
                e,
                marker.display()
            );
            Err(marker.display().to_string())
        }
    }
}

/// [复核修 2026-09-15] R6-4 恢复用临时文件后缀。必须以 fs.rs 的 ATOMIC_WRITE_TMP_SUFFIX(".touchhle-tmp")结尾:
/// 进程若死在「写临时文件」与「替换」之间,下次启动 fs 建树(from_host_dir)会自动清掉残留,
/// 不会出现在游戏的 Documents 视图里;恢复标记还在,startup 会整套重来。
const RESTORE_TMP_SUFFIX: &str = ".restore.touchhle-tmp";

/// 把快照目录里的文件写回沙盒。
/// 快照里没有的已知存档文件(例如拍快照时还没上过岛)会从 Documents 删掉,保证回到快照那一刻;
/// 偏好 plist 若快照里没有则保留现状,不删(删掉会让 isEncrypt 读成 NO,触发「存档损坏」退出循环)。
/// [复核修 2026-09-15] R6-4 原来只有「读进内存」这一步是全有全无的:写回阶段某个 write_atomic 失败就 `?` 返回,
/// 前面的 dat 已被覆盖、偏好 plist 还是当前版本,startup 又删了恢复标记 → 永久半恢复。现在分三步:
/// 1) 快照内容全部读进内存;2) 逐个写成目标同目录的临时文件,任一失败就删掉已写的临时文件返回
/// (这两步失败时沙盒一个字节没动);3) 全部写好后逐个 rename 覆盖目标(同目录 rename 是原子的),
/// 最后删快照里没有的文件。只有第 3 步中途失败才会半恢复(同卷 rename 几乎不会失败),
/// 错误信息写明沙盒状态,startup 保留恢复标记下次重试。
fn restore_snapshot_files(env: &mut Environment, dir: &Path) -> Result<usize, String> {
    let docs = env.fs.home_directory().join("Documents");
    let prefs_dir = env.fs.home_directory().join("Library").join("Preferences");
    let prefs_name = format!("{}.plist", env.bundle.bundle_identifier());

    // 第 1 步:全部读进内存。
    let mut staged: Vec<(&'static str, Option<Vec<u8>>)> = Vec::new();
    for f in SAVE_FILES {
        let p = dir.join(f);
        if p.is_file() {
            let data = std::fs::read(&p)
                .map_err(|e| format!("读取快照文件 {} 失败:{}(沙盒未改动)", f, e))?;
            staged.push((f, Some(data)));
        } else {
            staged.push((f, None));
        }
    }
    if staged.iter().all(|(_, d)| d.is_none()) {
        return Err("快照里没有任何存档文件(沙盒未改动)".to_string());
    }
    let prefs_src = dir.join(&prefs_name);
    let prefs_data = if prefs_src.is_file() {
        Some(
            std::fs::read(&prefs_src)
                .map_err(|e| format!("读取快照偏好 plist 失败:{}(沙盒未改动)", e))?,
        )
    } else {
        None
    };

    // 待替换:(文件名, 目标路径, 临时文件路径, 内容);待删除:(文件名, 目标路径)。
    // 偏好 plist 排最前、userinfo.dat 紧随其后:isEncrypt/encryVersion 必须与 userinfo.dat 配套,
    // 两者挨着替换,把第 3 步中途失败时不配套的窗口压到最小。
    let mut writes = Vec::new();
    let mut removals = Vec::new();
    if let Some(bytes) = prefs_data {
        let _ = env.fs.create_dir_all(&prefs_dir);
        writes.push((
            prefs_name.clone(),
            prefs_dir.join(&prefs_name),
            prefs_dir.join(format!(".{}{}", prefs_name, RESTORE_TMP_SUFFIX)),
            bytes,
        ));
    }
    let _ = env.fs.create_dir_all(&docs);
    for (f, data) in staged {
        match data {
            Some(bytes) => writes.push((
                f.to_string(),
                docs.join(f),
                docs.join(format!(".{}{}", f, RESTORE_TMP_SUFFIX)),
                bytes,
            )),
            None => removals.push((f, docs.join(f))),
        }
    }

    // 第 2 步:全部写成临时文件。任一失败:删掉前面已写好的临时文件再返回,目标文件都没动。
    // [复核修 2026-09-15] R6-4 返修:临时文件必须用 write_atomic 写,不能用 Fs::write。临时文件名在 guest 树里
    // 一定不存在,Fs::write → open_with_options 必走「新建文件」分支,宿主 open(O_CREAT) 失败(Preferences 目录
    // 只读、磁盘满 ENOSPC)时 handle_open_err 直接 panic 而不是返回 Err;恢复标记又只在成功时删,
    // 结果是每次离线启动都在这里闪退。write_atomic 在宿主新建/写入失败时返回 Err(FsError::IoError),
    // 并自己删掉内层临时文件 `..<名>.restore.touchhle-tmp.touchhle-tmp`(同样以 .touchhle-tmp 结尾,
    // 崩溃残留由启动建树清理);失败时不插入 guest 节点,所以下面按 is_file 回滚的判断照样成立。
    for (i, (name, _, tmp, bytes)) in writes.iter().enumerate() {
        if let Err(e) = env.fs.write_atomic(tmp, bytes.as_slice()) {
            for (_, _, t, _) in &writes[..=i] {
                if env.fs.is_file(t) {
                    let _ = env.fs.remove(t);
                }
            }
            return Err(format!("写临时文件 {} 失败:{:?}(沙盒未改动)", name, e));
        }
    }

    // 第 3 步:逐个 rename 覆盖目标。Fs::rename 在目标不存在时会先建一个空文件再 rename,
    // 失败时把这个空文件也删掉,免得游戏读到 0 字节存档。
    let mut n = 0usize;
    for (i, (name, target, tmp, _)) in writes.iter().enumerate() {
        let existed = env.fs.is_file(target);
        if let Err(e) = env.fs.rename(tmp, target) {
            if !existed && env.fs.is_file(target) {
                let _ = env.fs.remove(target);
            }
            for (_, _, t, _) in &writes[i..] {
                if env.fs.is_file(t) {
                    let _ = env.fs.remove(t);
                }
            }
            return Err(format!(
                "替换 {} 失败:{:?}(已替换 {} 个文件,沙盒处于半恢复状态)",
                name, e, n
            ));
        }
        n += 1;
    }

    // 删掉快照里没有的已知存档文件。失败只记日志、不算整体失败:删除失败多半是确定性的,
    // 若因此保留恢复标记,每次启动都会把玩家进度再回滚一遍,比留一个较新的文件更糟。
    for (f, gp) in removals {
        if env.fs.is_file(&gp) {
            match env.fs.remove(&gp) {
                Ok(()) => {
                    log!("[MOLEDEV] 快照里没有 {},已从 Documents 删除以对齐快照", f);
                }
                Err(e) => {
                    log!(
                        "[MOLEDEV] 删除 {} 失败:{:?}(它仍是回滚前的版本,与快照不一致)",
                        f,
                        e
                    );
                }
            }
        }
    }
    Ok(n)
}

// ───────────────────────── 建设商店 / 相机回中 ─────────────────────────

/// [扫描修 2026-09-15] F9-2 建设商店:复刻原版单例入口,取代菜单 alloc/init 出第二实例的「易卡」召唤。
/// 核实:-[NewStyleStoreMainLayer gotoBuyMoneyWithIndex:]@0x3af528 的写法是
/// `if (![[[WrapperManager sharedManager] currentUiLayer] getChildByTag:5])
///      [[NewStyleStoreMainLayer sharedInstance] showWithTarget:currentUiLayer selector:@selector(onBuildingSelected)]`;
/// showWithTarget:selector:@0x3aee78 第一道门是 currentGameMode==1(不满足时静默返回),这里先查一遍给出提示。
/// 不写 _defaltStatus、不发 delayToGotoBuyMoney:,停在默认建设页。SEL 参数走 r3 原始指针。
pub fn open_building_store(env: &mut Environment) -> DevResult {
    let wm = singleton(env, "WrapperManager", "sharedManager");
    if wm == nil {
        return Err("WrapperManager 还没初始化".to_string());
    }
    let s = sel(env, "currentGameMode");
    let mode: i32 = msg_send(env, (wm, s));
    if mode != 1 {
        return Err(format!(
            "现在不能打开建设商店(currentGameMode={}),请先关闭其它面板或退出编辑模式",
            mode
        ));
    }
    let s = sel(env, "currentUiLayer");
    let ui: id = msg_send(env, (wm, s));
    if ui == nil {
        return Err("当前没有 UI 层".to_string());
    }
    let s = sel(env, "getChildByTag:");
    let existing: id = msg_send(env, (ui, s, 5i32));
    if existing != nil {
        return Err("商店已经打开了".to_string());
    }
    let store = singleton(env, "NewStyleStoreMainLayer", "sharedInstance");
    if store == nil {
        return Err("NewStyleStoreMainLayer 单例创建失败".to_string());
    }
    let show = sel(env, "showWithTarget:selector:");
    let callback = sel(env, "onBuildingSelected");
    let _: () = msg_send(env, (store, show, ui, callback));
    log!("[MOLEDEV] 打开建设商店 sharedInstance showWithTarget:currentUiLayer selector:onBuildingSelected");
    Ok("已打开建设商店".to_string())
}

/// [扫描修 2026-09-15] F7-12 视角归位:[villageLayer resetPosAndZoom]。
/// 核实:-[VillageLayer resetPosAndZoom]@0x338b8 按 isIpad/extendAllHeight/extendHeight 算回初始 setPosition:,
/// 再 setScale: 并清 isMaxZoomed/isMinZoomed(0x339e8),等于进村时的视角。不做任意 setScale:,
/// 那样会绕过 zoom:touch2: 的范围限制露出地图外黑边。
pub fn camera_center(env: &mut Environment) -> DevResult {
    if crate::mole_cheats::island_session_active() {
        return Err("黄金岛地图层不是 VillageLayer,暂不支持视角归位".to_string());
    }
    let layer = main_village_layer(env);
    if layer == nil {
        return Err("请在主村使用视角归位".to_string());
    }
    let s = sel(env, "resetPosAndZoom");
    let _: () = msg_send(env, (layer, s));
    log!("[MOLEDEV] VillageLayer resetPosAndZoom");
    Ok("视角已回到进村时的位置和缩放".to_string())
}

// ───────────────────────── 选择子跟踪 ─────────────────────────

/// [扫描修 2026-09-15] F7-12 选择子跟踪开关。规则来自环境变量 MOLE_TRACE,只在启动后第一次用到时解析一次。
static TRACE_ON: AtomicBool = AtomicBool::new(false);

enum TraceRule {
    /// "Class":类名完全相等。
    Class(String),
    /// "Class.sel":类名与选择子都完全相等。
    ClassSel(String, String),
    /// "*片段":选择子包含该子串。
    SelContains(String),
}

fn trace_rules() -> &'static [TraceRule] {
    static RULES: OnceLock<Vec<TraceRule>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            let raw = std::env::var("MOLE_TRACE").unwrap_or_default();
            let mut rules = Vec::new();
            for item in raw.split(',') {
                let item = item.trim();
                if item.is_empty() {
                    continue;
                }
                if let Some(sub) = item.strip_prefix('*') {
                    if !sub.is_empty() {
                        rules.push(TraceRule::SelContains(sub.to_string()));
                    }
                } else if let Some((c, s)) = item.split_once('.') {
                    if !c.is_empty() && !s.is_empty() {
                        rules.push(TraceRule::ClassSel(c.to_string(), s.to_string()));
                    }
                } else {
                    rules.push(TraceRule::Class(item.to_string()));
                }
            }
            rules
        })
        .as_slice()
}

/// 选择子跟踪是否开启(objc/messages.rs 每条消息读一次,必须廉价:只读原子变量)。
pub fn trace_on() -> bool {
    TRACE_ON.load(O)
}
/// 跟踪过滤:这条 (类, 选择子) 是否要打印。仅在 trace_on() 为真时调用。只做字符串比较,限流由调用方负责。
pub fn trace_filter_matches(class_name: &str, sel_name: &str) -> bool {
    trace_rules().iter().any(|r| match r {
        TraceRule::Class(c) => c.as_str() == class_name,
        TraceRule::ClassSel(c, s) => c.as_str() == class_name && s.as_str() == sel_name,
        TraceRule::SelContains(sub) => sel_name.contains(sub.as_str()),
    })
}
pub fn toggle_trace() -> DevResult {
    let n = trace_rules().len();
    if n == 0 {
        TRACE_ON.store(false, O);
        return Err(
            "请先设置环境变量 MOLE_TRACE 再启动游戏(逗号分隔:类名 / 类名.选择子 / *选择子片段)"
                .to_string(),
        );
    }
    let now_on = !TRACE_ON.load(O);
    TRACE_ON.store(now_on, O);
    log!("[MOLEDEV] 选择子跟踪 = {}(规则 {} 条)", now_on, n);
    if now_on {
        Ok(format!(
            "选择子跟踪:开(MOLE_TRACE 规则 {} 条,消息会写进日志)",
            n
        ))
    } else {
        Ok("选择子跟踪:关".to_string())
    }
}

// ───────────────────────── 无头文本命令 ─────────────────────────

/// [2026-09-16] A1-04 文本命令的数字参数解析,失败时把原文带进提示。
fn parse_command_number<T: std::str::FromStr>(raw: &str, what: &str) -> Result<T, String> {
    raw.parse::<T>()
        .map_err(|_| format!("{}「{}」不是合法的数字", what, raw))
}

/// [2026-09-16] A1-04 无头文本命令台:执行命令文件(mole_diag::next_inject)里的一行开发命令。
/// 根因:回归脚本原来只能按菜单格子坐标点开发工具、隐藏物品、任务跳转,要开菜单、翻页、输寄存器好几步;
/// 菜单加页、宽屏 --fill-screen 多出水平偏移,坐标就失效。这里按名字直接调菜单按钮背后的同一批函数:
///   dev fps | dev grid | dev center | dev speed <倍率> → toggle_fps / toggle_map_grid / camera_center / set_time_scale
///   dev trace | dev unlock | dev store | dev weather <类型> → toggle_trace / unlock_interaction / open_building_store / weather
///   quest main|time|vip|island <任务号>                → quest_jump
///   story <段号>                                       → story_play
///   time <分钟>                                        → apply_time_minutes
///   give <物品ID>                                      → mole_items::place_item(与召唤页、隐藏物品页同一入口)
/// 在线模式、场景、数值范围的拒绝都由这些函数自己给出,与菜单点按钮完全一致,这里不另加门。
/// 刻意不开放时间旅行、快照恢复、删档:菜单上它们要二次确认,脚本一行就触发太危险。
/// `menu <页名>`:按页名打开菜单要 mole_menu 提供翻页接口(当前页是它的私有状态),那不归本包,先明确报错;
/// 不带参数的 `menu` 仍由 mole_diag 当开关处理。
/// 调用上下文:frameworks/uikit.rs handle_events 的注入分派点,与菜单 handle_touch 相同,可以发宿主 msg_send。
pub fn run_text_command(env: &mut Environment, line: &str) -> DevResult {
    let mut words = line.split_whitespace();
    let head = words.next().unwrap_or("");
    let args: Vec<&str> = words.collect();
    match head {
        "dev" => match args.as_slice() {
            ["fps"] => toggle_fps(env),
            ["grid"] => toggle_map_grid(env),
            ["center"] => camera_center(env),
            ["speed", x] => {
                let scale: f32 = parse_command_number(x, "倍率")?;
                set_time_scale(env, scale)
            }
            ["trace"] => toggle_trace(),
            ["unlock"] => unlock_interaction(env),
            ["store"] => open_building_store(env),
            ["weather", k] => {
                let kind: i64 = parse_command_number(k, "天气类型")?;
                weather(env, kind)
            }
            _ => Err("用法:dev fps | dev grid | dev center | dev speed <0.25..4> | dev trace | dev unlock | dev store | dev weather <类型>"
                .to_string()),
        },
        "quest" => match args.as_slice() {
            [family, n] => {
                let family = match *family {
                    "main" => QuestFamily::Main,
                    "time" => QuestFamily::Time,
                    "vip" => QuestFamily::Vip,
                    "island" => QuestFamily::Island,
                    other => {
                        return Err(format!(
                            "任务族「{}」不认识,只能是 main / time / vip / island",
                            other
                        ));
                    }
                };
                let quest_id: i64 = parse_command_number(n, "任务号")?;
                quest_jump(env, family, quest_id)
            }
            _ => Err("用法:quest main|time|vip|island <任务号>".to_string()),
        },
        "story" => match args.as_slice() {
            [n] => {
                let section: i64 = parse_command_number(n, "剧情段号")?;
                story_play(env, section)
            }
            _ => Err("用法:story <段号>".to_string()),
        },
        "time" => match args.as_slice() {
            [m] => {
                let minutes: i64 = parse_command_number(m, "分钟数")?;
                apply_time_minutes(env, minutes)
            }
            _ => Err(format!("用法:time <分钟>(1..{})", TIME_SKIP_MAX_MINUTES)),
        },
        "give" => match args.as_slice() {
            [id] => {
                let item: u32 = parse_command_number(id, "物品 ID")?;
                crate::mole_items::place_item(env, item)
            }
            _ => Err("用法:give <物品ID>".to_string()),
        },
        "menu" => Err(format!(
            "暂不支持按页名打开菜单(「{}」):mole_menu 还没有翻页接口,请用不带参数的 menu 开关菜单",
            args.join(" ")
        )),
        _ => Err(format!(
            "无法识别的命令「{}」,支持 tap / drag / menu / suspend / dev / quest / story / time / give",
            head
        )),
    }
}
