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
            // [2026-10-04 第八轮 R8-D1] 快照写回后,主档的上一代备份已不是它的上一代,清掉(见 mole_savebak)。
            crate::mole_savebak::forget_backups(env);
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

/// [2026-09-24 第四轮 K4 I4-05] 岛档计时快进:开发工具页「岛档快进」按钮(分钟取数值寄存器)与文本命令 `island ff <分钟>`。
/// 根因:上面的「对象计时快进」在岛上被拒(岛上对象计时走 NewSceneTimer,updateTime 的效果未核实),岛上的售卖/升级/出海/
///   修船/公寓/打工任务都没法无头验证;直接改岛上活对象又会被离岛回写覆盖。所以只在主村离线时,把【盘上】岛档里的绝对时间
///   往回拨 N 分钟(等价于这段时间已经流逝),下次进岛由原版计时逻辑自己判「已完成」。具体规则、前置与快照见
///   mole_cheats::island_ff_offline;这里只做入口校验。范围与对象计时快进同为 1..30 天。
pub fn island_fast_forward_minutes(env: &mut Environment, minutes: i64) -> DevResult {
    if env.options.network_access {
        return Err("在线模式下岛上进度以服务器为准,不能快进岛档".to_string());
    }
    // [2026-09-25 第五轮遗留 C] 时间旅行中不快进岛档:旅行期间进不了岛(mole_cheats 的 enterNewIslands 臂拦下),快进的效果要重启、
    //   偏移归零后才看得到;而且 island_storage_ff 在仓库档缺 savedAt 时回退用 now_cf_secs(),它含旅行偏移(wall_cf_secs 加了
    //   time_offset_secs),会把未来的 savedAt 写进 island_storage.dat。重启后再快进效果相同,所以直接拒绝。
    //   (回拨前自动拍的快照在旅行中同样是「旅行后的主档 + 旅行前的侧档」,与手动快照一样,不是拒绝的主因。)
    if crate::libc::time::time_offset_secs() != 0 {
        return Err(
            "时间旅行中不能快进岛档(旅行期间进不了岛,快进效果要重启后才看得到),请重启游戏回到现实时间后再快进".to_string(),
        );
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
        return Err(
            "黄金岛上不能快进岛档(离岛时岛上内存会覆盖改动),请回主村执行,下次进岛生效".to_string(),
        );
    }
    if main_village_layer(env) == nil {
        // 标题画面主档还没读进来,出海冷却写回主档时 saveUserInfoData 可能把空档写回去,所以要求先进主村。
        return Err("请先进入主村再快进岛档".to_string());
    }
    let r = crate::mole_cheats::island_ff_offline(env, (minutes * 60) as f64);
    match &r {
        Ok(text) => {
            log!("[MOLEDEV] 岛档快进 {} 分钟:{}", minutes, text);
        }
        Err(e) => {
            log!("[MOLEDEV] 岛档快进 {} 分钟失败:{}", minutes, e);
        }
    }
    r
}

// ───────────────────────── 主村工人/房间重算 ─────────────────────────
//
// [2026-09-25 第五轮遗留 WK99] 旧版「工人房间补满」写进 userinfo.dat 的 totalWorkers/totalRooms = 99 一键还原。
// 原版主村两个数的来源(full.asm 逐条核过):
//   · 新号:-[UserInfoData init]@0xb9050 在 0xb91da 把 3 写进 totalWorkers_(+36)/availableWorkers_(+40),0xb9150 把 1 写进
//     totalRooms_(+48);默认地图 createDefaultMapData@0x79418 放一座 5001 红色尖顶房(state 6 = 人口 3)。
//   · totalWorkers 唯一增长口 -[UserInfoData addWorker:]@0xbb2a4(总数、空闲同加 n,0xbb316 刷抬头,0xbb344 initMoleActors:n 生成
//     空闲摩尔),主村调用点:房屋建成 -[Building onFinishHandler] 0xb11d0(type 5、非仓库摆放模式 7、objectId≠60004 银行)、
//     房屋升级 -[Building upgrade] 0xb0baa(非模式 7、type≠6)、买「摩尔」19001 -[VillageMenuLayer addNewObject2Map:gift:] 0x65728。
//     仓库存取(gameMode 7)两头都不动它,所以仓库里房子的人口一直算在总数里;全程没有减少路径。
//   · 原版自带居民房人口 -[GameData getWorkerCountByRoom]@0x7dc00:地图上 [ObjectManager rooms](只收 type 5)按 buildingState
//     4/5/6 计 1/2/3(0x7dcc2/0x7dcd2/0x7dce2),再加仓库 recycledHouses 每个「id_等级」键的 等级 × 个数(0x7de06 mla)。
//   ⇒ 原版恒等式:totalWorkers = getWorkerCountByRoom − 已建成银行数 + 额外摩尔(买来的;锁 9 拿含银行的原始
//     getWorkerCountByRoom 比,额外摩尔封顶 110 + 已建成银行数)。额外摩尔除了总数本身存档里没有任何记录,只能由玩家自己报(寄存器)。
//     [复核补] 原版唯一会让总摩尔少于居民房人口的是竞态:onFinishHandler 在 0xb1060 取的是完工那一刻的 currentGameMode,
//     0xb118a 见 7 就跳过 addWorker:,所以仓库摆放(gameMode 7,0x62bb2/0x660f2)期间恰好有别的房屋完工(Building innerupdate:
//     0xaf60a 按帧触发,不看模式)会少给 1 人;正常流程不会出现。本工具按恒等式把它补回,预览里照实说明。
//   · totalRooms 唯一增长口 -[UserInfoData addRoom:]@0xbb548,只在 Porter 新摆一座房屋(含从仓库取出)时由
//     -[Building initWithTile:sprite:size:data:] blx 0xad860 调(另一处 initWithTile:sprite:size:data:isWay: 的 blx 0xada9c
//     所在方法没有任何 selref 引用,不会被调用);读它的 selref 只有 intiWithUserInfo:/encodeWithCoder:/
//     encodeUserInfoData 三处复制/编码,没有玩法读它。原值 = 1 + 历次摆放次数,推不出,只能还原到下界。
// 所以只在存档确实不符合原版时才改:三项同写 99 的旧作弊指纹(总摩尔与房间都 ≥ 99),或总摩尔少于居民房人口;
// 正常档(包括合法 ≥ 99 的重度玩家档,只要房间 < 99)不动,寄存器残留值也不碰。写回走原版路径:增加用原版 addWorker:,
// 减少(原版没有减少路径)用 setter,最后 saveUserInfoData 让游戏自己存主档,改前先自动存快照。
// 调用上下文:菜单 handle_touch / 文本命令台(frameworks/uikit.rs handle_events 运行循环顶部),可以发宿主消息;不在帧栈上。

/// 原版买「摩尔」(19001,type 0x13)的上限:-[GameData getLockType4Object:] 在 0x7d398..0x7d3d6 对 type 0x13 算
/// totalWorkers − getWorkerCountByRoom,> 0x6d 给锁 9 → 最多 110 个。只用于提示文案。
/// [复核补] 锁 9 用的是原始 getWorkerCountByRoom(含已建成银行,0x7d3cc),而本工具的额外摩尔按扣掉银行的居民房人口算,
/// 所以按本工具口径原版上限是 110 + 已建成银行数(见 WorkerRecalcPlan::orig_extra_max)。
const EXTRA_MOLES_ORIG_MAX: i32 = 110;
/// 寄存器里「额外摩尔数」允许的上限。开着「全物品解锁」(mole_cheats 让 getLockType4Object: 恒返回 0)或「工人补满」
/// (锁 9 那条 blx 的返回地址 0x7d3bd 在 K13 白名单里读到 99,99 − 居民房人口 永远不大于 109)时买的摩尔可以超过 110,
/// 要能原样保住,所以放宽到 999。
const EXTRA_MOLES_INPUT_MAX: i64 = 999;
/// 旧版「工人房间补满」(c01007a 之前)三个 getter 对所有调用者恒返回 99,经 -[UserInfoData encodeWithCoder:]
/// 0xba0e2/0xba108/0xba17a 把 99 同时写进总摩尔、空闲、房间;读档后两数只增不减,所以被污染的档一定两项都 ≥ 99。
const OLD_MAXFAC_VALUE: i32 = 99;
/// 中信虚拟银行(60004,type 5,store_able 1):-[Building onFinishHandler] 在 0xb11a2 `movw r1,#0xea64` 比 objectId,
/// 相等时跳过 0xb11d0 的 addWorker:1(建成不给工人),但 getWorkerCountByRoom 照样把它算进居民房人口。
const BANK_OBJECT_ID: i32 = 60004;
/// 开地上限锁 7 的地块:-[GameData getLockType4Object:] 在 0x7da5c..0x7daa4 对 1001/2001/1010 各发 objectCount:type:2 求和,
/// 0x7dac6 与 totalWorkers 比,地块数 ≥ 总摩尔数就锁(-[VillageMenuLayer canBuyMultiple:] 0x64500.. 同口径)。
const PLOT_OBJECT_IDS: [i32; 3] = [1001, 2001, 1010];

/// [SceneMannager curSceneId](i8@0:4):1 = 主村,10 = 黄金岛,2 = 切场景过场;单例拿不到时 -1。
fn scene_id(env: &mut Environment) -> i32 {
    let sm = singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return -1;
    }
    let s = sel(env, "curSceneId");
    msg_send(env, (sm, s))
}

/// [WrapperManager currentGameMode](i8@0:4);单例拿不到时 -1。
fn wrapper_game_mode(env: &mut Environment) -> i32 {
    let wm = singleton(env, "WrapperManager", "sharedManager");
    if wm == nil {
        return -1;
    }
    let s = sel(env, "currentGameMode");
    msg_send(env, (wm, s))
}

/// 主村地图是否还在加载:[[ActorManager Instance] m_isLoadMap](B8@0:4,ivar +260 槽 0xb03e54)。
/// 原版 -[GameManager loadMapFromData:forNPC:] 0x206e6 / loadMapFromData:selector:mapData:forNPC: 0x20b6c 置 1,
/// endLoadMap 以 1 秒间隔调度的 -[GameManager createIdleWorkers:] 先对各任务 minusNeededWorkers 扣预留,再在 0x1c0de 清 0。
/// 这段时间 rooms 和仓库只加载了一部分(getWorkerCountByRoom 偏小),空闲数也还没扣任务预留;而且
/// -[FriendsVillageLayer goToHomeVillage] 先在 0x108d10 setGameMode:1 才回家加载,gameMode 门挡不住。
/// 原版 -[GameData saveMapData:] 0x768f4/0x768fa、-[VillageMenuLayer onButtonFriendSelected:] 0x61624 都拿它当「加载中」门。
/// 单例为 nil 按「加载中」算。
fn main_map_loading(env: &mut Environment) -> bool {
    let am = singleton(env, "ActorManager", "Instance");
    if am == nil {
        return true;
    }
    let s = sel(env, "m_isLoadMap");
    let loading: bool = msg_send(env, (am, s));
    loading
}

/// 只改主村主档的开发工具共用的门:离线、不在岛会话、主村层已挂上、curSceneId==1、地图加载完、currentGameMode==1。
/// gameMode 门同时挡住串门(-[FriendsVillageLayer init] 0x100392 等处 setGameMode:0)、移动/编辑(-[MoveLayer showWithTarget:selector:] 0xad09c 模式 2)、
/// 仓库摆放(模式 7,rooms_ 可能暂时少一座房)、好友礼物(模式 9)等。
fn main_village_offline_gate(env: &mut Environment, what: &str) -> Result<(), String> {
    if env.options.network_access {
        return Err(format!("在线模式下存档以服务器为准,不能{}", what));
    }
    if crate::mole_cheats::island_session_active() {
        return Err(format!("黄金岛上不能{}(只改主村),请回主村再用", what));
    }
    if main_village_layer(env) == nil {
        return Err(format!(
            "请先进入主村再{}(标题画面、过场或串门时主村房屋没加载全)",
            what
        ));
    }
    let scene = scene_id(env);
    if scene != 1 {
        return Err(format!(
            "当前不在主村(curSceneId={}),请等进村完成再{}",
            scene, what
        ));
    }
    if main_map_loading(env) {
        return Err(format!(
            "主村地图还在加载(原版此时也不存地图,空闲摩尔要加载完 1 秒后才生成),稍等几秒再{}",
            what
        ));
    }
    let mode = wrapper_game_mode(env);
    if mode != 1 {
        return Err(format!(
            "请先关闭其它面板、退出编辑/摆放模式再{}(currentGameMode={})",
            what, mode
        ));
    }
    Ok(())
}

/// 重算计划(只读,不写)。菜单二次确认的预览、文本命令的预览和真正执行都从这里来,口径一致。
pub struct WorkerRecalcPlan {
    pub total_old: i32,
    pub avail_old: i32,
    pub rooms_old: i32,
    /// 原版 [GameData getWorkerCountByRoom] 的返回值(含银行)。
    pub by_room_raw: i32,
    /// 已建成的中信银行座数(地图 buildingState 4..=6 + 仓库「60004_等级」键);原版建成不给工人,每座比居民房人口少 1。
    pub bank_pop: i32,
    /// 居民房人口 = by_room_raw − bank_pop(不小于 0)。
    pub by_room: i32,
    pub houses_map: i32,
    pub houses_store: i32,
    /// 开地上限锁 7 计数的地块数(1001/2001/1010 的 objectCount:type:2 之和)。
    pub plots: i32,
    /// 额外摩尔数:要改工人时 = 寄存器输入;不改时 = 现状 total_old − by_room,只用于显示。
    pub extra: i64,
    /// 旧版 99 指纹:总摩尔、房间都 ≥ 99。
    pub polluted: bool,
    /// 总摩尔少于居民房人口(原版正常流程不会出现,只有仓库摆放期间恰好有房屋完工的竞态会少给,见本节开头)。
    pub below_resident: bool,
    /// 文本命令 force:跳过判定强制按 居民房人口 + 额外摩尔 重算。
    pub forced: bool,
    pub total_new: i32,
    pub avail_new: i32,
    pub rooms_new: i32,
}

impl WorkerRecalcPlan {
    pub fn changes(&self) -> bool {
        self.total_new != self.total_old
            || self.avail_new != self.avail_old
            || self.rooms_new != self.rooms_old
    }

    fn workers_fix(&self) -> bool {
        self.forced || self.polluted || self.below_resident
    }

    /// [复核补] 按本工具口径(居民房人口已扣银行)的原版额外摩尔上限:锁 9 拿含银行的原始 getWorkerCountByRoom 比(0x7d3cc),
    /// 所以是 110 + 已建成银行数。
    fn orig_extra_max(&self) -> i64 {
        EXTRA_MOLES_ORIG_MAX as i64 + self.bank_pop.max(0) as i64
    }

    fn resident_text(&self) -> String {
        if self.bank_pop > 0 {
            format!(
                "居民房人口 {}(原版 getWorkerCountByRoom {} 已扣除中信银行 {} 座:原版建成银行不给工人)",
                self.by_room, self.by_room_raw, self.bank_pop
            )
        } else {
            format!("居民房人口 {}", self.by_room)
        }
    }

    fn houses_text(&self) -> String {
        format!(
            "现有房屋 {}:地图 {} + 仓库 {}",
            self.houses_map + self.houses_store,
            self.houses_map,
            self.houses_store
        )
    }

    pub fn describe(&self) -> String {
        if !self.changes() {
            let extra_now = self.total_old - self.by_room;
            let mut notes: Vec<String> = Vec::new();
            if extra_now as i64 > self.orig_extra_max() {
                notes.push(format!(
                    "额外摩尔超过原版购买上限 {},多半是开着「工人补满」或旧版「全物品解锁」(现「解除购买门槛」)时买的",
                    self.orig_extra_max()
                ));
            }
            if self.total_old >= OLD_MAXFAC_VALUE && self.rooms_old < OLD_MAXFAC_VALUE {
                notes.push("总摩尔 ≥99 但房间 <99,不像旧版「工人房间补满」三项同写 99 的残留(确是旧版残留又按过「房间数 = 20」的,可用文本命令 workers recalc apply force [额外摩尔数])".to_string());
            }
            let why = if notes.is_empty() {
                "都在原版范围内".to_string()
            } else {
                format!("{},不改", notes.join(";"))
            };
            return format!(
                "总摩尔 {} = {} + 额外摩尔(买来的,不占住房){},空闲 {},房间 {}({}),{}",
                self.total_old,
                self.resident_text(),
                extra_now,
                self.avail_old,
                self.rooms_old,
                self.houses_text(),
                why
            );
        }
        let mut parts: Vec<String> = Vec::new();
        if self.workers_fix() {
            let why = if self.forced {
                "文本命令强制重算"
            } else if self.polluted {
                "总摩尔与房间都 ≥99,是旧版「工人房间补满」写进存档的 99 残留"
            } else {
                "总摩尔少于居民房人口,原版正常流程不会出现(仓库摆放时恰好有房屋完工会少给,或改过数值)"
            };
            parts.push(format!("判定:{}", why));
        }
        if self.total_new != self.total_old {
            let mut s = format!(
                "总摩尔 {}→{}({} + 额外摩尔 {};额外摩尔(买来的,不占住房)存档里没有记录,按寄存器算",
                self.total_old,
                self.total_new,
                self.resident_text(),
                self.extra
            );
            if self.extra > self.orig_extra_max() {
                s.push_str(&format!(
                    ";超过原版购买上限 {},只有开着「工人补满」或旧版「全物品解锁」(现「解除购买门槛」)时买才可能",
                    self.orig_extra_max()
                ));
            }
            let keep = (self.total_old - self.by_room) as i64;
            if self.total_old >= self.by_room && keep <= EXTRA_MOLES_INPUT_MAX && keep != self.extra
            {
                s.push_str(&format!(
                    ";若 {} 本来就对,把寄存器设为 {} 即保持不变",
                    self.total_old, keep
                ));
            }
            s.push(')');
            parts.push(s);
        }
        if self.avail_new != self.avail_old {
            parts.push(format!("空闲 {}→{}", self.avail_old, self.avail_new));
        }
        if self.rooms_new != self.rooms_old {
            parts.push(format!(
                "房间 {}→{}({};原版房间数 = 1 + 历次摆放房屋次数,推不出原值,只还原到下界,没有玩法读它)",
                self.rooms_old,
                self.rooms_new,
                self.houses_text()
            ));
        }
        if self.total_new < self.total_old && self.plots >= self.total_new {
            parts.push(format!(
                "现有田地/池塘等地块 {} 块 ≥ 新总摩尔数(原版开地上限 = 总摩尔数):已开的不拆,但要等总摩尔数超过 {} 才能再开新地",
                self.plots, self.plots
            ));
        }
        parts.join(",")
    }

    /// 二次确认码用的摘要:两次点击之间数值一变(房子刚好建成、做了仓库操作、改了寄存器),确认码就不同,会重新提示。
    pub fn digest(&self) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for v in [
            self.total_old,
            self.avail_old,
            self.rooms_old,
            self.total_new,
            self.avail_new,
            self.rooms_new,
        ] {
            h = (h ^ (v as u32)).wrapping_mul(0x0100_0193);
        }
        h % 100_000_000
    }
}

/// 算重算计划(只读)。extra_in = 额外摩尔数(菜单取寄存器);只有存档确实要改工人时才校验和使用它,
/// 正常档完全忽略寄存器(寄存器是整页共享的,任务跳转、快进留下的数不能影响这里)。force 只给文本命令用。
pub fn plan_worker_recalc(
    env: &mut Environment,
    extra_in: i64,
    force: bool,
) -> Result<WorkerRecalcPlan, String> {
    main_village_offline_gate(env, "重算工人/房间")?;
    let ui = user_info_data(env);
    let gd = singleton(env, "GameData", "sharedInstance");
    let om = singleton(env, "ObjectManager", "sharedManager");
    if ui == nil || gd == nil || om == nil {
        return Err("主村存档对象还没准备好(userInfoData/GameData/ObjectManager 为空)".to_string());
    }
    // 宿主 msg_send 的返回地址不在 K13 的 MAXFAC_GATE_LRS 白名单里,「工人补满」开着也读到存档真值。
    let s = sel(env, "totalWorkers");
    let total_old: i32 = msg_send(env, (ui, s));
    let s = sel(env, "availableWorkers");
    let avail_old: i32 = msg_send(env, (ui, s));
    let s = sel(env, "totalRooms");
    let rooms_old: i32 = msg_send(env, (ui, s));

    // 居民房人口:直接调原版函数,由它自己遍历 rooms 与仓库。
    let s = sel(env, "getWorkerCountByRoom");
    let by_room_raw: i32 = msg_send(env, (gd, s));

    // 地图房屋数 + 地图上已建成的银行。
    let s_count = sel(env, "count");
    let s_oai = sel(env, "objectAtIndex:");
    let mut bank_pop: i32 = 0;
    let s = sel(env, "rooms");
    let rooms: id = msg_send(env, (om, s));
    let houses_map: i32 = if rooms == nil {
        0
    } else {
        let n: u32 = msg_send(env, (rooms, s_count));
        let s_data = sel(env, "data");
        let s_oid = sel(env, "objectId");
        let s_state = sel(env, "buildingState");
        for i in 0..n {
            let b: id = msg_send(env, (rooms, s_oai, i));
            if b == nil {
                continue;
            }
            let d: id = msg_send(env, (b, s_data));
            if d == nil {
                continue;
            }
            let oid: i32 = msg_send(env, (d, s_oid));
            if oid != BANK_OBJECT_ID {
                continue;
            }
            // getWorkerCountByRoom 只计 buildingState 4/5/6(已建成);在建的银行两边都不计。
            let st: i32 = msg_send(env, (b, s_state));
            if (4..=6).contains(&st) {
                bank_pop += 1;
            }
        }
        n.min(i32::MAX as u32) as i32
    };
    // 仓库里的银行:键格式与 getWorkerCountByRoom 0x7ddd4 起的拆法一致(componentsSeparatedByString:@"_",[0]=物品号、[1]=等级),
    // 值是个数(intValue)。每座只扣 1:银行建成不给工人,之后若有升级,-[Building upgrade] 0xb0baa 照常 addWorker:1,
    // 所以不论等级,每座银行的居民房人口都恰好比它给总摩尔数的贡献多 1。
    let s = sel(env, "recycledHouses");
    let store: id = msg_send(env, (om, s));
    if store != nil {
        let s = sel(env, "allKeys");
        let keys: id = msg_send(env, (store, s));
        if keys != nil {
            let n: u32 = msg_send(env, (keys, s_count));
            let s_ofk = sel(env, "objectForKey:");
            let s_int = sel(env, "intValue");
            for i in 0..n {
                let k: id = msg_send(env, (keys, s_oai, i));
                if !is_kind_of(env, k, "NSString") {
                    continue;
                }
                let key =
                    crate::frameworks::foundation::ns_string::to_rust_string(env, k).into_owned();
                let mut it = key.split('_');
                let oid = it.next().and_then(|x| x.parse::<i32>().ok()).unwrap_or(0);
                let level = it.next().and_then(|x| x.parse::<i32>().ok()).unwrap_or(0);
                if oid != BANK_OBJECT_ID || level < 1 {
                    continue;
                }
                let v: id = msg_send(env, (store, s_ofk, k));
                let c: i32 = if v == nil {
                    0
                } else {
                    msg_send(env, (v, s_int))
                };
                bank_pop += c.max(0);
            }
        }
    }
    let by_room = (by_room_raw - bank_pop).max(0);
    let s = sel(env, "houseNumberInRecycler");
    let houses_store: i32 = msg_send(env, (om, s));
    let houses = houses_map + houses_store.max(0);

    // 开地上限锁 7 的地块数(只用于提示)。objectCount:type: 签名 i16@0:4i8i12。
    let s = sel(env, "objectCount:type:");
    let mut plots: i32 = 0;
    for pid in PLOT_OBJECT_IDS {
        let c: i32 = msg_send(env, (om, s, pid, 2i32));
        plots += c.max(0);
    }

    let polluted = total_old >= OLD_MAXFAC_VALUE && rooms_old >= OLD_MAXFAC_VALUE;
    let below_resident = total_old < by_room;
    let workers_fix = force || polluted || below_resident;
    let (extra, total_new) = if workers_fix {
        if !(0..=EXTRA_MOLES_INPUT_MAX).contains(&extra_in) {
            return Err(format!(
                "额外摩尔数(寄存器)要在 0..={} 之间(原版最多买 {} 个),当前是 {};先按「清零」再输入",
                EXTRA_MOLES_INPUT_MAX, EXTRA_MOLES_ORIG_MAX, extra_in
            ));
        }
        (extra_in, by_room + extra_in as i32)
    } else {
        ((total_old - by_room) as i64, total_old)
    };
    let avail_new = if total_new > total_old {
        // 与原版 addWorker: 同口径:空闲同加增量。工人补满开过后空闲可能为负,夹回 0..=新总数。
        (avail_old + (total_new - total_old)).clamp(0, total_new)
    } else if total_new < total_old {
        // 本局在忙的(派工中、任务预留)照旧算占用;归还时 addAvailableWorker:@0xbb34c 会夹在新总数以内。
        // 存盘的空闲数下次读档会被 intiWithUserInfo: 0xb96bc 重置为总数,再由 createIdleWorkers: 扣任务预留,与原版口径一致。
        let occupied = (total_old - avail_old).clamp(0, total_old.max(0));
        (total_new - occupied).clamp(0, total_new)
    } else {
        avail_old
    };
    // 房间:旧版指纹 → 真值 ≥ 现有房屋,且污染后每摆放一次 +1,真值 = 原值 + (rooms_old − 99) ≥ rooms_old − 98;
    // 否则只在少于现有房屋(原版不可能:房屋不能销毁,每摆一座 +1)时补到现有房屋数。
    let rooms_new = if polluted {
        houses.max(rooms_old - (OLD_MAXFAC_VALUE - 1))
    } else if rooms_old < houses {
        houses
    } else {
        rooms_old
    };
    Ok(WorkerRecalcPlan {
        total_old,
        avail_old,
        rooms_old,
        by_room_raw,
        bank_pop,
        by_room,
        houses_map,
        houses_store,
        plots,
        extra,
        polluted,
        below_resident,
        forced: force,
        total_new,
        avail_new,
        rooms_new,
    })
}

/// 按存档重算主村工人/房间并存主档(开发工具页「重算工人/房间」第二次点击 / 文本命令 `workers recalc apply`)。
pub fn recalc_workers(env: &mut Environment, extra: i64, force: bool) -> DevResult {
    let p = plan_worker_recalc(env, extra, force)?;
    if !p.changes() {
        log!("[MOLEDEV] 工人/房间重算:无需改动,{}", p.describe());
        return Ok(format!("存档正常,无需重算(未改动存档):{}", p.describe()));
    }
    let snap =
        snapshot_save(env).map_err(|e| format!("重算前保存快照失败:{},为安全起见没有改动", e))?;
    let ui = user_info_data(env);
    if ui == nil {
        return Err("主村 userInfoData 为空,没有改动".to_string());
    }
    let mole_note = if p.total_new > p.total_old {
        // 原版唯一的增长路径:总数、空闲同加,0xbb2cc 记 updateTime_,0xbb316 刷抬头,0xbb344 initMoleActors: 当场生成空闲摩尔。
        let delta = p.total_new - p.total_old;
        let s = sel(env, "addWorker:");
        let _: () = msg_send(env, (ui, s, delta));
        let s = sel(env, "availableWorkers");
        let a: i32 = msg_send(env, (ui, s));
        if a < 0 || a > p.total_new {
            // 兜底:工人补满开过后空闲数可能本来就是负的,加完仍不在 0..=新总数。
            let s = sel(env, "setAvailableWorkers:");
            let _: () = msg_send(env, (ui, s, p.avail_new));
        }
        format!("按原版 addWorker: 当场刷出 {} 只空闲摩尔", delta)
    } else if p.total_new < p.total_old {
        // 原版没有减少路径,只能用 setter;多出来的空闲摩尔本局还在村里走,下次进主村由 createIdleWorkers: 按新空闲数生成。
        let s = sel(env, "setTotalWorkers:");
        let _: () = msg_send(env, (ui, s, p.total_new));
        let s = sel(env, "setAvailableWorkers:");
        let _: () = msg_send(env, (ui, s, p.avail_new));
        "村里多出的空闲摩尔下次进主村或重启后按新数量生成".to_string()
    } else {
        "总摩尔不变".to_string()
    };
    if p.rooms_new != p.rooms_old {
        let s = sel(env, "setTotalRooms:");
        let _: () = msg_send(env, (ui, s, p.rooms_new));
    }
    // 与买摩尔 0x65766、addVipGold: 同一条原版写回路径。
    game_data_call(env, "saveUserInfoData");
    // 照原版 addWorker: 0xbb304..0xbb316 刷抬头(增加时 addWorker: 已刷过,再刷一次无害)。
    let wm = singleton(env, "WrapperManager", "sharedManager");
    if wm != nil {
        let s = sel(env, "updateUserInfoView:");
        let _: () = msg_send(env, (wm, s, 3i32));
    }
    log!(
        "[MOLEDEV] 工人/房间重算:总摩尔 {}→{}(居民房人口 {} = 原版 {} − 银行 {},额外摩尔 {}),空闲 {}→{},房间 {}→{}(地图 {} + 仓库 {}),地块 {},污染={} 低于居民房={} 强制={};{};重算前{}",
        p.total_old,
        p.total_new,
        p.by_room,
        p.by_room_raw,
        p.bank_pop,
        p.extra,
        p.avail_old,
        p.avail_new,
        p.rooms_old,
        p.rooms_new,
        p.houses_map,
        p.houses_store,
        p.plots,
        p.polluted,
        p.below_resident,
        p.forced,
        mole_note,
        snap
    );
    let max_fac_note = if crate::mole_cheats::is_on("max_facility") {
        ";「工人补满」还开着,抬头仍显示 99"
    } else {
        ""
    };
    Ok(format!(
        "已重算并存档:{}。{}{};重算前{}(可用「快照:下次启动恢复」回滚)",
        p.describe(),
        mole_note,
        max_fac_note,
        snap
    ))
}

/// 测试专用(只开放给文本命令 `workers set`,不上菜单):直接写总摩尔/房间,空闲按读档口径 = 总数,用来无头造出旧版 99 档。
/// 主档带 md5 尾,nska.py 改不回去;写法与菜单「工人数 = 20」「房间数 = 20」相同,门控与重算相同。
pub fn set_workers_raw(env: &mut Environment, total: i64, rooms: i64) -> DevResult {
    main_village_offline_gate(env, "设工人/房间")?;
    if !(0..=999).contains(&total) || !(0..=999).contains(&rooms) {
        return Err(format!(
            "总摩尔与房间都要在 0..=999 之间(当前 {} / {})",
            total, rooms
        ));
    }
    let ui = user_info_data(env);
    if ui == nil {
        return Err("主村 userInfoData 为空,没有改动".to_string());
    }
    let s = sel(env, "totalWorkers");
    let t_old: i32 = msg_send(env, (ui, s));
    let s = sel(env, "totalRooms");
    let r_old: i32 = msg_send(env, (ui, s));
    let (t, r) = (total as i32, rooms as i32);
    let s = sel(env, "setTotalWorkers:");
    let _: () = msg_send(env, (ui, s, t));
    let s = sel(env, "setAvailableWorkers:");
    let _: () = msg_send(env, (ui, s, t));
    let s = sel(env, "setTotalRooms:");
    let _: () = msg_send(env, (ui, s, r));
    game_data_call(env, "saveUserInfoData");
    let wm = singleton(env, "WrapperManager", "sharedManager");
    if wm != nil {
        let s = sel(env, "updateUserInfoView:");
        let _: () = msg_send(env, (wm, s, 3i32));
    }
    log!(
        "[MOLEDEV] 工人/房间直接设值(测试用):总摩尔 {}→{},空闲 → {},房间 {}→{}",
        t_old,
        t,
        t,
        r_old,
        r
    );
    Ok(format!(
        "已直接设值并存档(测试用):总摩尔 {}→{}(空闲同为 {}),房间 {}→{}",
        t_old, t, t, r_old, r
    ))
}

// ───────────────────────── [2026-10-03] 按经验值重算等级 ─────────────────────────

/// UserInfoData.curLevel_(i,编译期 +16)的 _OBJC_IVAR 槽。存的是 +[CryptUtils encryptInt:]@0x124b28 的密文(XOR 0x01011011),
/// getter -[UserInfoData curLevel]@0xbaffc 读槽后 decryptInt: 解密;读档 -[UserInfoData initWithCoder:] 0xb9c50..0xb9c82 是
/// decodeIntForKey:@"curLevel" → encryptInt: → setNewCurLevel:(纯赋值 0xbc184)。
const UI_CUR_LEVEL_SLOT: u32 = 0xb03ff8;
const UI_CUR_LEVEL_OFF: u32 = 16;
const CUR_LEVEL_XOR: u32 = 0x0101_1011;

/// 按经验值重算等级的计划(只读算出,recalc_level 才写)。
pub struct LevelRecalcPlan {
    /// 存档里的真实等级(密文 ivar 解出,不受「等级=N」覆盖)。
    pub level_old: i32,
    pub xp: i32,
    /// 按原版 checkUpgrade 口径从经验值推出的等级。
    pub level_new: i32,
    /// 等级表级数(114_0.dat,5.5.0 为 52)。
    pub max_level: i32,
    /// level_new 这一级的门槛「level<N>」(经验值小于它才停在这一级);经验值超过整张表时为 None。
    pub need_new: Option<u32>,
}

impl LevelRecalcPlan {
    pub fn changes(&self) -> bool {
        self.level_new != self.level_old
    }

    pub fn describe(&self) -> String {
        let basis = match self.need_new {
            Some(need) => format!(
                "经验 {} 小于 level{} 门槛 {}",
                self.xp, self.level_new, need
            ),
            None => format!("经验 {} 超过整张等级表(共 {} 级)", self.xp, self.max_level),
        };
        if self.changes() {
            format!("等级 {}→{}({})", self.level_old, self.level_new, basis)
        } else {
            format!("等级 {}({})", self.level_old, basis)
        }
    }

    /// 二次确认码用的摘要:两次点击之间等级或经验一变,确认码就不同,会重新提示。
    pub fn digest(&self) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for v in [self.level_old, self.xp, self.level_new] {
            h = (h ^ (v as u32)).wrapping_mul(0x0100_0193);
        }
        h % 100_000_000
    }
}

/// [2026-10-03] 用户拍板:旧版「等级=N」开着时,-[UserInfoData encodeWithCoder:] 读的是被覆盖的 curLevel,把强制等级写进了
/// userinfo.dat(3ca5ece 起存档读真值,已不再写坏,但以前写进去的还在)。这里按经验值反推真实等级(只读,不写)。
/// 口径照原版 -[UserInfoData checkUpgrade]@0xba754:从当前等级 L 起,取 [[GameData sharedInstance] upgradeXPs] 的
/// 「level<L>」(0xba88a 格式串 "level%d",unsignedIntegerValue),经验值 ≥ 它就 L+1 继续(0xba8a6 cmp / bhs,无符号比较),
/// 第一个经验值小于门槛的 L 就是该有的等级。这里从 1 级起算(假等级可能比真实的高,不能从当前等级往上找);
/// 经验值超过整张表时取最高级(原版在 0xba852 越过表尾就不再升,满级玩家正好停在最高级)。
pub fn plan_level_recalc(env: &mut Environment) -> Result<LevelRecalcPlan, String> {
    main_village_offline_gate(env, "按经验值重算等级")?;
    let ui = user_info_data(env);
    let gd = singleton(env, "GameData", "sharedInstance");
    if ui == nil || gd == nil {
        return Err("主村存档对象还没准备好(userInfoData/GameData 为空)".to_string());
    }
    // 「等级=N」开着时 FORCE_LEVEL 臂对宿主发的 curLevel 也返回强制等级(只对 14 个存档/上传/比较调用点放行真值),
    // 所以直接读密文 ivar,照原 getter 解密。
    let off = ivar_offset(env, UI_CUR_LEVEL_SLOT, UI_CUR_LEVEL_OFF);
    let level_ptr: ConstPtr<u32> = Ptr::from_bits(ui.to_bits().wrapping_add(off));
    let raw: u32 = env.mem.read(level_ptr);
    let level_old = (raw ^ CUR_LEVEL_XOR) as i32;
    let s = sel(env, "xp");
    let xp: i32 = msg_send(env, (ui, s));
    let s = sel(env, "upgradeXPs");
    let table: id = msg_send(env, (gd, s));
    if table == nil {
        return Err("等级表 upgradeXPs 还没加载(GameData loadUpgradeXP 未执行)".to_string());
    }
    let s = sel(env, "count");
    let count: u32 = msg_send(env, (table, s));
    if count == 0 || count > 1000 {
        return Err(format!("等级表 upgradeXPs 条数异常({})", count));
    }
    let s_ofk = sel(env, "objectForKey:");
    let s_uiv = sel(env, "unsignedIntegerValue");
    let mut level_new = count as i32;
    let mut need_new = None;
    for l in 1..=count {
        let key =
            crate::frameworks::foundation::ns_string::from_rust_string(env, format!("level{}", l));
        let v: id = msg_send(env, (table, s_ofk, key));
        release(env, key);
        if v == nil {
            return Err(format!("等级表缺 level{},不敢推算", l));
        }
        let need: u32 = msg_send(env, (v, s_uiv));
        if (xp as u32) < need {
            level_new = l as i32;
            need_new = Some(need);
            break;
        }
    }
    Ok(LevelRecalcPlan {
        level_old,
        xp,
        level_new,
        max_level: count as i32,
        need_new,
    })
}

/// 按经验值重算等级并存主档(开发工具页「按经验值重算等级」第二次点击 / 文本命令 `level recalc apply`)。
/// 往哪个方向改都直接写等级,不走 checkUpgrade:升上来的那段原版当初升级时已发过奖励(假等级是后来被覆盖写进去的),
/// 再走一遍会重复发;降下去原版本来就没有降级路径。以前按假等级领过的升级奖励、买过的高等级物品都保留。
/// 实测:假低等级其实到不了这里——原版每次启动 -[MainMenuScene init] 0xb37fc 都调一次 checkUpgrade,读档后就按经验值逐级升回
/// (照原版发升级奖励);只有原版不会往下修的假高等级要靠本工具。
pub fn recalc_level(env: &mut Environment) -> DevResult {
    let p = plan_level_recalc(env)?;
    if !p.changes() {
        log!("[MOLEDEV] 按经验值重算等级:无需改动,{}", p.describe());
        return Ok(format!(
            "等级与经验值一致,无需重算(未改动存档):{}",
            p.describe()
        ));
    }
    let snap =
        snapshot_save(env).map_err(|e| format!("重算前保存快照失败:{},为安全起见没有改动", e))?;
    let ui = user_info_data(env);
    if ui == nil {
        return Err("主村 userInfoData 为空,没有改动".to_string());
    }
    // 与读档 initWithCoder: 0xb9c64..0xb9c82 同一写法:encryptInt: 后 setNewCurLevel:。
    let cu = env.objc.get_known_class("CryptUtils", &mut env.mem);
    if cu == nil {
        return Err("找不到 CryptUtils 类,没有改动".to_string());
    }
    let s = sel(env, "encryptInt:");
    let enc: i32 = msg_send(env, (cu, s, p.level_new));
    let s = sel(env, "setNewCurLevel:");
    let _: () = msg_send(env, (ui, s, enc));
    let off = ivar_offset(env, UI_CUR_LEVEL_SLOT, UI_CUR_LEVEL_OFF);
    let level_ptr: ConstPtr<u32> = Ptr::from_bits(ui.to_bits().wrapping_add(off));
    let raw: u32 = env.mem.read(level_ptr);
    let level_now = (raw ^ CUR_LEVEL_XOR) as i32;
    if level_now != p.level_new {
        return Err(format!(
            "写入后读回的等级是 {}(应为 {}),没有存档;重启即恢复原值",
            level_now, p.level_new
        ));
    }
    game_data_call(env, "saveUserInfoData");
    // 照原版 addXp: 0xbb12c..0xbb132 刷抬头(等级显示在经验条那一栏)。
    let wm = singleton(env, "WrapperManager", "sharedManager");
    if wm != nil {
        let s = sel(env, "updateUserInfoView:");
        let _: () = msg_send(env, (wm, s, 1i32));
    }
    log!("[MOLEDEV] 按经验值重算等级:{};重算前{}", p.describe(), snap);
    let forced = crate::mole_cheats::level();
    let force_note = if forced > 0 {
        format!(";「等级={}」还开着,抬头仍显示 {} 级", forced, forced)
    } else {
        String::new()
    };
    Ok(format!(
        "已按经验值重算并存档:{}(以前领过的升级奖励、买过的物品都保留){};重算前{}(可用「快照:下次启动恢复」回滚)",
        p.describe(),
        force_note,
        snap
    ))
}

// ───────────────────────── 任务跳转 ─────────────────────────

/// [2026-09-25 第五轮遗留 MISC-3] 读 [[SceneMannager sharedManager] curSceneId](+sharedManager @8@0:4、curSceneId i8@0:4);单例拿不到返回 -1。
/// 必须走宿主 msg_send,不能直读 ivar +12:-[ActorManager changeAvailableMolerForTask:]@0x9d9f8 自己也是发消息读它
/// (0x9da1c sharedManager、0x9da2c curSceneId,0x9da30 cmp #10),类级拦截不分宿主还是游戏发起,ON_ISLAND 时
/// mole_cheats 的 ("SceneMannager","curSceneId") 臂把 2 强制成 10 也同样作用于两边,所以这里读到的就是归还时游戏将读到的值。
fn island_scene_id(env: &mut Environment) -> i32 {
    let sm = singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return -1;
    }
    let s = sel(env, "curSceneId");
    msg_send(env, (sm, s))
}

/// [2026-09-25 第五轮遗留 MISC-3] 黄金岛任务跳转前,照原版 -[NewSceneQuest finish] 归还进行中打工任务(questType 7)占用的工人,
/// 返回归还人数(没有要还的返回 0)。
/// 根因:接取 -[NewSceneQuest accept]@0x3289b0 对打工类(0x328cce 判类型 7)在 0x3291ca rsb 取负、0x3291d8 发
/// [ActorManager changeAvailableMolerForTask:-n] 扣人(带走 n 只摩尔),0x329206 起把开工时刻写进 curQuestResult;原版唯一的归还点是
/// finish:0x32a130/0x32a144 先写哨兵 4294967295.0(0x32a298),0x32a156 checkQuestState,0x32a16a 判 questState==1,0x32a196 置 2,
/// 0x32a1c2 判 [curQuestData questType]==7,0x32a1da-0x32a1fc 取 n=[[[curQuestData requireThings] objectAtIndex:0] intValue],
/// 0x32a22c 发 changeAvailableMolerForTask:+n。开发工具的跳转走 -[NewSceneQuest quickStart:]@0x32b510:0x32b532 questState=0、
/// 0x32b54a curQuestId=0、0x32b564 nextQuestId=N、0x32b584 curQuestResult=0.0(0x32b608),不归还;以前本会话空闲工人就一直少 n 个、
/// 地图少 n 只摩尔,要等下次进岛 load_island_userinfo 从总数重算才恢复(quickStart 在 0x32b5b0 存的岛档也带着少掉的空闲数)。
/// 判据照 -[NewSceneQuest minusNeededWorkers]@0x32ae58(进岛重扣时判「本会话给它扣过人」的原版口径):先 checkQuestState
/// (@0x32b180:questState 为 2 保持,否则 = curQuestId>0),再 0x32aea2 curQuestResult≠哨兵(0x32af58)、0x32aeb2 questState==1、
/// 0x32aec8 curQuestType==7。-[NewSceneQuest curQuestType]@0x328150 = curQuestData 为 nil 时 0、否则 [curQuestData questType],与 finish 同判据。
/// 另加 ActorManager.m_isLoadMap(槽 0xb03e54,+260,BOOL)判据,同 mole_cheats 的 K8 臂:它由 loadMapFromData:forNPC: 在 0x245e0e 置 1,
/// 进岛 1 秒后 createIdleWorkers: 才在 0x241fde 清 0 并跑 minusNeededWorkers;窗口内本会话还没给任务扣过人,quickStart 之后也不会
/// 再扣,所以不还。
/// 归还用与 finish 0x32a22c 同一个调用 changeAvailableMolerForTask:+n(v12@0:4i8),不用 changeAvailableWorkers:(只改计数、不刷摩尔):
/// 岛分支 0x9dba6 changeAvailableWorkers:+n、0x9dbd4 addMoleForTask:n 刷回摩尔;0x9daa0「空闲≥总数」时原版自己跳过,不会超总数。
/// 调用方 quest_jump 已保证 curSceneId==10(见那里的场景门),这里再防御性判一次,保证走岛分支 0x9da34 而不是 0x9dae2 主村分支。
/// 只由菜单点击/文本命令回调调用,不在帧栈上,也不在 intercept 里,不需要恢复 r0-r3;宿主发出的这条消息 LR 不是 0x32a231,
/// K8 吞归还臂不会误吞。签名逐个按 objc_meta 核过:checkQuestState v8@0:4,questState/curQuestType/curQuestId/curIdleWorkerCount
/// i8@0:4,getUserInfoData/curQuestData/requireThings/+Instance @8@0:4,curQuestResult d8@0:4,count I8@0:4,objectAtIndex: @12@0:4I8。
fn island_return_task_workers(env: &mut Environment, quest: id) -> i32 {
    if quest == nil || island_scene_id(env) != 10 {
        return 0;
    }
    let s = sel(env, "checkQuestState");
    let _: () = msg_send(env, (quest, s));
    let s = sel(env, "questState");
    let state: i32 = msg_send(env, (quest, s));
    let s = sel(env, "curQuestType");
    let qtype: i32 = msg_send(env, (quest, s));
    if state != 1 || qtype != 7 {
        return 0;
    }
    let s = sel(env, "getUserInfoData");
    let ui: id = msg_send(env, (quest, s));
    if ui == nil {
        return 0;
    }
    let s = sel(env, "curQuestResult");
    let started: f64 = msg_send(env, (ui, s));
    // 哨兵 4294967295.0 = finish 已写过(已归还,等领奖);用 >= 避开浮点相等比较。
    if started >= 4294967295.0 {
        return 0;
    }
    let s = sel(env, "curQuestData");
    let qd: id = msg_send(env, (quest, s));
    if qd == nil {
        return 0;
    }
    let s = sel(env, "requireThings");
    let things: id = msg_send(env, (qd, s));
    if things == nil {
        return 0;
    }
    let s = sel(env, "count");
    let cnt: u32 = msg_send(env, (things, s));
    if cnt == 0 {
        return 0;
    }
    let s = sel(env, "objectAtIndex:");
    let first: id = msg_send(env, (things, s, 0u32));
    if first == nil
        || !env
            .objc
            .object_has_method_named(&env.mem, first, "intValue")
    {
        return 0;
    }
    let s = sel(env, "intValue");
    let n: i32 = msg_send(env, (first, s));
    if n <= 0 {
        return 0;
    }
    let am = singleton(env, "ActorManager", "Instance");
    if am == nil {
        return 0;
    }
    let off = ivar_offset(env, 0xb03e54, 260);
    let loading: u8 = env.mem.read(ConstPtr::<u8>::from_bits(am.to_bits() + off));
    if loading != 0 {
        log!(
            "[MOLEDEV] 黄金岛任务跳转:打工任务进行中,但仍在进岛加载窗口内(m_isLoadMap=1),createIdleWorkers: 尚未给它扣人,不归还"
        );
        return 0;
    }
    let s = sel(env, "curIdleWorkerCount");
    let before: i32 = msg_send(env, (ui, s));
    let s = sel(env, "curQuestId");
    let cur: i32 = msg_send(env, (quest, s));
    let s = sel(env, "changeAvailableMolerForTask:");
    let _: () = msg_send(env, (am, s, n));
    let s = sel(env, "curIdleWorkerCount");
    let after: i32 = msg_send(env, (ui, s));
    log!(
        "[MOLEDEV] 黄金岛任务跳转:打工任务 {} 进行中,照原版 finish 归还 {} 个工人(空闲 {} → {})",
        cur,
        n,
        before,
        after
    );
    n
}

/// [扫描修 2026-09-15] F7-4 任务跳转:四族 quickStart:。
/// 核实:-[Quest quickStart:]@0x128500 做的是 questState=0、setCurQuestId:0、setNextQuestId:N,
/// 要等下一次 Quest activate: 才真正变成任务 N,所以主线跳转后补发 [GameManager activateStoryQuest]
/// (@0x1a218 → Story activate → Quest activate:)。限时/VIP 的进度存在 map 里的 timeQuestDataInMap/
/// vipQuestDataInMap,由 -[GameData saveMapData:] 里的 saveTimeQuestDataInDir/saveVipQuestDataInDir 落盘。
/// 黄金岛 -[NewSceneQuest quickStart:]@0x32b510 自己会调 saveUserinfoBothInLocalAndRemote。
/// 任务号范围照原版 onButton*QuestPlus: 的封顶:1..对应数据表 count。
/// [2026-09-25 第五轮遗留 MISC-3] 黄金岛另要求场景已是黄金岛(curSceneId==10,进出岛途中拒绝);跳离进行中的打工任务前,
/// 照原版 finish 归还它占用的工人(见 island_return_task_workers)。
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
        // [2026-09-25 第五轮遗留 MISC-3] 场景门:island_session_active() 在三段过渡时间里也为真——进岛半路失败时进岛窗口在主村残留
        //   (curSceneId=1,最多 1200 帧)、LoadingHoliday 期间(startNewSceneFrom:toScene: 在 0x24152a 写 2)、离岛过渡(2→1,最多 3600 帧)。
        //   这时 +[NewSceneQuest sharedInstance]@0x327e08 只在 curSceneId==1(0x327e3c)才返回 nil,curSceneId=2 时照样建出单例、
        //   读到已载入的岛进度;-[ActorManager changeAvailableMolerForTask:] 在 0x9da30 判场景号不是 10,就走 0x9dae2 主村分支
        //   (0x9db22/0x9db48 比空闲与总数 → 0x9dba6 changeAvailableWorkers: / 0x9dbd4 addMoleForTask:),归还会加到主村工人上
        //   (随主档落盘)并往主村刷摩尔;quickStart: 在 0x32b5b0 也会存一份不在场的岛档。所以岛任务跳转只在 curSceneId==10 时执行。
        //   放在取 NewSceneData/NewSceneQuest 单例之前,过渡窗口里也不会提前建出单例。读法见 island_scene_id。
        if island_scene_id(env) != 10 {
            return Err(
                "进岛加载或离岛过渡中,场景还没切到黄金岛,请等进岛完成后再跳转黄金岛任务"
                    .to_string(),
            );
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
    // [2026-09-25 第五轮遗留 MISC-3] 必须在 quickStart: 之前:之后 curQuestId 已清零、curQuestData 为 nil,判不出进行中的打工任务;
    //   放在前面,quickStart 0x32b5b0 的存盘也会带上归还后的空闲数。
    let returned_workers = if family == QuestFamily::Island {
        island_return_task_workers(env, quest)
    } else {
        0
    };
    // [2026-10-04 第八轮 R8-B1] 限时/VIP 跳转后会直接 saveMapData(只判 curSceneId==1、对象数、m_isLoadMap);主村好友入口放开后
    //   离线也能串门,好友村/丝尔特村时 gameMode=0 而 curSceneId 仍是 1,会把别人的地图写进 map.dat。原版 saveToLocal 遇 gameMode 0/6
    //   跳过(0x7cb14/0x7cb18),这里照同一口径要求 currentGameMode==1,放在 quickStart: 之前,拒绝时不改任何进度。
    if matches!(family, QuestFamily::Time | QuestFamily::Vip) {
        let mode = wrapper_game_mode(env);
        if mode != 1 {
            return Err(format!(
                "请先回到自己的庄园、关闭其它面板再跳转{}任务(currentGameMode={})",
                label, mode
            ));
        }
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
        // [2026-09-24 第四轮 K14 I4-4] 黄金岛跳转后补发激活,照原版 -[NewGameManager checkActiveStoryQuest] 在 0x246850-0x246866
        // 发的 [[NewSceneQuest sharedInstance] activate:0]。根因:quickStart:@0x32b510 只写 questState=0(0x32b532)、
        // setCurQuestId:0(0x32b54a)、setNextQuestId:N(0x32b564),不碰 canActivate(ivar +244);点 NPC 走
        // -[ActorManager touchEnd:] 发 activate:1,-[NewSceneQuest activate:]@0x328190 在参数为 1 时(0x3281ea/0x3281ee)
        // 跳过 checkCanActivate,0x32820e-0x328212 读到 canActivate==0 就整条返回——点布兰没反应,要退岛重进才恢复。
        // activate:0 走 checkCanActivate@0x328380 → setCanActivate:1@0x32847c(刷 NPC 101 头顶感叹号)。activate: 自己的门
        // 照原版执行:NewGameManager.gameMode 不为 0/6(0x3281d0/0x3281e6)、岛等级≥needLevel(0x32841e);等级不够置不上
        // canActivate 是原版行为,不绕。任务是 isAutomatic 且等级够时,原版这一发会直接 nextQuest 开始任务(0x32833a-0x328376)。
        // 签名 v12@0:4c8,BOOL 参数按仓库惯例传 false。菜单点击/文本命令回调,不在帧栈也不在 intercept 里,不需要恢复 r0-r3。
        // quickStart: 自己已调 saveUserinfoBothInLocalAndRemote(0x32b5b0),这里不再额外存盘。
        QuestFamily::Island => {
            let s = sel(env, "activate:");
            let _: () = msg_send(env, (quest, s, false));
        }
    }
    log!(
        "[MOLEDEV] 任务跳转 {} → {}(表内共 {} 条)",
        label,
        quest_id,
        count
    );
    // [2026-09-16] A2-05 删掉原来「简体中文下限时/VIP 任务受原版语言门限制」的附注:与反汇编相反。
    // -[VipQuest activate:]@0x38722c 没有语言门;-[TimeQuest activate:] 的语言门在 zh-Hans 下放行(见上)。
    // [2026-09-25 第五轮遗留 MISC-3] 黄金岛补一句沙原碎片:inject_sandgarden_fragments 下次进岛按进度兜底
    //   done(N) = nextQuestId>N && curQuestId!=N(N=81/83),跳过的 81/83 会补发 31005/31007(containsObject: 去重,已有的不重复发)。
    let skipped = if family == QuestFamily::Island {
        "被跳过任务的奖励和前置条件不会补发;沙原碎片(任务 81/83 的奖励)下次进岛会按任务进度兜底补发(已有的不重复发)"
    } else {
        "被跳过任务的奖励和前置条件不会补发"
    };
    let mut note = String::from(match family {
        // 菜单还会在后面追加激活条件;toast 超宽会自动折行(mole_menu.rs add_toast)。
        // [2026-09-24 第四轮 K14 I4-4] 黄金岛跳转同样补发了激活(见上)。
        QuestFamily::Time | QuestFamily::Island => ";已补发激活",
        _ => "",
    });
    if returned_workers > 0 {
        note.push_str(&format!(
            ";已照原版归还进行中打工任务的 {} 个工人",
            returned_workers
        ));
    }
    Ok(format!(
        "已跳到{}任务 {}({}){}",
        label, quest_id, skipped, note
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
    // [2026-09-25 第五轮遗留 C] 岛会话中(进岛窗口/在岛上/进岛加载/离岛过渡,见 mole_cheats::island_session_active)不许开始旅行:
    //   旅行期间 island_flush 的落盘闸不写岛档,从岛上开始旅行的话,之后的岛上进度离岛即丢(下次进岛从盘上重读),
    //   交任务的奖励却已经 add*InNewScene: 当场进了主档 → 重启后同一条岛任务还能再领。旅行期间进岛另由 enterNewIslands 臂拦下。
    //   只用 island_session_active(),不另读 curSceneId:那样会连标题画面、节日村、好友村一起拒掉;在岛上 curSceneId 本来就被
    //   钩子强制成 10,不多给信息。岛档快进、岛任务跳转用的也是这个判据。进岛半路失败时进岛窗口会在主村残留最多 1200 帧
    //   (约 20 秒),离岛过渡最多 3600 帧,所以文案写「进出岛途中」,并提示等场景切换结束。
    if crate::mole_cheats::island_session_active() {
        return Err(
            "黄金岛上或进出岛途中不能时间旅行(旅行期间岛上进度无法保存),请回到主村、等场景切换结束后再旅行".to_string(),
        );
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
    // [2026-09-24 第四轮 K3 I7-01] 再补一句黄金岛:旅行期间 mole_cheats::island_flush 开头的落盘闸不写任何岛档
    // (免得把"未来"时间戳写进 island_*.dat,重启后出海/NPC 冷却/打工任务长期卡死)。
    // [2026-09-25 第五轮遗留 C] 只挡落盘会让岛上进度回滚而奖励留在主档(同一条岛任务能反复领),所以现在旅行期间进不了岛
    // (mole_cheats 的 enterNewIslands 臂拦下并弹提示)、岛会话中开始不了旅行(上面)、岛档快进也停用(island_fast_forward_minutes);
    // 落盘闸只作兜底。文案同步成「不能进岛,重启后恢复」,与菜单确认文案一致。
    Ok(format!(
        "已前进 {} 小时(不可回退),本次运行累计 {} 小时。偏移不跨重启保存:重启后时间回到现实,期间存下的\"未来\"时间要等现实追上。旅行期间活动中心付费操作禁用,此时拍的快照活动数据与主档不一致。旅行期间不能进入黄金岛,岛档快进也停用(岛上进度这段时间无法保存),重启回到现实时间后恢复;要测岛上计时请在不旅行时用「岛档快进」",
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
    // [2026-10-06 第十轮 R10-B2] 快照目录(<user_data>/snapshots)不分单机/联机,而恢复只在单机做:联机时存的
    //   快照(复制的是联机沙盒)下次单机「恢复快照」会被当成最新一份,整份覆盖单机档。与恢复口径一致,
    //   联机模式不保存快照(联机存档以服务器为准)。
    if env.options.network_access {
        return Err("在线模式下存档以服务器为准,不支持保存快照(快照只用于单机存档)".to_string());
    }
    if crate::mole_cheats::island_session_active() {
        return Err("请先回到主村再保存快照(离岛时岛档会自动完整落盘)".to_string());
    }
    // [2026-10-04 第八轮 R8-B1] 串门(好友村/丝尔特村,gameMode=0)时主村菜单层还在、curSceneId 仍是 1,下面的 saveMapData 会把
    //   别人的地图写进 map.dat;照原版 saveToLocal 的口径(gameMode 0/6 不存)要求 currentGameMode==1。
    if main_village_layer(env) != nil {
        let mode = wrapper_game_mode(env);
        if mode != 1 {
            return Err(format!(
                "请先回到自己的庄园、关闭其它面板再保存快照(currentGameMode={})",
                mode
            ));
        }
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

/// [2026-09-25 第五轮遗留 WK99] `workers` 命令的用法提示。
const WORKERS_USAGE: &str = "用法:workers recalc [force] [额外摩尔数 0..999](只预览) | workers recalc apply [force] [额外摩尔数 0..999] | workers set <总摩尔> <房间>(测试用)";

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
///   island ff <分钟>                                   → island_fast_forward_minutes([2026-09-24 第四轮 K4 I4-05] 岛档计时快进,
///                                                        主村离线执行、先自动存快照,下次进岛生效)
///   workers recalc [force] [额外摩尔数]                → plan_worker_recalc 只预览,不写盘([2026-09-25 第五轮遗留 WK99])
///   workers recalc apply [force] [额外摩尔数]          → recalc_workers(= 菜单「重算工人/房间」第二次点击;先自动存快照)
///   workers set <总摩尔> <房间>                        → set_workers_raw(测试专用,造旧版 99 档)
///   level recalc                                       → plan_level_recalc 只预览,不写盘([2026-10-03] 按经验值重算等级)
///   level recalc apply                                 → recalc_level(= 菜单「按经验值重算等级」第二次点击;先自动存快照)
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
        // [2026-09-24 第四轮 K4 I4-05] 岛档计时快进(主村离线执行,下次进岛生效;见 island_fast_forward_minutes)。
        "island" => match args.as_slice() {
            ["ff", m] => {
                let minutes: i64 = parse_command_number(m, "分钟数")?;
                island_fast_forward_minutes(env, minutes)
            }
            _ => Err(format!(
                "用法:island ff <分钟>(1..{},在主村离线执行,下次进岛生效)",
                TIME_SKIP_MAX_MINUTES
            )),
        },
        // [2026-09-25 第五轮遗留 WK99] 主村工人/房间重算。不带 apply 只预览;写入必须显式带 apply(= 菜单上的第二次确认)。
        //   force 跳过「旧版 99 指纹 / 少于居民房人口」判定,强制按 居民房人口 + 额外摩尔 重算(只开放在这里,不上菜单);
        //   set 仅供无头测试造旧版 99 档。
        "workers" => match args.as_slice() {
            ["recalc", rest @ ..] => {
                let mut rest = rest;
                let apply = rest.first() == Some(&"apply");
                if apply {
                    rest = &rest[1..];
                }
                let force = rest.first() == Some(&"force");
                if force {
                    rest = &rest[1..];
                }
                let extra: i64 = match rest {
                    [] => 0,
                    [n] => parse_command_number(n, "额外摩尔数")?,
                    _ => return Err(WORKERS_USAGE.to_string()),
                };
                if apply {
                    recalc_workers(env, extra, force)
                } else {
                    let p = plan_worker_recalc(env, extra, force)?;
                    Ok(if p.changes() {
                        format!(
                            "预览(未写入):{}。写入用 workers recalc apply{} [额外摩尔数]",
                            p.describe(),
                            if force { " force" } else { "" }
                        )
                    } else {
                        format!("存档正常,无需重算:{}", p.describe())
                    })
                }
            }
            ["set", t, r] => {
                let total: i64 = parse_command_number(t, "总摩尔")?;
                let rooms: i64 = parse_command_number(r, "房间数")?;
                set_workers_raw(env, total, rooms)
            }
            _ => Err(WORKERS_USAGE.to_string()),
        },
        // [2026-10-03] 按经验值重算等级。不带 apply 只预览;写入必须显式带 apply(= 菜单上的第二次确认)。
        "level" => match args.as_slice() {
            ["recalc"] => {
                let p = plan_level_recalc(env)?;
                Ok(if p.changes() {
                    format!("预览(未写入):{}。写入用 level recalc apply", p.describe())
                } else {
                    format!("等级与经验值一致,无需重算:{}", p.describe())
                })
            }
            ["recalc", "apply"] => recalc_level(env),
            _ => Err("用法:level recalc(只预览) | level recalc apply".to_string()),
        },
        "menu" => Err(format!(
            "暂不支持按页名打开菜单(「{}」):mole_menu 还没有翻页接口,请用不带参数的 menu 开关菜单",
            args.join(" ")
        )),
        _ => Err(format!(
            "无法识别的命令「{}」,支持 tap / drag / menu / suspend / dev / quest / story / time / give / island / workers / level",
            head
        )),
    }
}
