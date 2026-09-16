/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! MoleWorld offline port: toggle-style cheats (the "write config + hook getter"
//! features of the user's tweak), implemented by intercepting specific game
//! ObjC messages in `objc::messages`.
//!
//! The debug menu (`mole_menu`) flips these flags; `intercept` is called at the
//! top of `objc_msgSend_inner` for every message when at least one flag is on.
//! It either fully handles the call (returns `true` — the caller then returns
//! without dispatching) or modifies an argument register in place and returns
//! `false` (the real method then runs with the tweaked argument).

use crate::frameworks::core_graphics::cg_geometry::{CGPoint, CGRect, CGSize};
use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::objc::{autorelease, id, msg_send, nil, release, retain, SEL};
use crate::Environment;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const O: Ordering = Ordering::Relaxed;

/// [扫描修 2026-09-15] F10-6 同一日志点:首次用 log!(证明钩子生效,保留无头测试依赖的关键字),之后降为 log_dbg!。
/// `$flag` 是该日志点专属的 static AtomicBool。
macro_rules! log_first_then_dbg {
    ($flag:expr, $($arg:tt)+) => {
        if !$flag.swap(true, O) {
            log!($($arg)+);
        } else {
            log_dbg!($($arg)+);
        }
    };
}

/// 强制 VIP 的等级上限。游戏真实上限是 VIP10,但本移植按用户要求封顶 **VIP4**
/// (调试菜单「VIP等级」在 1..=VIP_LEVEL_MAX 循环,getVipInfoDataWithLevel: 也 clamp 到此)。
/// [扫描修 2026-09-15] F5-10/F1-9 纠偏记录:-[GameData loadVipUserInfoData] 只读本地 250_1.dat 的前 4 行
///   (0x7416e cmp r5,#4,第 5/6 行是死数据),与这里封顶 4 一致。property.dat 的 vip_only 字段无效
///   (parseObjectData@0x6efa0 写进 anonym_ 且 <1000 当场清零),250_2 是死表(revokeItems 无任何调用者);
///   真正的 VIP 限购只有 vip_level(主村 14286-14290/14529/相框 90000-90004,岛 32043-32047/32049/32050/32064/32066),
///   强制 VIP 已覆盖,别再拿 vip_only/250_2 当隐藏机制排查。
const VIP_LEVEL_MAX: i32 = 4;

/// [扫描修 2026-09-15] F11-10 在线模式(--allow-network-access)标志。intercept 收到首条消息起置位。
/// 菜单(is_on/toggle/island_arm_entry)没有 env,据此如实反映"在线时离线岛总闸被强制关闭"——以前菜单显示可切,
/// 下一条消息就被 intercept 复位,玩家看不出开关其实无效。
static ONLINE_MODE: AtomicBool = AtomicBool::new(false);
/// [扫描修 2026-09-15] 集成:mole_dev::startup 只调一次(见 intercept)。
static DEV_STARTUP_DONE: AtomicBool = AtomicBool::new(false);
/// [扫描修 2026-09-15] F10-6 去广告/地图上传各日志点的"已打过一次 log!"标志。
// [2026-09-16] B-05 删掉 LOG1_AD_MOLECART:它只服务 getMoleCartAdImageFromServer 诊断臂,那一臂在原版不可达,已一并删除。
//   (用普通注释而非 ///,免得这句挂成下一行 LOG1_AD_PROMPT 的文档注释。)
static LOG1_AD_PROMPT: AtomicBool = AtomicBool::new(false);
static LOG1_AD_MOREGAME: AtomicBool = AtomicBool::new(false);
static LOG1_AD_ZHONGXIN: AtomicBool = AtomicBool::new(false);
static LOG1_MAP_UPLOAD: AtomicBool = AtomicBool::new(false);
/// [2026-09-16] F1-02 mapExtend 取景覆盖、F1-01 岛上 showWithTarget:selector: 非法哨兵 target 被吞掉,各自的首次 log! 标志
/// (两处都可能每帧/每次点击命中,首次 log! 证明钩子生效,之后降为 log_dbg!)。
static LOG1_MAPEXTEND_VIEW: AtomicBool = AtomicBool::new(false);
static LOG1_ISLAND_BAD_TARGET: AtomicBool = AtomicBool::new(false);
/// [扫描修 2026-09-15] F12-10 「左左右右」操作提示本进程是否已弹过(只弹一次)。
static WASHROOM_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

static FREE_SHOP: AtomicBool = AtomicBool::new(false);
static KILL_ANTICHEAT: AtomicBool = AtomicBool::new(false);
static FORCE_VIP: AtomicBool = AtomicBool::new(false);
/// 1 = off (no multiplier). Toggled to 10 by the menu.
static GOLD_MULT: AtomicI32 = AtomicI32::new(1);
static XP_MULT: AtomicI32 = AtomicI32::new(1);
static INSTANT_CROP: AtomicBool = AtomicBool::new(false);
static NO_WITHER: AtomicBool = AtomicBool::new(false);
static NO_COOLDOWN: AtomicBool = AtomicBool::new(false);
static INSTANT_BUILD: AtomicBool = AtomicBool::new(false);
/// 主村工人/空闲工人/房间数 getter 恒返回 99(收菜建造不卡人力/容量)。
/// [2026-09-16] G-07 只管主村 UserInfoData,岛上不做(见 intercept 里的说明)。
static MAX_FACILITY: AtomicBool = AtomicBool::new(false);
/// 收菜结算建筑加成倍率 getter 恒返回 1000(=10倍经验/金币,走原生管线无溢出)。
static HARVEST_MULT: AtomicBool = AtomicBool::new(false);
/// 任务/催熟所需贝壳数 → 0(秒完成免费)。
/// [2026-09-16] G-07 覆盖主线/限时/黄金岛/日常/VIP 任务(Quest/TimeQuest/NewSceneQuest/DailyQuest/VipQuest)。
static FREE_QUEST: AtomicBool = AtomicBool::new(false);
/// 海底寻宝必中稀有:generateRandomRewardId 恒返回最稀档 id(roll6-10 档 = 31169)。
static SEABED_BEST: AtomicBool = AtomicBool::new(false);
/// 小游戏奖励满。
/// [2026-09-16] A2-03+G-04 改成在 -[MiniGameManager enterAchivement:] 结算读 gainCoin/gainXP 时放大 10 倍(封顶 99999),
/// 对所有经这个结算点入账的小游戏生效;原来钩的 getRewardCoin:/getRewardXp: 已删。
static MINIGAME_REWARD: AtomicBool = AtomicBool::new(false);
/// VIP level reported while force_vip is on (cycled 1..=VIP_LEVEL_MAX by the menu).
static VIP_LEVEL: AtomicI32 = AtomicI32::new(VIP_LEVEL_MAX);
/// Forced player level (0 = off; cycled 0/10/.../100 by the menu). Overrides the
/// curLevel getter, mirroring how FORCE_VIP overrides vipLevel.
static FORCE_LEVEL: AtomicI32 = AtomicI32::new(0);
/// All shop / collection items reported as unlocked.
static ALL_UNLOCK: AtomicBool = AtomicBool::new(false);
/// 成就面板全亮:只让 -[AchievementItems unlocked:] 返回 YES(纯显示)。
/// [2026-09-16] G-05 不再拦 checkInAlreadyUnlockList:,真实成就判定、记录与发奖照常进行。
static ALL_ACHIEVE: AtomicBool = AtomicBool::new(false);
/// Tripped when a save field that should be an NSDictionary
/// (UserInfoData.achieveUnlock / attributeValue, or mapData) decoded as an
/// NSMutableArray — the signature of a save corrupted by the old archiver
/// pointer-reuse dedup bug (now fixed in `ns_keyed_archiver.rs`). Set by
/// `note_dict_as_array_corruption()`, called from the foundation layer
/// (ns_array.rs dictionary-message shims, ns_dictionary.rs initWithDictionary:
/// emptying). When set, the harvest achievement re-trigger is suppressed (see
/// `checkInAlreadyUnlockList:`) so already-corrupted saves don't OOM-crash on
/// mass harvest. Healthy saves never trip it, so real achievement logic runs.
static SAVE_HAS_DICT_AS_ARRAY: AtomicBool = AtomicBool::new(false);

/// Called by the Foundation layer when a dictionary-typed value turns out to be
/// an NSMutableArray (corrupted save). Idempotent; logs once.
pub fn note_dict_as_array_corruption() {
    if !SAVE_HAS_DICT_AS_ARRAY.swap(true, O) {
        log!("[MOLECHEAT] 侦测到坏档:本应是字典的字段被还原为数组,启用成就重复触发抑制以防批量收菜 OOM 崩溃(治本在 NSKeyedArchiver,旧坏档下次保存即自愈)");
    }
}
/// Magic-password bypass. Read by the MagicNumberView hook in `objc::messages`
/// (class-gated there, not via `any_enabled()`), so it stays out of that fast
/// path — it never needs to intercept ordinary messages.
static MAGIC_BYPASS: AtomicBool = AtomicBool::new(false);
/// Golden Island (加勒比寻宝 Caribbean) offline fix: locally synthesize the
/// server-only CaribbeanDiscoveringData + dismiss the modal LoadingLayer that
/// otherwise freezes the activity offline. Read by the SHELLHOOK in
/// `objc::messages` (class-gated, not via `any_enabled()`). Defaults ON because
/// it's a repair for a dead server feature (the hooks only touch Caribbean
/// methods), so opening Golden Island in-game just works without toggling.
static FIX_GOLDEN_ISLAND: AtomicBool = AtomicBool::new(true);
/// Golden Island "sail straight to the finish" (curIsland=5, distanceToNext=0).
static GOLDEN_WIN: AtomicBool = AtomicBool::new(false);
/// Set when GOLDEN_WIN flips so `build_caribbean_data` re-applies the fields
/// once — WITHOUT clobbering the player's in-progress sailing on every read.
static CARIBBEAN_DIRTY: AtomicBool = AtomicBool::new(false);

/// 离线**黄金岛(NewScene 可建筑岛,scene id 10)**总开关。注意:这跟上面那个
/// `FIX_GOLDEN_ISLAND`(Caribbean 加勒比寻宝活动)是**两个不同功能**,别混。
/// 用户描述的"小岛/飞机过场/单独可建筑场景"= 本 NewScene 岛。
/// ✅ 一期 ABI 验证桩 `probe_island_abi` 已实测通过(2026-06-03):构造 TMMapDataShop,
/// setObjectId:(int)/setBaseTile:(CGPoint)/setBeginTime:(double) 全部正确落字段,
/// ivar 与 getter(含 CGPoint sret 返回)双向回读 objectId=30101 baseTile=(22,42),
/// 零崩溃。→ mapData 注入(方案A 手工构造 NSMutableDictionary)的 ABI 已确认可行。
/// ★默认 ON(用户要求:不用每次开关,点村里的飞机/岛屿热点即可进岛)。岛上各 hook 仅在
/// 岛专属选择器(enterNewIslands/updateLoading/HolidayVillageLayer 等)上动作,主村期间几乎
/// 全部空过(网络门只在 ISLAND_ENTER_WINDOW>0||ON_ISLAND 时强制,主村两者皆假);看门狗也
/// 改为只在岛上生效。代价仅是 intercept 走全量消息(与开任意作弊时同档,可接受)。
/// ★★2026-06-22 修回 new(true)(曾被搞服务器时误改成 new(false)→离线点飞机进岛卡死:飞机路径
/// 不像作弊菜单 enter_island 会先 island_arm_entry() 置 true,ENABLE=false 时所有岛 hook[网络门/
/// 解活锁/SUCC 调度]全不跑→撞死掉的离线网络→卡死)。在线模式(--allow-network-access)由 intercept
/// 开头强制 store(false),不干扰私服/在线工作;离线(默认)保持 ON,飞机/作弊菜单两条路径等价可进。
static ENABLE_NEWSCENE_ISLAND: AtomicBool = AtomicBool::new(true);

/// 进岛网络门强制窗口(剩余帧数;>0 时把 NetworkManager isConnected/state/isReachable
/// 强制成"在线",**只覆盖进岛加载序列**,不污染主村离线行为)。每帧 drawScene 递减。
/// gate#1 触发时设为约 20 秒(1200 帧),足够走完飞机过场 + LoadingHoliday 全部状态。
static ISLAND_ENTER_WINDOW: AtomicI32 = AtomicI32::new(0);

/// 问题2-A:玩家当前是否在黄金岛上。★事件驱动(loadNewScene 置 true / gobackMainVillage
/// 置 false),绝不在 drawScene 每帧 msg_send 探测——那会在帧定时器栈同步跑 guest=进岛卡死。
/// 网络门在"进岛窗口内 或 在岛上"都强制在线 → 岛上周期/触摸网络检查不再弹断网框踢人,
/// 且触摸时 state==6 走正常 processTouch(否则触摸被网络检查分支吞掉)。
static ON_ISLAND: AtomicBool = AtomicBool::new(false);
/// [审计修] 进岛链确实走到了 gate#1(-[GameManager updateGameDateForEnterNewSceneWithTarget:andCallback:])。
/// 菜单一键进岛据此确认真 enterNewIslands 没有被前置门静默拒绝(以前不查,日志照报成功)。
static ISLAND_GATE1_HIT: AtomicBool = AtomicBool::new(false);
/// [审计修] 岛存档"脏"标志:岛上发生了需要持久化的变化(经营态/增删建筑/任务剧情/经济/碎片)。
/// 由 moleIslandTick(每秒一次,运行循环 perform 相位)节流落盘。以前只在离岛时写,崩溃/关窗=整局丢。
static ISLAND_DIRTY: AtomicBool = AtomicBool::new(false);
/// moleIslandTick 定时器是否在跑(防重复排程)。
static ISLAND_TICK_RUNNING: AtomicBool = AtomicBool::new(false);
/// [2026-09-16 黄金岛审查修 I5-01] 岛农场任务 4「雇一只摩尔」的完成信号本次进岛是否已补发(一次性)。
/// 进岛(loadNewScene:10)时清零,补发成功后置位,避免每拍重发。
static ISLAND_QUEST4_SENT: AtomicBool = AtomicBool::new(false);
/// [审计修] 岛存档正在落盘(island_flush 内部会调 saveUserinfoToLocal 等,别让它们反过来置脏形成 1.5s 循环)。
static ISLAND_FLUSHING: AtomicBool = AtomicBool::new(false);
/// [审计修] 进岛加载中:[LoadingManager enterLoadingWithDelegate:nextSceneId:10] 起,到 [SceneMannager loadNewScene:10] 止。
/// 以前网络门/加载活锁解除/默认岛注入全挂在 1200 帧窗口上,加载一慢窗口先耗尽就永久卡在加载画面。
static ISLAND_LOADING: AtomicBool = AtomicBool::new(false);
/// [审计修] 离岛过渡中:startNewSceneFrom:10 toScene:1 起,到 SceneMannager.curSceneId_ 回到 1 止。
static ISLAND_EXITING: AtomicBool = AtomicBool::new(false);
/// SceneMannager 单例指针。drawScene 里只读它 +12 的 curSceneId_ 判定过渡是否完成(绝不在帧栈里发消息)。
static ISLAND_SCENE_MGR: AtomicU32 = AtomicU32::new(0);
/// 离岛过渡超时兜底(drawScene 帧数)。LoadingMainVillage 若卡住 curSceneId_ 会一直停在 2。
static ISLAND_EXIT_FRAMES: AtomicI32 = AtomicI32::new(0);
/// [深扫修 2026-09-11] #7 岛档坏档保护位掩码(ISLAND_FILE_*)。某位置位 = 该岛档的规范路径上是一份【存在但解档为 nil、
/// 且还没能改名隔离成 .corrupt】的文件——置位期间 island_flush 的所有落盘路径(节拍 / 离岛 startNewSceneFrom 出口 /
/// 关窗 AWRA·AWT)都跳过这份文件,绝不拿默认岛/默认进度覆盖玩家唯一的一份数据。
/// 清位时机(想清楚的规则):
///   ① 读档时发现坏档并【改名隔离成功】→ 立即清(数据已安全转移到 .corrupt,原路径上已无可丢的数据;若继续阻塞,
///      本会话在默认岛上的新进度会在退出时白丢,下次启动又是"无档"→ 永远存不下来);
///   ② 之后同一文件【成功解档】(例如玩家手动修好/换回了文件再进岛)→ 清;
///   ③ 落盘前发现原路径上的文件已不存在(玩家手动删了/挪走了)→ 清并恢复落盘。
///   隔离失败时一直保持到进程结束(下次进岛/下次启动会重新判定并重试隔离);不在节拍里反复重试改名,避免失败时
///   每秒在 Documents 里留下空的 .corrupt-* 目标文件。
static ISLAND_LOAD_FAILED: AtomicU32 = AtomicU32::new(0);
/// 各岛档"跳过落盘"日志只打一次(节拍每 1.5s 一次,防刷屏)。
static ISLAND_BLOCK_LOGGED: AtomicU32 = AtomicU32::new(0);
const ISLAND_FILE_MAP: u32 = 1 << 0;
const ISLAND_FILE_USERINFO: u32 = 1 << 1;
const ISLAND_FILE_SHIPS: u32 = 1 << 2;
const ISLAND_FILE_FRAGMENTS: u32 = 1 << 3;
thread_local! {
    /// 上次岛存档落盘时刻(节流用)。
    static ISLAND_LAST_FLUSH: Cell<Option<Instant>> = const { Cell::new(None) };
    /// 上一次被受理的 moleIslandTick 时刻(闩锁自愈 + 合并重复节拍链用)。
    static ISLAND_LAST_TICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 岛上 curSceneId 被改成 1/10 以外的值时强制 10,只打一次真实值(防刷屏)。
/// [2026-09-16] B-08 旧标签「[P3 商店空白真因诊断]」已过时:商店空白的真因早已由非脆弱 ivar 偏移写回 guest 修掉,这里只剩一次性状态日志。
static CURSCENE_DIAG_DONE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The locally-built CaribbeanDiscoveringData (retained guest object) or nil.
    static CARIBBEAN_DATA: Cell<id> = const { Cell::new(nil) };
    /// 本次进岛是否已注入默认 mapData(每次进岛在 gate#1 reset,避免重复注入)。
    static ISLAND_INJECTED: Cell<bool> = const { Cell::new(false) };
}

// ===== ONLINE MODE statics (boot-login passport bypass; see reference_touchhle_online_mode) =====
/// G3 armed the deferred login synth (set in autoLoginWithUserID: intercept).
static LOGIN_ARMED: AtomicBool = AtomicBool::new(false);
/// Login synth already fired once this launch (latched).
static LOGIN_FIRED: AtomicBool = AtomicBool::new(false);
/// The 米米号 to log in as (= MOLE_MIMI), captured when armed.
static LOGIN_MIMI: AtomicU32 = AtomicU32::new(0);
/// drawScene frame counter for auto-login arming (online mode, no Play tap needed).
static LOGIN_BOOT_FRAMES: AtomicU32 = AtomicU32::new(0);
/// Captured live MainMenuScene instance. CCDirector runningScene is only a CCScene
/// wrapper; the menu layer (which has onButtonChangeIDSelected:) is its child. 0 = unseen.
static MAINMENU_SCENE: AtomicU32 = AtomicU32::new(0);
/// Online login phase-2 one-shot: the login packet has been sent (after the socket connected).
static LOGIN_PKT_SENT: AtomicBool = AtomicBool::new(false);
/// Debug HUD live connection stats, counted in the changeStateTo: hook (state 6 = a packet was
/// written, state 7 = a packet was parsed/received). loss/pending = sent - recv; RTT = the gap
/// between the last state→6 and the next state→7.
static PKTS_SENT: AtomicU32 = AtomicU32::new(0);
static PKTS_RECV: AtomicU32 = AtomicU32::new(0);
static LAST_RTT_MS: AtomicU32 = AtomicU32::new(0);
/// Diagnostic: last logged GameData.remoteMapData.mapdata.count (-99 = never read). Tells us
/// whether the server's 1001 map unarchives to a non-empty dict in THIS unarchiver (#2).
static LAST_MAP_COUNT: AtomicI32 = AtomicI32::new(-99);
/// The HUD must NOT msg_send during the connect window (state 4/6) — doing so starved the run-loop
/// and dropped the cf_stream Open event. STATE_IS_7 (set by the changeStateTo: hook) gates HUD
/// startup to AFTER the connection is up; HUD_TIMER_SET latches a 1s self-rescheduling tick that
/// refreshes the HUD via performSelector:afterDelay: in the run-loop perform phase — never inside
/// the drawScene frame stack — so it can't interfere with packets or the village scene transition.
static STATE_IS_7: AtomicBool = AtomicBool::new(false);
static HUD_TIMER_SET: AtomicBool = AtomicBool::new(false);
/// Village-render workaround. showWithTarget:4 schedules -[LoadingLayer update:] → (performSelector
/// OnMainThread:) loadTarget → case 4 (loadFromLocal + [GameManager startGame]) = build the village.
/// But in touchHLE the LoadingLayer's `update:` re-schedule after a prior loadTarget's
/// unscheduleAllSelectors does NOT re-fire, so the village's loadTarget never runs and we stay on the
/// title. We latch the LoadingLayer pointer at showWithTarget:4 and, if its natural update:/loadTarget
/// hasn't fired within a few frames, drive loadTarget ourselves from the drawScene tick.
static PENDING_LOADTARGET: AtomicU32 = AtomicU32::new(0);
static PENDING_LOADTARGET_FRAMES: AtomicU32 = AtomicU32::new(0);
/// 庄园地图持久化(修法甲)帧计数。进村稳定后(STATE_IS_7)host 周期性 saveMapData+updateInfoToServer
/// 把活图整包(gzip blob)发上来——主庄园持久化唯一上行通道(非 1059 增量,那是黄金岛机制)。
/// 原版自发上传被 saveMapData: 5道闸卡死→map 恒 0B;host 主动调已验证可用的无参 saveMapData 兜上。
static MAP_UPLOAD_FRAMES: AtomicU32 = AtomicU32::new(0);
thread_local! {
    /// MOLE_PASSWORD cleartext (None = unset; server-lenient empty hash).
    static LOGIN_PWD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// Instant of the last state→6 (packet written), for RTT to the next state→7.
    static LAST_SEND_AT: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// `Some(mimi)` only when online mode is on (`--allow-network-access`) AND `MOLE_MIMI`
/// parses to a u32. Otherwise `None` so every online-login branch is a no-op and the
/// offline single-player path is bit-for-bit unchanged.
fn online_login_mimi(env: &Environment) -> Option<u32> {
    if !env.options.network_access {
        return None;
    }
    std::env::var("MOLE_MIMI")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

// ===== ACCOUNT-MENU MODE: 让 touchHLE 也弹出原版账号管理菜单(切换账号)=====
// 默认在线模式靠 G3 吞掉 autoLoginWithUserID: + 帧180自动合成登录,passport UI 链从不出现。
// MOLE_ACCOUNT_MENU=1 时:不自动合成、不吞 autoLogin,放原版走真 passport 流程(TMALoginViewController/
// TMAccountManagerView);而 touchHLE 的 TMA_ASIHTTPRequest 出站是死桩,故把 app 发的 passport HTTP
// 在 host 侧用 std::net::TcpStream 明文真发到私服 passport shim(已跑通真机),响应异步回灌原版 requestFinish:。
// 全部门控在 account_menu_mode(),默认模式逐字节不变。

/// MOLE_ACCOUNT_MENU 开关(缓存,避免每条消息都查 env)。
fn account_menu_mode() -> bool {
    use std::sync::atomic::AtomicU8;
    static CACHE: AtomicU8 = AtomicU8::new(2); // 2=未初始化, 0=false, 1=true
    let c = CACHE.load(O);
    if c != 2 {
        return c == 1;
    }
    let v = std::env::var_os("MOLE_ACCOUNT_MENU").is_some();
    CACHE.store(u8::from(v), O);
    v
}

/// 一笔在飞的 passport 代理:retain 住的 request/delegate + 后台线程填的响应槽。
struct PassportProxy {
    request: u32,
    delegate: u32,
    /// None=在飞;Some(None)=失败;Some(Some(bytes))=拿到响应体。
    resp: Arc<Mutex<Option<Option<Vec<u8>>>>>,
}
static PASSPORT_PENDING: Mutex<Vec<PassportProxy>> = Mutex::new(Vec::new());
/// 回灌期间原版 [request responseData] 的 hook 从这里取 JSON(request-bits -> body)。
static PASSPORT_RESP: Mutex<Vec<(u32, Vec<u8>)>> = Mutex::new(Vec::new());
/// 最近一次 TMAHttpManager sendRequest: 的命令字(reqID)。touchHLE 模拟原版 ASI 请求构建残缺
/// (postData 丢了 service/extra_data 等字段),故 reqID 从 sendRequest: 参数直取,代理时据此构造 body。
static PENDING_REQID: AtomicU32 = AtomicU32::new(0);
/// 玩家点了"切换账号"(showAccountManagerViewWithDelegate:)后置真。只代理这之后的 passport;
/// 进村后 app 自动发的 autoLogin(走静默登录分支 onLoginRequestFinishWithStatusCode,touchHLE 缺桩 null deref)不碰。
static MENU_ACTIVE: AtomicBool = AtomicBool::new(false);
/// [扫描修 2026-09-15] F11-1 账号菜单模式:主菜单「切换账号」被拦下后锁存的 MainMenuScene 指针(0 = 无待办)。
/// 由 drawScene/mainLoop 钩子的寄存器恢复安全区消费(发原版 showLoginView),绝不在按钮回调栈里内联派发。
static PENDING_SHOW_LOGIN: AtomicU32 = AtomicU32::new(0);
/// [扫描修 2026-09-15] F11-1 默认(MOLE_MIMI)模式下「账号由启动器决定」提示,本进程只弹一次。
static CHANGEID_HINT_SHOWN: AtomicBool = AtomicBool::new(false);
/// [扫描修 2026-09-15] F11-4 登录回包 errorID==112(账号校验失败)待弹提示,由 drawScene 安全区消费。
static AUTH_FAIL_HINT_PENDING: AtomicBool = AtomicBool::new(false);
/// [扫描修 2026-09-15] F11-4 112 提示本进程已弹过。密码错时原版会反复重连重登、反复收到 112,只提示一次防刷屏。
static AUTH_FAIL_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

/// passport 私服端点:连私服 host 的明文 HTTP 端口,发 Host: account-mapi.61.com 让反代路由到 web passport。
/// MOLE_PASSPORT 覆盖 connect host:port(默认 MOLE_SERVER 的 host + 80 = Caddy 的 http://account-mapi.61.com 块)。
fn passport_endpoint() -> (String, u16) {
    if let Ok(p) = std::env::var("MOLE_PASSPORT") {
        if let Some((h, pt)) = p.rsplit_once(':') {
            if let Ok(pt) = pt.parse::<u16>() {
                return (h.to_string(), pt);
            }
        }
        return (p, 80);
    }
    let server =
        std::env::var("MOLE_SERVER").unwrap_or_else(|_| "login.moleworld.net:7821".to_string());
    let host = server
        .rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or(server);
    (host, 80)
}

/// host 侧明文 HTTP/1.1 POST(无 TLS;私服 Caddy:80 明文 + 客户端 setValidatesSecureCertificate:0)。
fn http_post_form(host: &str, port: u16, body: &[u8]) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect((host, port)).ok()?;
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(8)));
    let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(8)));
    let head = format!(
        "POST /account_service.php HTTP/1.1\r\nHost: account-mapi.61.com\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).ok()?;
    s.write_all(body).ok()?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).ok()?;
    let idx = resp.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    Some(resp[idx..].to_vec())
}

/// 读 NSData 的字节(反向 nsdata_from_bytes;[data length] + [data bytes])。
fn nsdata_to_bytes(env: &mut Environment, data: id) -> Vec<u8> {
    if data == nil {
        return Vec::new();
    }
    let len_sel = env
        .objc
        .register_host_selector("length".to_string(), &mut env.mem);
    let len: crate::mem::GuestUSize = msg_send(env, (data, len_sel));
    if len == 0 {
        return Vec::new();
    }
    let bytes_sel = env
        .objc
        .register_host_selector("bytes".to_string(), &mut env.mem);
    // NSData -bytes 返回 const void*(ConstVoidPtr),host msg_send 的返回类型必须精确匹配,
    // 写成 ConstPtr<u8> 会触发 touchHLE 的 Type mismatch panic。取 ConstVoidPtr 再 cast。
    let ptr: crate::mem::ConstVoidPtr = msg_send(env, (data, bytes_sel));
    if ptr.is_null() {
        return Vec::new();
    }
    env.mem.bytes_at(ptr.cast(), len).to_vec()
}

/// 把扁平 JSON `{"k":v,...}` 解析成 (key,value) 串对(value 去引号)。passport 响应都是扁平的。
/// 用来绕开 touchHLE 没实现的 JSONKit(JKDictionary/JKArray 是 unimplemented class)。
fn parse_flat_json(bytes: &[u8]) -> Vec<(String, String)> {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    let s = s.strip_prefix('{').unwrap_or(s);
    let s = s.strip_suffix('}').unwrap_or(s);
    let mut out = Vec::new();
    for pair in s.split(',') {
        if let Some((k, v)) = pair.split_once(':') {
            let k = k.trim().trim_matches('"').to_string();
            let v = v.trim().trim_matches('"').to_string();
            if !k.is_empty() {
                out.push((k, v));
            }
        }
    }
    out
}

/// 用串对构造标准 NSMutableDictionary(值用 NSString,客户端 objectForKey: + intValue 可读),
/// 替代 touchHLE 没实现的 JKDictionary。autoreleased。
fn build_nsdict(env: &mut Environment, pairs: &[(String, String)]) -> id {
    let cls = env
        .objc
        .get_known_class("NSMutableDictionary", &mut env.mem);
    let alloc = env
        .objc
        .register_host_selector("alloc".to_string(), &mut env.mem);
    let init = env
        .objc
        .register_host_selector("init".to_string(), &mut env.mem);
    let set = env
        .objc
        .register_host_selector("setObject:forKey:".to_string(), &mut env.mem);
    let dict: id = msg_send(env, (cls, alloc));
    let dict: id = msg_send(env, (dict, init));
    for (k, v) in pairs {
        let key = crate::frameworks::foundation::ns_string::from_rust_string(env, k.clone());
        let val = crate::frameworks::foundation::ns_string::from_rust_string(env, v.clone());
        let _: () = msg_send(env, (dict, set, val, key));
        // [扫描修 2026-09-15] F10-7 from_rust_string 返回 +1;touchHLE 的 setObject:forKey: 会 copy 键、retain 值
        //   (ns_dictionary.rs insert copy_key=true),字典已持有 → 释放我们自己的 +1。
        release(env, key);
        release(env, val);
    }
    autorelease(env, dict)
}

/// [[req url] absoluteString] -> Rust String。
fn asi_request_url(env: &mut Environment, req: id) -> String {
    let url_sel = env
        .objc
        .register_host_selector("url".to_string(), &mut env.mem);
    let nsurl: id = msg_send(env, (req, url_sel));
    if nsurl == nil {
        return String::new();
    }
    let abs_sel = env
        .objc
        .register_host_selector("absoluteString".to_string(), &mut env.mem);
    let s: id = msg_send(env, (nsurl, abs_sel));
    if s == nil {
        return String::new();
    }
    crate::frameworks::foundation::ns_string::to_rust_string(env, s).into_owned()
}

/// 拦 TMA_ASINetworkQueue addOperation:(passport 的实际发送动作)。若是 passport 请求:先 buildPostBody 取
/// body,retain 住 request/delegate,后台线程 host HTTP 发到私服,返回 true 跳过死的真出站;由 drive_passport 回灌。
fn passport_proxy_enqueue(env: &mut Environment, req: id) -> bool {
    if req == nil {
        return false;
    }
    let url = asi_request_url(env, req);
    if !(url.contains("account_service.php") || url.contains("account-mapi")) {
        return false;
    }
    // touchHLE 模拟原版 ASI 请求构建残缺(postData 丢 service/extra_data,sign 也空),没法从请求对象提取 body。
    // 改用 sendRequest: 抓到的 reqID + 登录米米号自己构造最小 passport body:
    //   service=reqID(服务端按它路由)、user_id/userid=米米号(1012 回显要与请求一致)、
    //   extra_data=reqID(客户端 requestFinish: 算 extra_data%65535=reqID 路由;reqID<65535 故就是 reqID)。
    let reqid = PENDING_REQID.load(O);
    if reqid == 0 {
        log!("[MOLECHEAT] passport 代理: 未捕获 reqID,放弃代理放行");
        return false;
    }
    let mimi = LOGIN_MIMI.load(O);
    let body =
        format!("service={reqid}&user_id={mimi}&userid={mimi}&extra_data={reqid}").into_bytes();
    let del_sel = env
        .objc
        .register_host_selector("delegate".to_string(), &mut env.mem);
    let delegate: id = msg_send(env, (req, del_sel));
    // 跳过了真 addOperation:(queue 不会 retain),自己 retain 住到回灌后再 release。
    let req_r = retain(env, req);
    let del_r = retain(env, delegate);
    let (host, port) = passport_endpoint();
    let preview: String = String::from_utf8_lossy(&body[..body.len().min(140)]).into_owned();
    log!(
        "[MOLECHEAT] passport 代理: {} ({}B) -> {}:{} body={:?}",
        url,
        body.len(),
        host,
        port,
        preview
    );
    let resp: Arc<Mutex<Option<Option<Vec<u8>>>>> = if reqid == 1012 {
        // ★autoLogin(1012)直接合成 status_code:1011:客户端 requestFinish: 走 case 1011 →
        //   [viewController showAccountManagerView] 弹账号菜单,绕过 status_code:0 走的
        //   onLoginRequestFinishWithStatusCode(touchHLE 缺桩 → null-page 崩)。
        let json =
            format!(r#"{{"status_code":1011,"result":0,"user_id":{mimi},"extra_data":{reqid}}}"#);
        log!("[MOLECHEAT] passport 1012 → 合成 status_code:1011(直接弹账号菜单,绕静默登录崩溃路径)");
        Arc::new(Mutex::new(Some(Some(json.into_bytes()))))
    } else {
        // 其它 reqID(1004 换号输框 / 1006 / 1008 邮箱...)走真代理到私服。
        let r: Arc<Mutex<Option<Option<Vec<u8>>>>> = Arc::new(Mutex::new(None));
        let rc = r.clone();
        std::thread::spawn(move || {
            *rc.lock().unwrap() = Some(http_post_form(&host, port, &body));
        });
        r
    };
    PASSPORT_PENDING.lock().unwrap().push(PassportProxy {
        request: req_r.to_bits(),
        delegate: del_r.to_bits(),
        resp,
    });
    true
}

/// 每帧调(drawScene):把后台线程已拿到响应的 passport 代理回灌给原版 requestFinish:/requestFailed:。
/// 含 msg_send(clobber 寄存器),只在 drawScene 的 saved_r0/r1 恢复区内调用。
fn drive_passport(env: &mut Environment) {
    let mut done: Vec<(u32, u32, Option<Vec<u8>>)> = Vec::new();
    {
        let mut pend = match PASSPORT_PENDING.try_lock() {
            Ok(p) => p,
            Err(_) => return,
        };
        if pend.is_empty() {
            return;
        }
        pend.retain(|p| match p.resp.lock().unwrap().take() {
            Some(result) => {
                done.push((p.request, p.delegate, result));
                false
            }
            None => true,
        });
    }
    for (req_bits, del_bits, body) in done {
        let req: id = Ptr::from_bits(req_bits);
        let delegate: id = Ptr::from_bits(del_bits);
        match body {
            Some(bytes) => {
                PASSPORT_RESP.lock().unwrap().push((req_bits, bytes));
                let rf = env
                    .objc
                    .register_host_selector("requestFinish:".to_string(), &mut env.mem);
                let _: () = msg_send(env, (delegate, rf, req));
                PASSPORT_RESP.lock().unwrap().retain(|(b, _)| *b != req_bits);
                log!("[MOLECHEAT] passport 代理回灌 requestFinish: req={:#x}", req_bits);
            }
            None => {
                let rf = env
                    .objc
                    .register_host_selector("requestFailed:".to_string(), &mut env.mem);
                let _: () = msg_send(env, (delegate, rf, req));
                log!("[MOLECHEAT] passport 代理失败 requestFailed: req={:#x}", req_bits);
            }
        }
        release(env, req);
        release(env, delegate);
    }
}

/// Deferred boot-login synth, fired once from the safe drawScene/mainLoop frame edge
/// (NEVER inline from the intercept — cocos2d re-entrancy freezes, same as the island
/// lesson). Builds GameData.taomeeUserInfo = TaomeeUserInfo{MOLE_MIMI, MOLE_PASSWORD},
/// resolves the live login delegate (MainMenuScene), and drives
/// onTaomeeLoginViewDidUnloadWithUserID:password:returnCode: which (because isReachable
/// was forced true) runs establishConnection -> serverlist -> AsyncSocket/CFStream connect.
fn fire_online_login(env: &mut Environment) {
    if LOGIN_PKT_SENT.load(O) {
        return; // both phases done
    }
    // PHASE 2: phase 1 fired the cold native passport callback, which armed the scene (+235) and ran
    // establishConnection. Once the socket reached state 4 (connected), re-fire the SAME callback —
    // its state==4 branch sets delegateLoginMainMenu (so 1234/1001 replies reach
    // onLoginMainMenuCommandReceived:) and sends the native login (sendType 3). One [nm state] read
    // per frame is light enough not to disturb the connect (it was the HUD's MANY per-frame msg_sends
    // that dropped the Open event, not a single state read).
    if LOGIN_FIRED.load(O) {
        let scene: id = Ptr::from_bits(MAINMENU_SCENE.load(O));
        if scene == nil {
            return;
        }
        let nm_cls = env.objc.get_known_class("NetworkManager", &mut env.mem);
        let shared = env
            .objc
            .register_host_selector("sharedInstance".to_string(), &mut env.mem);
        let nm: id = msg_send(env, (nm_cls, shared));
        if nm == nil {
            return;
        }
        let st = env
            .objc
            .register_host_selector("state".to_string(), &mut env.mem);
        let state: i32 = msg_send(env, (nm, st));
        if state != 4 {
            return; // still connecting; retry next frame
        }
        LOGIN_PKT_SENT.store(true, O);
        let mimi = LOGIN_MIMI.load(O);
        let pwd = std::env::var("MOLE_PASSWORD").unwrap_or_default();
        fire_passport_unload(env, scene, mimi, &pwd);
        log!(
            "[MOLECHEAT] 在线:phase2 原生 passport 回调@state4(挂 delegateLoginMainMenu + 发原生登录),米米号={}",
            mimi
        );
        return;
    }
    // Use the captured live MainMenuScene instance (running scene is just a CCScene wrapper;
    // onButtonChangeIDSelected: lives on this menu layer).
    let scene: id = Ptr::from_bits(MAINMENU_SCENE.load(O));
    if scene == nil {
        return; // MainMenuScene not seen yet; retry next frame
    }
    let resp_btn = env
        .objc
        .object_has_method_named(&env.mem, scene, "onButtonChangeIDSelected:");
    // Wait until MainMenuScene is the running scene (it implements the Play handler).
    if !resp_btn {
        return; // not ready yet; retry next frame (LOGIN_FIRED stays false)
    }

    LOGIN_FIRED.store(true, O);
    // Populate GameData.serverLinkInfoList directly with the private server. This is
    // deterministic and skips the async serverlist HTTP + background NSOperationQueue timing
    // race: establishConnection then sees a non-empty list and goes straight to connectToHost
    // (RE: establishConnection iterates serverLinkInfoList of ServerLinkData(ip,port)).
    // (Tested removing this — the "remote player" disconnect persisted AND the village no longer stayed
    // on screen, so it is NOT the churn cause and is load-bearing for a stable connection. Keep it.)
    if let Ok(server) = std::env::var("MOLE_SERVER") {
        let (ip, port) = match server.trim().rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.trim().parse::<i32>().unwrap_or(7821)),
            None => (server.trim().to_string(), 7821),
        };
        let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
        let shared0 = env
            .objc
            .register_host_selector("sharedInstance".to_string(), &mut env.mem);
        let gd: id = msg_send(env, (gd_cls, shared0));
        if gd != nil {
            let rm = env.objc.register_host_selector(
                "removeAllObjectFromServerLinkList".to_string(),
                &mut env.mem,
            );
            let _: () = msg_send(env, (gd, rm));
            let sld_cls = env.objc.get_known_class("ServerLinkData", &mut env.mem);
            let alloc_s = env
                .objc
                .register_host_selector("alloc".to_string(), &mut env.mem);
            let sld: id = msg_send(env, (sld_cls, alloc_s));
            let init_s = env
                .objc
                .register_host_selector("init".to_string(), &mut env.mem);
            let sld: id = msg_send(env, (sld, init_s));
            let ip_ns = crate::frameworks::foundation::ns_string::from_rust_string(env, ip.clone());
            let setip = env
                .objc
                .register_host_selector("setIp:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (sld, setip, ip_ns));
            // [扫描修 2026-09-15] F10-7 -[ServerLinkData setIp:]@0x6911c 释放旧值后自己 [[NSString alloc] init…] 重建一份,
            //   不持有参数 → 释放 from_rust_string 的 +1。
            release(env, ip_ns);
            let setport = env
                .objc
                .register_host_selector("setPort:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (sld, setport, port));
            let addobj = env.objc.register_host_selector(
                "addObjectToServerLinkListWithObject:".to_string(),
                &mut env.mem,
            );
            let _: () = msg_send(env, (gd, addobj, sld));
            let rel = env
                .objc
                .register_host_selector("release".to_string(), &mut env.mem);
            let _: () = msg_send(env, (sld, rel));
            log!(
                "[MOLECHEAT] 在线:已直接注入 serverLinkInfoList -> {}:{}",
                ip,
                port
            );
        }
    }
    // Hand off to the game's NATIVE online entry instead of poking the state machine out-of-band
    // (RE-confirmed root cause: out-of-band parked at state 4, where -[NetworkManager
    // sendPacket:commandId:]@0xe231c REDIRECTS every non-1234 packet back into re-login, so the
    // server only ever saw cmd=1234 — AND we never set delegateGameData, the master gate).
    // -[GameManager connect2Server]@0x1aedc: `if [NM isReachable](method, our G1 hook→1) {
    //   setDelegateGameData:GameManager (★the gate); setDelegateFriends:0; if !connected {
    //   setState:2; establishConnection } }`. On connect, -[GameManager onStateChangedTo:]@0x21984
    // case 4 auto-sends login (sendType 3) → the state machine advances 4→6→7, after which the
    // native village fetches (1001/1062) actually transmit. We only pre-seed what establishConnection
    // / the sendType-3 login read directly: the isReachable_ IVAR, the header userId (=米米号), a
    // TaomeeUserInfo password fallback, and serverLinkInfoList (injected just above). Then the game runs.
    let mimi = LOGIN_MIMI.load(O);
    let nm_cls = env.objc.get_known_class("NetworkManager", &mut env.mem);
    let shared = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nm: id = msg_send(env, (nm_cls, shared));
    if nm == nil {
        return;
    }
    let set_reach = env
        .objc
        .register_host_selector("setIsReachable:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (nm, set_reach, true));
    // header userId = 米米号 (loginWithDeviceInfo sendType 3 reads getLocalUserInfoDataFromGameData.userId)
    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
    let gd: id = msg_send(env, (gd_cls, shared));
    let glu = env
        .objc
        .register_host_selector("getLocalUserInfoDataFromGameData".to_string(), &mut env.mem);
    let uinfo: id = msg_send(env, (gd, glu));
    if uinfo != nil {
        let set_uid = env
            .objc
            .register_host_selector("setUserId:".to_string(), &mut env.mem);
        let _: () = msg_send(env, (uinfo, set_uid, mimi));
    }
    // TaomeeUserInfo{米米号, MOLE_PASSWORD} — password fallback for the sendType-3 login builder.
    let pwd = std::env::var("MOLE_PASSWORD").unwrap_or_default();
    let tui_cls = env.objc.get_known_class("TaomeeUserInfo", &mut env.mem);
    let alloc_s = env
        .objc
        .register_host_selector("alloc".to_string(), &mut env.mem);
    let tui: id = msg_send(env, (tui_cls, alloc_s));
    let init_s = env
        .objc
        .register_host_selector("init".to_string(), &mut env.mem);
    let tui: id = msg_send(env, (tui, init_s));
    let set_tuid = env
        .objc
        .register_host_selector("setTaomeeUserID:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (tui, set_tuid, mimi));
    let pwd_ns = crate::frameworks::foundation::ns_string::from_rust_string(env, pwd);
    let set_pwd = env
        .objc
        .register_host_selector("setTaomeePasswordOfUserID:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (tui, set_pwd, pwd_ns));
    // [扫描修 2026-09-15] F10-7 -[TaomeeUserInfo setTaomeePasswordOfUserID:]@0x692b8 用 [[NSString alloc] initWithString:]
    //   自存副本(0x692fa-0x69314),不持有参数 → 释放 +1。
    release(env, pwd_ns);
    let set_tui = env
        .objc
        .register_host_selector("setTaomeeUserInfo:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (gd, set_tui, tui));
    let rel = env
        .objc
        .register_host_selector("release".to_string(), &mut env.mem);
    let _: () = msg_send(env, (tui, rel));
    // Step 2 / Plan A — drive the game's GENUINE passport-success path instead of out-of-band
    // connect2Server. Call the live MainMenuScene's onTaomeeLoginViewDidUnloadWithUserID:password:
    // returnCode:0. In the cold (not-yet-connected) state this ARMS the scene (+235=1) and runs
    // setState:2 + establishConnection — exactly the native cold-start. PHASE 2 (top of this fn)
    // re-fires it at state 4 so its state==4 branch sets delegateLoginMainMenu + sends the native
    // login. The genuine state machine then runs: 1234(sendFlag=1234→byte_B409B0)/1001 replies →
    // onLoginMainMenuCommandReceived: → onButtonPlaySelected:→OnLoginOk→showWithTarget:4 → village.
    // (connect2Server is a FriendsVillageLayer helper; it set delegateGameData but NOT the scene's
    // armed flag / delegateLoginMainMenu, which is why hand-wiring those looped — RE-confirmed.)
    let pwd_unload = std::env::var("MOLE_PASSWORD").unwrap_or_default();
    fire_passport_unload(env, scene, mimi, &pwd_unload);
    log!(
        "[MOLECHEAT] 在线:phase1 原生 passport 回调(冷态 arm 场景 + establishConnection),米米号={}",
        mimi
    );
}

/// Fire the game's native Taomee-passport success callback on the live MainMenuScene:
/// `-[MainMenuScene onTaomeeLoginViewDidUnloadWithUserID:password:returnCode:]`@0xb7e78.
/// userID is a NUMERIC uint (matched against GameData.userInfoData.userId), password is an NSString,
/// returnCode 0 = success. Cold → arms scene + establishConnection; at state 4 → delegate + login.
fn fire_passport_unload(env: &mut Environment, scene: id, mimi: u32, pwd: &str) {
    let pw_ns = crate::frameworks::foundation::ns_string::from_rust_string(env, pwd.to_string());
    let sel = env.objc.register_host_selector(
        "onTaomeeLoginViewDidUnloadWithUserID:password:returnCode:".to_string(),
        &mut env.mem,
    );
    let _: () = msg_send(env, (scene, sel, mimi, pw_ns, 0i32));
    // [扫描修 2026-09-15] F10-7 from_rust_string 的 +1 以前从不平衡。原版淘米登录界面传进来的密码串就是 autoreleased,
    //   回调(0xb7e78)只把它交给会自拷副本的 setter,所以这里改成 autorelease——与原版调用方的所有权语义逐字一致,
    //   比立即 release 更稳(不依赖回调内部有没有延后使用)。
    autorelease(env, pw_ns);
}

/// Inject the private server into the serverlist, bypassing the dead HTTP path.
/// The game's `-[TaomeeGetServerIpListManager getServerListWithServiceName:andDelegate:]`
/// fetches `http://mlogin.61.com/ipsvr.fcgi?...&Format=json` via TM_ASIHTTPRequest (CFHTTP,
/// which touchHLE doesn't implement → dead) and parses the JSON array
/// `[{"ip":..,"port":..}]` via `parseData:` into TaomeeServerData. We build that exact JSON
/// for MOLE_SERVER, run the game's OWN `parseData:` to get the array, and hand it to the
/// delegate's `getListSuccAndReturnByArray:`/`getListSucc:` exactly like `requestFinished:`.
fn inject_serverlist(env: &mut Environment, manager: id, delegate: id) {
    let server = match std::env::var("MOLE_SERVER") {
        Ok(s) => s,
        Err(_) => return,
    };
    let (ip, port) = match server.trim().rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (server.trim().to_string(), "7821".to_string()),
    };
    let json = format!("[{{\"ip\":\"{}\",\"port\":\"{}\"}}]", ip, port);
    let json_ns = crate::frameworks::foundation::ns_string::from_rust_string(env, json);
    // NSData via dataUsingEncoding:NSUTF8StringEncoding(4)
    let due = env
        .objc
        .register_host_selector("dataUsingEncoding:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (json_ns, due, 4u32));
    // [扫描修 2026-09-15] F10-7 dataUsingEncoding: 是宿主实现,把字节拷进新 NSData、不持有源串 → 释放 +1。
    release(env, json_ns);
    // Reuse the game's own JSON parser → array of TaomeeServerData.
    let pd = env
        .objc
        .register_host_selector("parseData:".to_string(), &mut env.mem);
    let arr: id = msg_send(env, (manager, pd, data));
    if delegate != nil {
        if env
            .objc
            .object_has_method_named(&env.mem, delegate, "getListSuccAndReturnByArray:")
        {
            let s = env
                .objc
                .register_host_selector("getListSuccAndReturnByArray:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (delegate, s, arr));
        }
        if env
            .objc
            .object_has_method_named(&env.mem, delegate, "getListSucc:")
        {
            let s = env
                .objc
                .register_host_selector("getListSucc:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (delegate, s, data));
        }
    }
    log!(
        "[MOLECHEAT] 在线:已注入 serverlist -> {}:{}(JSON,复用游戏 parseData:)",
        ip,
        port
    );
}

/// Current forced VIP level (for the menu label).
pub fn vip_level() -> i32 {
    VIP_LEVEL.load(O)
}

/// Cycle the forced VIP level 1..=VIP_LEVEL_MAX and make sure force_vip is on so it shows.
pub fn bump_vip_level() {
    let next = if VIP_LEVEL.load(O) >= VIP_LEVEL_MAX { 1 } else { VIP_LEVEL.load(O) + 1 };
    VIP_LEVEL.store(next, O);
    FORCE_VIP.store(true, O);
    log!("[MOLECHEAT] vip_level -> {} (force_vip on)", next);
}

/// Current forced player level (for the menu label; 0 = off).
pub fn level() -> i32 {
    FORCE_LEVEL.load(O)
}

/// Cycle the forced player level 0/10/.../100/0 (one tap = +10; 0 = off). Step
/// of 10 keeps it to a few taps to reach round levels.
pub fn bump_level() {
    let cur = FORCE_LEVEL.load(O);
    let next = if cur >= 100 { 0 } else { cur + 10 };
    FORCE_LEVEL.store(next, O);
    log!("[MOLECHEAT] force_level -> {}", next);
}

/// Whether the magic-password bypass is on (read by the MagicNumberView hook).
pub fn magic_bypass_on() -> bool {
    MAGIC_BYPASS.load(O)
}

/// Whether the Golden Island offline fix is on (read by the Caribbean hooks).
pub fn fix_golden_island_on() -> bool {
    FIX_GOLDEN_ISLAND.load(O)
}

/// Set a single int field on a guest object via its setter, guarding with
/// respondsToSelector first (mirrors the tweak; avoids crashing if a setter is
/// missing on some build).
fn obj_set_int(env: &mut Environment, obj: id, sel_name: &str, v: i32) {
    if env.objc.object_has_method_named(&env.mem, obj, sel_name) {
        let s = env
            .objc
            .register_host_selector(sel_name.to_string(), &mut env.mem);
        let _: () = msg_send(env, (obj, s, v));
    }
}

/// Build (and cache) a local `CaribbeanDiscoveringData` so the Golden Island
/// activity has data offline. The object is constructed once and then left
/// alone (so the game's own sailing progress isn't clobbered on every read);
/// only when GOLDEN_WIN was toggled (CARIBBEAN_DIRTY) are the fields re-applied.
/// Returns nil if the class/init isn't available.
pub fn build_caribbean_data(env: &mut Environment) -> id {
    let mut data = CARIBBEAN_DATA.with(|c| c.get());
    let mut apply = false;
    if data == nil {
        let cls = env
            .objc
            .get_known_class("CaribbeanDiscoveringData", &mut env.mem);
        if cls == nil {
            return nil;
        }
        let alloc_s = env.objc.register_host_selector("alloc".to_string(), &mut env.mem);
        let obj: id = msg_send(env, (cls, alloc_s));
        let init_s = env.objc.register_host_selector("init".to_string(), &mut env.mem);
        let obj: id = msg_send(env, (obj, init_s));
        if obj == nil {
            return nil;
        }
        retain(env, obj);
        CARIBBEAN_DATA.with(|c| c.set(obj));
        data = obj;
        apply = true;
    } else if CARIBBEAN_DIRTY.swap(false, O) {
        apply = true;
    }
    if apply {
        let win = GOLDEN_WIN.load(O);
        obj_set_int(env, data, "setCurIsland:", if win { 5 } else { 1 });
        obj_set_int(env, data, "setDistanceToNext:", if win { 0 } else { 100 });
        obj_set_int(env, data, "setTotleDistance:", 500);
        obj_set_int(env, data, "setCorrectionSoulOfTheSea:", 9999);
        obj_set_int(env, data, "setLeftDaysNum:", 99);
        log!("[MOLECHEAT] built caribbean data (win={})", win);
    }
    data
}

/// Write an `f64` return value into r0:r1 (touchHLE is soft-float, so doubles
/// are returned in the integer register pair, low word first).
fn ret_double(env: &mut Environment, v: f64) {
    let bits = v.to_bits();
    let r = env.cpu.regs_mut();
    r[0] = bits as u32;
    r[1] = (bits >> 32) as u32;
}

/// `[[<class> alloc] init]` for a guest class by name (nil if class missing).
fn island_alloc_init(env: &mut Environment, class_name: &str) -> id {
    let cls = env.objc.get_known_class(class_name, &mut env.mem);
    if cls == nil {
        return nil;
    }
    let alloc_s = env
        .objc
        .register_host_selector("alloc".to_string(), &mut env.mem);
    let obj: id = msg_send(env, (cls, alloc_s));
    let init_s = env
        .objc
        .register_host_selector("init".to_string(), &mut env.mem);
    msg_send(env, (obj, init_s))
}

/// Call a `setFoo:(CGPoint)` setter (struct arg in r2:r3 — ABI verified 2026-06-03).
fn island_set_point(env: &mut Environment, obj: id, sel_name: &str, x: f32, y: f32) {
    if env.objc.object_has_method_named(&env.mem, obj, sel_name) {
        let s = env
            .objc
            .register_host_selector(sel_name.to_string(), &mut env.mem);
        let _: () = msg_send(env, (obj, s, CGPoint { x, y }));
    }
}

/// Call a `setFoo:(double)` setter (f64 arg in r2:r3).
fn island_set_double(env: &mut Environment, obj: id, sel_name: &str, v: f64) {
    if env.objc.object_has_method_named(&env.mem, obj, sel_name) {
        let s = env
            .objc
            .register_host_selector(sel_name.to_string(), &mut env.mem);
        let _: () = msg_send(env, (obj, s, v));
    }
}

/// `dict[key] = [NSMutableArray arrayWithObject:obj]` — the island mapData value
/// is an NSMutableArray wrapping the TMMapData (the renderer fast-enumerates it;
/// see [[feedback_island_mapdata_gate]]), keyed by the decimal-string tile id.
fn island_put(env: &mut Environment, dict: id, key: &'static str, obj: id) {
    if obj == nil {
        return;
    }
    let arr = island_alloc_init(env, "NSMutableArray");
    if arr == nil {
        return;
    }
    let add_s = env
        .objc
        .register_host_selector("addObject:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (arr, add_s, obj));
    let key_ns = crate::frameworks::foundation::ns_string::get_static_str(env, key);
    let set_s = env
        .objc
        .register_host_selector("setObject:forKey:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (dict, set_s, arr, key_ns));
}

/// 同 island_put,但【同 key 已有数组则追加】而非覆盖——放多个同族建筑(如 5 个商店都在 key
/// "28")必须用它,否则 island_put 每次 setObject:forKey: 覆盖,5 个只剩最后 1 个。
fn island_put_append(env: &mut Environment, dict: id, key: &'static str, obj: id) {
    if obj == nil {
        return;
    }
    let key_ns = crate::frameworks::foundation::ns_string::get_static_str(env, key);
    let get_s = env
        .objc
        .register_host_selector("objectForKey:".to_string(), &mut env.mem);
    let mut arr: id = msg_send(env, (dict, get_s, key_ns));
    if arr == nil {
        arr = island_alloc_init(env, "NSMutableArray");
        if arr == nil {
            return;
        }
        let set_s = env
            .objc
            .register_host_selector("setObject:forKey:".to_string(), &mut env.mem);
        let _: () = msg_send(env, (dict, set_s, arr, key_ns));
    }
    let add_s = env
        .objc
        .register_host_selector("addObject:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (arr, add_s, obj));
}

/// Build the offline **default Golden Island** `mapData` (3 buildings) and inject
/// it into `[NewSceneData sharedInstance]` via `setMapData:`, so LoadingHoliday's
/// state-2 gate (which requires `mapData.count > 0`, normally filled by the dead
/// server) passes and the island scene loads. All field values come from a
/// byte-level disassembly of the game's own `-[LoadingHoliday createDefaultMapData]`
/// (0x252508); we hand-construct the dict instead of calling that method because
/// it also fires ~8 NetworkManager pushes that are pointless/risky offline.
// ★【已回滚 load_island_shop_atlases】:进岛 loadNewScene 补加载那 4 个建筑商店图集会把黄金岛
// 渲染搞坏成全绿场地(疑这 4 图集的贴图在 CCTextureCache/帧缓存里覆盖/冲突了岛背景贴图)。补图集
// 要换更安全的时机/方式(只在进建设庄园那刻、且不覆盖岛贴图),留后续。
/// Returns whether injection succeeded.
/// [P1 离线持久化] 黄金岛布局存档路径 = Documents/island_map.dat(与 userinfo.dat 同目录,
/// 走游戏 GameData.pathForDataFile: 解析,与主村存档同一套)。失败回 nil(则持久化静默跳过)。
fn island_map_path(env: &mut Environment) -> id {
    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
    if gd_cls == nil {
        return nil;
    }
    let shared_s = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let gd: id = msg_send(env, (gd_cls, shared_s));
    if gd == nil {
        return nil;
    }
    let pfd = env
        .objc
        .register_host_selector("pathForDataFile:".to_string(), &mut env.mem);
    let fname =
        crate::frameworks::foundation::ns_string::from_rust_string(env, "island_map.dat".to_string());
    let path: id = msg_send(env, (gd, pfd, fname));
    // [审查修 2026-09-13] S1 from_rust_string 返回 +1,以前从不释放 → 每次取路径泄漏一个串(节拍落盘每 1.5s 都会走到)。
    //   反汇编 -[GameData pathForDataFile:]@0x75374 实证:参数只暂存 r4,传给 [文档目录 stringByAppendingPathComponent:]
    //   后返回新串,不保存参数;touchHLE 的 stringByAppendingPathComponent: 也是拷成 Rust 串再新建 autorelease 串,
    //   返回值与参数不是同一对象 → 这里直接 release 安全。
    release(env, fname);
    path
}

/// [P1] 进岛时先试读持久化布局:有效(非空 dict)→ setMapData: 并返 true(跳过默认岛注入)。
/// 坏档/无档/空 → false(回退默认岛)。NSKeyedUnarchiver 已有坏档容错(返 nil 不崩)。
fn load_island_map(env: &mut Environment) -> bool {
    let path = island_map_path(env);
    if path == nil {
        return false;
    }
    let unarch_cls = env.objc.get_known_class("NSKeyedUnarchiver", &mut env.mem);
    if unarch_cls == nil {
        return false;
    }
    let unarch_s = env
        .objc
        .register_host_selector("unarchiveObjectWithFile:".to_string(), &mut env.mem);
    let loaded: id = msg_send(env, (unarch_cls, unarch_s, path));
    if loaded == nil {
        // [深扫修 2026-09-11] #7 以前"无档"与"坏档"混为一谈直接回退默认岛,1.5s 后节拍(离岛/关窗更是无条件)
        //   就用默认岛把坏档覆盖掉、再无恢复可能。现在坏档先改名隔离,隔离不了就本会话禁止覆盖。
        island_note_load_failure(env, path, ISLAND_FILE_MAP, "island_map.dat");
        return false;
    }
    island_note_load_ok(ISLAND_FILE_MAP);
    let count_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let cnt: crate::mem::GuestUSize = msg_send(env, (loaded, count_s));
    if cnt == 0 {
        return false;
    }
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return false;
    }
    let shared_s = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, shared_s));
    if nsd == nil {
        return false;
    }
    let set_s = env
        .objc
        .register_host_selector("setMapData:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (nsd, set_s, loaded));
    log!("[MOLECHEAT] island: 读到持久化布局 island_map.dat(count={}),跳过默认岛", cnt);
    true
}

/// [P1] 退岛时把当前 [NewSceneData mapData] 归档存盘(明文 NSKeyedArchiver,与主村 map.dat 同法)。
/// archive 失败(nil)或空 dict 绝不写文件(避免历史上 36B 空壳坏档崩启动);独立文件,坏了最多回退默认岛。
/// [扫描修 2026-09-15] F10-6 返回本次落盘摘要(没写就 None),由 island_flush 汇总成一行日志;成功时逐文件日志降为 log_dbg!,
///   ok=false 仍用 log!。
fn save_island_map(env: &mut Environment) -> Option<String> {
    // [深扫修 2026-09-11] #7 坏档保护中(读到的 island_map.dat 解档失败且未能隔离)→ 不写,别拿默认岛覆盖它。
    // [审查修 2026-09-13] S1 先判保护位,置位时才取路径(island_flush 每个节拍都走这里,常态零分配、零消息)。
    if (ISLAND_LOAD_FAILED.load(O) & ISLAND_FILE_MAP) != 0 {
        let p = island_map_path(env);
        if island_save_blocked(env, p, ISLAND_FILE_MAP, "island_map.dat") {
            return None;
        }
    }
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return None;
    }
    let shared_s = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, shared_s));
    if nsd == nil {
        return None;
    }
    let md_s = env
        .objc
        .register_host_selector("mapData".to_string(), &mut env.mem);
    let md: id = msg_send(env, (nsd, md_s));
    if md == nil {
        return None;
    }
    let count_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let cnt: crate::mem::GuestUSize = msg_send(env, (md, count_s));
    if cnt == 0 {
        return None; // 没东西可存,留默认岛兜底
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    if arch_cls == nil {
        return None;
    }
    let arch_s = env
        .objc
        .register_host_selector("archivedDataWithRootObject:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (arch_cls, arch_s, md));
    if data == nil {
        return None; // 归档失败,绝不写空壳坏档
    }
    let path = island_map_path(env);
    if path == nil {
        return None;
    }
    let write_s = env
        .objc
        .register_host_selector("writeToFile:atomically:".to_string(), &mut env.mem);
    let ok: bool = msg_send(env, (data, write_s, path, true));
    if ok {
        log_dbg!("[MOLECHEAT] island: 存盘 island_map.dat(count={} ok={})", cnt, ok);
    } else {
        log!("[MOLECHEAT] island: 存盘 island_map.dat(count={} ok={})", cnt, ok);
    }
    Some(format!("存盘 island_map.dat(count={} ok={})", cnt, ok))
}

/// [2026-09-16] A1-02+A2-02 本岛档的沙原碎片是否已改按「原版获取途径」管理:不再白送商店可买的 31006/31008,
/// 31005/31007 只在任务已完成却缺碎片时兜底。置位后由 save_island_userinfo 写进 island_userinfo.dat 的
/// ISLAND_FRAG_BY_QUEST_KEY 键,load_island_userinfo 每次进岛先清零再读回。老版本读档只认固定键,多一个键无影响。
/// ★为什么必须持久化、不能只看 island_fragments.dat 在不在:save_island_fragments 碎片数为 0 时不写文件(防空壳坏档),
///   真新岛档没买碎片、任务没做到 81 就退岛,只会留下 island_map.dat。第二次进岛它和「P4-b 之前的老档」一模一样,
///   又会被当老档补齐 4 块(island_e2e.sh 两进两出的第二进必现),降级等于白做。
static ISLAND_FRAG_BY_QUEST: AtomicBool = AtomicBool::new(false);
const ISLAND_FRAG_BY_QUEST_KEY: &str = "moleSandFragByQuest";

/// 沙原地图碎片兜底(从 build_default 抽出:持久化路径和默认路径都在 load_island_fragments 之后调用)。
/// [扫描修 2026-09-15] F1-3/F5-10 纠错:31005-31008 是「沙原地图碎片Ⅰ-Ⅳ」,火山是 31009-31012(propertyHV 描述实证),
///   以前的函数名/注释/日志都把它叫"火山",错。原版来源本地齐全:
///   · 31006/31008(以及火山 31009/31011)= 岛建设商店 20 贝壳可买(shop_type=1 sub=2);
///   · 31005/31007 = 岛农场任务 81/83 的 rew_potato(-[NewSceneQuest rewardXP:vipGold:buildValue:]@0x32a49c ≥1000 走物品分支
///     → GET_ITEM_FROM_QUEST 框 → addAdventureMapFragment:@0x32a6d6);火山 31010/31012 = 咖啡任务 16/17 的 rew_object。
/// [2026-09-16] A1-02+A2-02 从「每次进岛无条件补 4 块」降级为兜底,分三种情况:
///   (a) 老档:island_fragments.dat 不存在、island_map.dat 存在,且 island_userinfo.dat 里没有 ISLAND_FRAG_BY_QUEST 标记
///       (P4-b 之前的档,或从没正常退岛落盘过)→ 照旧补齐 4 块。否则老档的 31006/31008 会凭空消失,任务又早就做完、
///       31005/31007 不会再发,沙原被锁。补齐后本次退岛会把 4 块写进 island_fragments.dat,下次进岛自然转入 (c)。
///   (b) 真新岛档(两个文件都不存在)/(c) island_fragments.dat 已存在或已有标记:不注入 31006/31008,
///       31005/31007 只按任务进度兜底 done(N) = nextQuestId > N && curQuestId != N(N=81/83),并置位标记。
///       依据:-[NewSceneQuest accept]@0x3289b0 在 0x328a6c setCurQuestId:next、0x328a88 setNextQuestId:next+1;
///       -[NewSceneQuest postFinish]@0x32a2a0 发完奖在 0x32a334 setCurQuestId:0。两处接收者都是
///       -[NewSceneQuest getUserInfoData]@0x328040 = [[NewSceneData sharedInstance] userInfoDataInNewScene],
///       也就是这里读的同一个对象(getter nextQuestId@0x323a04 / curQuestId@0x323a24 都是纯 ivar 读)。
///       所以「任务 N 进行中」(cur==N、next==N+1)不算完成,不提前送。
///       时序:两个调用点都在 build_default_island_mapdata 里 load_island_userinfo 之后,进度已从 island_userinfo.dat 读回;
///       没档时是 init 默认的 nextQuestId=1。不读 NewSceneQuest 单例的 curQuestId(进岛注入时它可能还没初始化)。
///   读不到进度(userInfoDataInNewScene 为 nil,或 nextQuestId<=0)→ 退回全量注入且不置标记,绝不锁死沙原。
///   31006/31008 即便因坏档丢失,也能在岛建设商店重新买到,不会锁死。
fn inject_sandgarden_fragments(env: &mut Environment, nsd: id) {
    let frags_s = island_sel(env, "mapFragments");
    let frags: id = msg_send(env, (nsd, frags_s));
    if frags == nil {
        return;
    }
    // 情况 (a) 判定。标记已置位时不必再发 fileExistsAtPath:。
    let mut legacy = false;
    if !ISLAND_FRAG_BY_QUEST.load(O) {
        let frag_path = island_data_path(env, "island_fragments.dat");
        if !guest_file_exists(env, frag_path) {
            let map_path = island_map_path(env);
            legacy = guest_file_exists(env, map_path);
        }
    }
    let (ids, mode): (Vec<i32>, &str) = if legacy {
        (vec![31005, 31006, 31007, 31008], "老档补齐")
    } else {
        let ui_s = island_sel(env, "userInfoDataInNewScene");
        let ui: id = msg_send(env, (nsd, ui_s));
        let (next, cur): (i32, i32) = if ui != nil {
            let next_s = island_sel(env, "nextQuestId");
            let cur_s = island_sel(env, "curQuestId");
            let next: i32 = msg_send(env, (ui, next_s));
            let cur: i32 = msg_send(env, (ui, cur_s));
            (next, cur)
        } else {
            (0, 0)
        };
        if next <= 0 {
            log!(
                "[MOLECHEAT] island: 读不到岛任务进度(userInfo 为空={} nextQuestId={}),沙原碎片退回全量兜底(不锁死)",
                ui == nil,
                next
            );
            (vec![31005, 31006, 31007, 31008], "读不到任务进度,全量兜底")
        } else {
            ISLAND_FRAG_BY_QUEST.store(true, O);
            let done = |n: i32| next > n && cur != n;
            let mut v = Vec::new();
            if done(81) {
                v.push(31005);
            }
            if done(83) {
                v.push(31007);
            }
            log_dbg!(
                "[MOLECHEAT] island: 沙原碎片按任务进度兜底 nextQuestId={} curQuestId={} → 候选 {:?}",
                next,
                cur,
                v
            );
            (v, "按岛任务 81/83 进度兜底")
        }
    };
    if ids.is_empty() {
        return;
    }
    let num_cls = env.objc.get_known_class("NSNumber", &mut env.mem);
    let nwi = island_sel(env, "numberWithInt:");
    let add_s = island_sel(env, "addObject:");
    let has_s = island_sel(env, "containsObject:");
    let mut added: Vec<i32> = Vec::new();
    for fid in ids {
        let num: id = msg_send(env, (num_cls, nwi, fid));
        let dup: bool = msg_send(env, (frags, has_s, num));
        if !dup {
            let _: () = msg_send(env, (frags, add_s, num));
            added.push(fid);
        }
    }
    // [扫描修 2026-09-15] F10-6 只在真的补进了碎片时打 log!(老档每次进岛碎片都已在,不再重复刷一行)。
    if !added.is_empty() {
        log!(
            "[MOLECHEAT] island: 补注入沙原地图碎片 {} 块 {:?}({},去重)",
            added.len(),
            added,
            mode
        );
    } else {
        log_dbg!("[MOLECHEAT] island: 沙原地图碎片无需补注入({})", mode);
    }
}

/// [P4-b 探险地图碎片持久化] 退岛把 NewSceneData.mapFragments_(玩家买到/已得的探险地图碎片 NSNumber 数组)
/// 归档存 island_fragments.dat。★为什么需要:mapFragments_ 不入 mapData 也不入 userinfo.dat(淘米设计成
/// 服务器权威 cmd addMapFragments/setModMapFragments 上行、纯内存),离线退岛即丢→玩家在建设庄园【买】的
/// 碎片(沙原 31006/31008、火山 31009/31011,可买;addNewObject2Map→addAdventureMapFragment 本地已加)下次进岛全没。
/// 空(0 个)不写文件(避免空壳坏档),与 save_island_map 一致。
/// [扫描修 2026-09-15] F10-6 返回落盘摘要供 island_flush 汇总;成功时逐文件日志降为 log_dbg!,ok=false 仍 log!。
fn save_island_fragments(env: &mut Environment) -> Option<String> {
    // [深扫修 2026-09-11] #7 坏档保护中 → 不写。
    // [审查修 2026-09-13] S1 先判保护位,置位时才取路径(常态零分配)。
    if (ISLAND_LOAD_FAILED.load(O) & ISLAND_FILE_FRAGMENTS) != 0 {
        let p = island_data_path(env, "island_fragments.dat");
        if island_save_blocked(env, p, ISLAND_FILE_FRAGMENTS, "island_fragments.dat") {
            return None;
        }
    }
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return None;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return None;
    }
    let frags_s = env
        .objc
        .register_host_selector("mapFragments".to_string(), &mut env.mem);
    let frags: id = msg_send(env, (nsd, frags_s));
    if frags == nil {
        return None;
    }
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let cnt: crate::mem::GuestUSize = msg_send(env, (frags, cnt_s));
    if cnt == 0 {
        return None;
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    if arch_cls == nil {
        return None;
    }
    let arch_s = env
        .objc
        .register_host_selector("archivedDataWithRootObject:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (arch_cls, arch_s, frags));
    if data == nil {
        return None;
    }
    let path = island_data_path(env, "island_fragments.dat");
    if path == nil {
        return None;
    }
    let write_s = env
        .objc
        .register_host_selector("writeToFile:atomically:".to_string(), &mut env.mem);
    let ok: bool = msg_send(env, (data, write_s, path, true));
    if ok {
        log_dbg!("[MOLECHEAT] island: 存盘 island_fragments.dat(碎片 count={} ok={})", cnt, ok);
    } else {
        log!("[MOLECHEAT] island: 存盘 island_fragments.dat(碎片 count={} ok={})", cnt, ok);
    }
    Some(format!("存盘 island_fragments.dat(碎片 count={} ok={})", cnt, ok))
}

/// [P4-b 探险地图碎片持久化] 进岛读回 island_fragments.dat 里玩家买到的碎片,逐个并入 mapFragments_
/// (containsObject 去重,与 inject_sandgarden_fragments 同法,不发包)。坏档/无档=静默跳过(NSKeyedUnarchiver
/// 已有数值解码容错)。★注:沙原的 31005/31007 商店【不卖】(propertyHV 实证 shop_type=None,原版来源是岛任务 81/83)。
/// 本函数只负责【恢复买到的/已得的】;[2026-09-16] A1-02+A2-02 起 inject_sandgarden_fragments 只做兜底
/// (老档补齐 4 块,其余只补任务 81/83 已完成却缺的 31005/31007),规则见该函数注释。
/// [扫描修 2026-09-15] F1-3/F5-10 纠错:以前这里写成"火山必需",实为沙原碎片;火山 31010/31012 来自咖啡任务 16/17。
fn load_island_fragments(env: &mut Environment) {
    let path = island_data_path(env, "island_fragments.dat");
    if path == nil {
        return;
    }
    let unarch_cls = env.objc.get_known_class("NSKeyedUnarchiver", &mut env.mem);
    if unarch_cls == nil {
        return;
    }
    let unarch_s = env
        .objc
        .register_host_selector("unarchiveObjectWithFile:".to_string(), &mut env.mem);
    let loaded: id = msg_send(env, (unarch_cls, unarch_s, path));
    if loaded == nil {
        // [深扫修 2026-09-11] #7 区分无档/坏档(坏档隔离或禁止覆盖)。
        island_note_load_failure(env, path, ISLAND_FILE_FRAGMENTS, "island_fragments.dat");
        return;
    }
    island_note_load_ok(ISLAND_FILE_FRAGMENTS);
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let n: crate::mem::GuestUSize = msg_send(env, (loaded, cnt_s));
    if n == 0 {
        return;
    }
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return;
    }
    let frags_s = env
        .objc
        .register_host_selector("mapFragments".to_string(), &mut env.mem);
    let frags: id = msg_send(env, (nsd, frags_s));
    if frags == nil {
        return;
    }
    let oai = env
        .objc
        .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
    let has_s = env
        .objc
        .register_host_selector("containsObject:".to_string(), &mut env.mem);
    let add_s = env
        .objc
        .register_host_selector("addObject:".to_string(), &mut env.mem);
    let mut restored = 0i32;
    for i in 0..n {
        let num: id = msg_send(env, (loaded, oai, i));
        if num == nil {
            continue;
        }
        let dup: bool = msg_send(env, (frags, has_s, num));
        if !dup {
            let _: () = msg_send(env, (frags, add_s, num));
            restored += 1;
        }
    }
    if restored > 0 {
        log!(
            "[MOLECHEAT] island: 读回 island_fragments.dat 恢复 {} 个买到的碎片",
            restored
        );
    }
}

/// [P5 地基] 确保 NewSceneData.userInfoDataInNewScene 存在 —— NPC(createAllNpcs)/任务(NewSceneQuest)/
/// 剧情(NewSceneStory)/成就 全靠它当【本地载体】。离线首进岛它可能为 nil(原版靠 1001 回包填,离线无)
/// → 这些系统无处挂。nil 则 alloc-init 一个(init 默认 nextQuestId=1/nextStoryId=1/extendMap=1/空 npcs+
/// achieveDict),内容系统即有载体,并能随 userinfo.dat 持久(saveUserinfoToLocal)。
fn ensure_island_userinfo(env: &mut Environment, nsd: id) {
    let ui_s = env
        .objc
        .register_host_selector("userInfoDataInNewScene".to_string(), &mut env.mem);
    let ui: id = msg_send(env, (nsd, ui_s));
    if ui != nil {
        return;
    }
    let uic = env.objc.get_known_class("NewSceneUserInfoData", &mut env.mem);
    if uic == nil {
        return;
    }
    let alloc_s = env
        .objc
        .register_host_selector("alloc".to_string(), &mut env.mem);
    let init_s = env
        .objc
        .register_host_selector("init".to_string(), &mut env.mem);
    let newui: id = msg_send(env, (uic, alloc_s));
    let newui: id = msg_send(env, (newui, init_s));
    if newui == nil {
        return;
    }
    // ★C1 修复:userInfoDataInNewScene 是 readonly ivar 直返、【无 setter】(IDA 实证 getter@0x223cf4
    // 从 _OBJC_IVAR_$_NewSceneData.userInfoDataInNewScene_ 读偏移=4)。原来 msg setUserInfoDataInNewScene:
    // 是【不存在的 selector】→ touchHLE no-op 静默丢弃 → ivar 仍 nil、新对象泄漏、内容持久化整条失效。
    // 改直写 ivar(self+4):alloc-init 的 +1 转给 ivar(NewSceneData dealloc 时 -1 平衡)。
    let slot: crate::mem::MutPtr<u32> = crate::mem::Ptr::from_bits(nsd.to_bits() + 4);
    env.mem.write(slot, newui.to_bits());
    log!("[MOLECHEAT] island: 补建 NewSceneUserInfoData(直写 ivar self+4,载体挂上)");
}

/// [P5 内容持久化] 通用存档路径 = Documents/<fname>(走 GameData.pathForDataFile:)。失败回 nil。
fn island_data_path(env: &mut Environment, fname: &str) -> id {
    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
    if gd_cls == nil {
        return nil;
    }
    let shared_s = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let gd: id = msg_send(env, (gd_cls, shared_s));
    if gd == nil {
        return nil;
    }
    let pfd = env
        .objc
        .register_host_selector("pathForDataFile:".to_string(), &mut env.mem);
    let f = crate::frameworks::foundation::ns_string::from_rust_string(env, fname.to_string());
    let path: id = msg_send(env, (gd, pfd, f));
    // [审查修 2026-09-13] S1 释放 +1 临时串(理由同 island_map_path:pathForDataFile:@0x75374 不保存参数,返回的是新串)。
    release(env, f);
    path
}

/// [深扫修 2026-09-11] #7/#3 guest 文件是否存在:[[NSFileManager defaultManager] fileExistsAtPath:]。
/// 用来区分"文件不存在"与"文件存在但解档为 nil"——NSKeyedUnarchiver unarchiveObjectWithFile: 两种情况都返回 nil。
fn guest_file_exists(env: &mut Environment, path: id) -> bool {
    if path == nil {
        return false;
    }
    let fm_cls = env.objc.get_known_class("NSFileManager", &mut env.mem);
    if fm_cls == nil {
        return false;
    }
    let dm = island_sel(env, "defaultManager");
    let fm: id = msg_send(env, (fm_cls, dm));
    if fm == nil {
        return false;
    }
    let fe = island_sel(env, "fileExistsAtPath:");
    msg_send(env, (fm, fe, path))
}

/// [深扫修 2026-09-11] #7/#3 把坏档改名隔离:<path>.corrupt(已存在则 <path>.corrupt-<unix秒>,绝不覆盖更早的隔离件)。
/// 走 [NSFileManager moveItemAtPath:toPath:error:];error 传 nil(touchHLE 实现里只有 error 非空才会走 todo!)。
/// 源文件都在 Documents(可写节点),不触发 fs.rename 里的 writeable 断言。
/// 返回"原路径上已经没有这份坏档"(= 数据已安全转移),调用方据此决定能否解除落盘保护。
fn quarantine_corrupt_file(env: &mut Environment, path: id) -> bool {
    if path == nil {
        return false;
    }
    let src = crate::frameworks::foundation::ns_string::to_rust_string(env, path).into_owned();
    let mut dst = format!("{}.corrupt", src);
    let probe = crate::frameworks::foundation::ns_string::from_rust_string(env, dst.clone());
    // [审查修 2026-09-13] S1 probe/dst_ns 都是 from_rust_string 的 +1 临时串,以前不释放。fileExistsAtPath: /
    //   moveItemAtPath:toPath:error: 的宿主实现都只把参数拷成 Rust 串、不持有对象,用完立即 release(释放后不再使用)。
    let probe_exists = guest_file_exists(env, probe);
    release(env, probe);
    if probe_exists {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        dst = format!("{}.corrupt-{}", src, secs);
    }
    let fm_cls = env.objc.get_known_class("NSFileManager", &mut env.mem);
    if fm_cls == nil {
        return false;
    }
    let dm = island_sel(env, "defaultManager");
    let fm: id = msg_send(env, (fm_cls, dm));
    if fm == nil {
        return false;
    }
    let dst_ns = crate::frameworks::foundation::ns_string::from_rust_string(env, dst.clone());
    let mv = island_sel(env, "moveItemAtPath:toPath:error:");
    // error 形参是 NSError**(宿主签名 MutPtr<id>):必须传空指针而不是 nil(id),
    // 宿主方法按完整签名做类型校验,类型不符会直接 panic(坏档注入实测 T2 复现过)。
    let no_error: MutPtr<id> = Ptr::null();
    let ok: bool = msg_send(env, (fm, mv, path, dst_ns, no_error));
    release(env, dst_ns); // [审查修 2026-09-13] S1 见上
    let moved = ok && !guest_file_exists(env, path);
    log!("[MOLECHEAT] 坏档隔离:{} → {}(成功={})", src, dst, moved);
    moved
}

/// [深扫修 2026-09-11] #7 岛档解档为 nil 时的分类处理(规则见 ISLAND_LOAD_FAILED 注释)。
///   · 文件不存在 = 正常无档(首进岛/从没存过),清位返回,调用方照旧回退默认值;
///   · 文件存在 = 坏档/写残:置位 → 尝试改名隔离;隔离成功立即清位(数据已保住),失败则保持置位、本会话不覆盖它。
fn island_note_load_failure(env: &mut Environment, path: id, bit: u32, fname: &str) {
    if !guest_file_exists(env, path) {
        ISLAND_LOAD_FAILED.fetch_and(!bit, O);
        return;
    }
    log!(
        "[MOLECHEAT] island: ⚠️ {} 存在但解档失败(坏档/写残)→ 不当作无档直接覆盖,先隔离保留",
        fname
    );
    ISLAND_LOAD_FAILED.fetch_or(bit, O);
    if quarantine_corrupt_file(env, path) {
        ISLAND_LOAD_FAILED.fetch_and(!bit, O);
        log!(
            "[MOLECHEAT] island: {} 已改名保留为 .corrupt,本次按无档处理(可手动改回原名恢复)",
            fname
        );
        // [审查修 2026-09-13] D3 布局档隔离成功 → 船档一并隔离,两份同进退。
        //   根因:island_ships.dat 描述的是 island_map.dat 里那批船/咖啡馆,只在布局读档成功分支(load_island_ships)读回;
        //   布局隔离成功清掉 MAP 位后,save_island_ships 的 MAP 位保护与 SHIPS 保护位都放行,默认岛自带的 1 艘默认船
        //   (34001)在首个节拍/离岛/关窗落盘时就覆盖原船档 → 玩家把 .corrupt 改回原名后 shipState/待领奖品/咖啡馆 isNew 全丢。
        //   取舍:不改成"布局读档失败的会话一律不写船档"(否则默认岛的船每次重进都退回坏船);
        //   隔离失败时置 SHIPS 保护位(与其它岛档"隔离不了就本会话禁止覆盖"同一规则),代价仅是本会话默认岛船状态不落盘。
        //   island_fragments.dat 不动:默认岛路径同样 load_island_fragments 读回并去重并入,不会被默认数据覆盖。
        if bit == ISLAND_FILE_MAP {
            let sp = island_data_path(env, "island_ships.dat");
            if guest_file_exists(env, sp) {
                if quarantine_corrupt_file(env, sp) {
                    ISLAND_LOAD_FAILED.fetch_and(!ISLAND_FILE_SHIPS, O);
                    log!("[MOLECHEAT] island: island_ships.dat 已随布局档一并改名保留 → 恢复时 island_map.dat 与 island_ships.dat 两份隔离件需一起改回原名(船/咖啡馆状态存在船档里)");
                } else {
                    ISLAND_LOAD_FAILED.fetch_or(ISLAND_FILE_SHIPS, O);
                    log!("[MOLECHEAT] island: island_ships.dat 随布局档隔离失败 → 本会话暂停覆盖船档(默认岛的船状态本会话不落盘)");
                }
            }
        }
    } else {
        log!(
            "[MOLECHEAT] island: {} 改名隔离失败 → 本会话暂停覆盖这份文件(节拍/离岛/关窗落盘都跳过它)",
            fname
        );
    }
}

/// [深扫修 2026-09-11] #7 岛档解档成功:解除该文件的坏档保护。
fn island_note_load_ok(bit: u32) {
    ISLAND_LOAD_FAILED.fetch_and(!bit, O);
}

/// [深扫修 2026-09-11] #7 落盘前检查:该岛档是否处于坏档保护中(是 → 调用方跳过写这份文件)。
/// 原路径上的文件已经不在了(玩家手动处理)就解除保护、恢复落盘。
fn island_save_blocked(env: &mut Environment, path: id, bit: u32, fname: &str) -> bool {
    if (ISLAND_LOAD_FAILED.load(O) & bit) == 0 {
        return false;
    }
    if !guest_file_exists(env, path) {
        ISLAND_LOAD_FAILED.fetch_and(!bit, O);
        log!(
            "[MOLECHEAT] island: {} 原路径上的坏档已不在(被手动处理)→ 解除保护、恢复落盘",
            fname
        );
        return false;
    }
    if (ISLAND_BLOCK_LOGGED.fetch_or(bit, O) & bit) == 0 {
        log!(
            "[MOLECHEAT] island: 跳过落盘 {}(原路径仍是未隔离的坏档,绝不用当前内存里的默认数据覆盖)",
            fname
        );
    }
    true
}

/// [深扫修 2026-09-11] #3 游戏层兜底(兼 #2 截断档兜底):-[GameData loadUserInfoData] 的前置钩子。
/// 调用方负责快照/恢复 r0-r3(这里要发十几条消息)。
/// #3 根因:偏好 plist(Library/Preferences/com.taomee.MoleWorld.plist)丢失或写残时 NSUserDefaults 退回空字典,
///   -[GameSettings loadSettings] 读到 isEncrypt=NO / EV130=NO;只要 userinfo.dat 存在,loadUserInfoData 在 0x757c6
///   就跳 0x75936 弹 HACK_USERINFO_DATA_ERROR(delegate=self),touchHLE 自动关框 → -[GameData alertView:clickedButtonAtIndex:]
///   @0x754b4 直接 exit(0)。存档完好却每次启动秒退、无任何提示。
/// 修法:仅当 isEncrypt=NO、userinfo.dat 存在、长度符合 5.5.0 格式(密文 16 字节整数倍 + 4 字节 + 16 字节 md5)、
///   且【复用原版 -[GameData CheckUserInfoData:]】(0x754c0 → checkUserinfoMd5:)自校验通过时,补 setIsEncrypt:YES +
///   setEncryVersion:YES + saveSettings。5.5.0 只会写这种加密档(-[NewSceneData saveUserinfoToLocal] 0x21de92 同样
///   先 setEncryVersion:1/setIsEncrypt:1),所以补写是忠实的;两个必须同时补(只补 isEncrypt 会走 0x7584c 不去尾解密、静默失败)。
///   不自己重算 md5(免得算法不一致);不吞 exit(否则带着空 UserInfoData 继续跑,自动存档会用默认值覆盖好档)。
///   CheckUserInfoData: 有副作用:入口把 isHackData_(ivar 槽 0xb038c4)清 0、md5 失败再置 1;失败时这里把它恢复原值,
///   不给后续流程留下我们造成的"作弊"标记。成功时真方法紧接着自己也会调一次、结果相同。
/// #2 兜底:游戏自己写档长度必 ≥20(0x7563e/0x75652 追加 4+16 字节),<20 只可能是写一半被截断;这种文件在
///   checkUserinfoMd5: 里 len-16 下溢,<16 时 touchHLE 宿主直接 panic(每次启动崩溃循环),16~19 则走原版删档分支把
///   完好的 map.dat 一起删掉。这里把它改名为 userinfo.dat.corrupt 隔离:游戏按"无 userinfo"启动,map.dat 保住。
///   (≥20 字节但 md5 不符的档仍交给原版反作弊分支处理,不在这里改语义。)
fn guard_userinfo_before_load(env: &mut Environment, gd: id) {
    if gd == nil {
        return;
    }
    let path: id = {
        let pfd = island_sel(env, "pathForDataFile:");
        let f = crate::frameworks::foundation::ns_string::from_rust_string(
            env,
            "userinfo.dat".to_string(),
        );
        let p: id = msg_send(env, (gd, pfd, f));
        // [审查修 2026-09-13] S1 释放 +1 临时串(每次 -[GameData loadUserInfoData] 都会走到;
        //   pathForDataFile:@0x75374 不保存参数、返回新串,释放安全)。
        release(env, f);
        p
    };
    if path == nil || !guest_file_exists(env, path) {
        return; // 无档:原版自己走新档路径
    }
    let data_cls = env.objc.get_known_class("NSData", &mut env.mem);
    if data_cls == nil {
        return;
    }
    let dwc = island_sel(env, "dataWithContentsOfFile:");
    let data: id = msg_send(env, (data_cls, dwc, path));
    if data == nil {
        return;
    }
    let len_s = island_sel(env, "length");
    let len: crate::mem::GuestUSize = msg_send(env, (data, len_s));
    if len < 20 {
        log!(
            "[MOLECHEAT] ⚠️ userinfo.dat 只有 {} 字节(游戏自己写档必 ≥20,判定为写一半被截断)→ 改名隔离,防止 checkUserinfoMd5: 崩溃循环/连带删 map.dat",
            len
        );
        let _ = quarantine_corrupt_file(env, path);
        return;
    }
    let gs_cls = env.objc.get_known_class("GameSettings", &mut env.mem);
    if gs_cls == nil {
        return;
    }
    let sh = island_sel(env, "sharedInstance");
    let gs: id = msg_send(env, (gs_cls, sh));
    if gs == nil {
        return;
    }
    let ie = island_sel(env, "isEncrypt");
    let is_enc: u8 = msg_send(env, (gs, ie));
    if is_enc != 0 {
        return; // 偏好正常,零干预
    }
    if (len - 20) % 16 != 0 {
        log!(
            "[MOLECHEAT] userinfo.dat 长度 {} 不符合 5.5.0 加密档格式,isEncrypt=NO 时不补偏好(交给原版处理)",
            len
        );
        return;
    }
    // 快照 isHackData_(GameData BOOL ivar,偏移从 _OBJC_IVAR 槽现读)。
    let hack_off: u32 = env.mem.read(ConstPtr::<u32>::from_bits(0xb038c4));
    let hack_ptr: Option<MutPtr<u8>> = if hack_off != 0 && hack_off < 0x1000 {
        Some(Ptr::from_bits(gd.to_bits() + hack_off))
    } else {
        None
    };
    let hack_before: Option<u8> = match hack_ptr {
        Some(p) => Some(env.mem.read(p)),
        None => None,
    };
    let cud = island_sel(env, "CheckUserInfoData:");
    // 方法类型串 i12@0:4@8(返回 int):0=校验失败,非 0=通过(0x754ea-0x754f2)。
    let ok: i32 = msg_send(env, (gd, cud, data));
    if ok == 0 {
        if let (Some(p), Some(v)) = (hack_ptr, hack_before) {
            env.mem.write(p, v);
        }
        log!(
            "[MOLECHEAT] 偏好缺失(isEncrypt=NO)但 userinfo.dat md5 自校验未通过 → 不补偏好(交给原版处理)"
        );
        return;
    }
    let sie = island_sel(env, "setIsEncrypt:");
    let _: () = msg_send(env, (gs, sie, true));
    let sev = island_sel(env, "setEncryVersion:");
    let _: () = msg_send(env, (gs, sev, true));
    let ss = island_sel(env, "saveSettings");
    let _: () = msg_send(env, (gs, ss));
    log!(
        "[MOLECHEAT] ⚠️ 偏好 plist 缺 isEncrypt/EV130,但 userinfo.dat({} 字节)md5 自校验通过 → 补 isEncrypt=YES/EV130=YES 并 saveSettings,避免原版弹框 exit(0)",
        len
    );
}

/// [P5 内容持久化命门] 黄金岛专属进度(任务/剧情/成就/扩地/建设值/NPC)淘米设计成【服务器权威+纯内存】:
/// NewSceneUserInfoData 无 NSCoding、无本地存读,saveUserinfoToLocal 存的是另一个对象(主庄园 UserInfoData)。
/// → 离线退岛即丢。这里自建 island_userinfo.dat:退岛把岛 userInfo 标量字段 + npcs(NpcData 有 NSCoding)
/// + achieveAlreadyUnlock(标准 NSMutableDict)塞进一个 dict 整体 NSKeyedArchiver 归档落盘。
/// [扫描修 2026-09-15] F10-6 返回落盘摘要供 island_flush 汇总(没写就 None);F10-7 固定键名一律用 get_static_str
///   (零分配、永不释放,也就不存在释放时机问题),以前每个键 from_rust_string 一个 +1 串从不释放、每次落盘泄漏约 10 个。
fn save_island_userinfo(env: &mut Environment) -> Option<String> {
    // [深扫修 2026-09-11] #7 坏档保护中 → 不写(否则 init 默认的任务/剧情/扩地进度会覆盖玩家的档)。
    // [审查修 2026-09-13] S1 先判保护位,置位时才取路径(常态零分配)。
    if (ISLAND_LOAD_FAILED.load(O) & ISLAND_FILE_USERINFO) != 0 {
        let p = island_data_path(env, "island_userinfo.dat");
        if island_save_blocked(env, p, ISLAND_FILE_USERINFO, "island_userinfo.dat") {
            return None;
        }
    }
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return None;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return None;
    }
    let ui_s = env
        .objc
        .register_host_selector("userInfoDataInNewScene".to_string(), &mut env.mem);
    let ui: id = msg_send(env, (nsd, ui_s));
    if ui == nil {
        return None;
    }
    let dict = island_alloc_init(env, "NSMutableDictionary");
    if dict == nil {
        return None;
    }
    let num_cls = env.objc.get_known_class("NSNumber", &mut env.mem);
    let nwi = env
        .objc
        .register_host_selector("numberWithInt:".to_string(), &mut env.mem);
    let sfk = env
        .objc
        .register_host_selector("setObject:forKey:".to_string(), &mut env.mem);
    // 标量 int 字段
    for key in [
        "nextQuestId",
        "curQuestId",
        "nextStoryId",
        "extendMap",
        "buildValue",
        "curTotalWorkersCount",
        "curIdleWorkerCount",
    ] {
        let g = env.objc.register_host_selector(key.to_string(), &mut env.mem);
        let v: i32 = msg_send(env, (ui, g));
        let num: id = msg_send(env, (num_cls, nwi, v));
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, key);
        let _: () = msg_send(env, (dict, sfk, num, k));
    }
    // [2026-09-16] A1-02+A2-02 沙原碎片「按任务进度兜底」标记(见 ISLAND_FRAG_BY_QUEST)。只在置位时写,
    //   没有这个键 = 老档语义;老版本读档只认固定键,多一个键无影响。
    if ISLAND_FRAG_BY_QUEST.load(O) {
        let num: id = msg_send(env, (num_cls, nwi, 1i32));
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, ISLAND_FRAG_BY_QUEST_KEY);
        let _: () = msg_send(env, (dict, sfk, num, k));
    }
    // curQuestResult 是 double
    {
        let g = env
            .objc
            .register_host_selector("curQuestResult".to_string(), &mut env.mem);
        let v: f64 = msg_send(env, (ui, g));
        let nwd = env
            .objc
            .register_host_selector("numberWithDouble:".to_string(), &mut env.mem);
        let num: id = msg_send(env, (num_cls, nwd, v));
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, "curQuestResult");
        let _: () = msg_send(env, (dict, sfk, num, k));
    }
    // 对象字段 npcs(NSMutableArray<NpcData>)/ achieveAlreadyUnlock(NSMutableDict)整体入 dict,
    // 随 NSKeyedArchiver 递归归档(NpcData 有 encodeWithCoder、字典 keyed-archive 往返已支持)。
    for key in ["npcs", "achieveAlreadyUnlock"] {
        let g = env.objc.register_host_selector(key.to_string(), &mut env.mem);
        let o: id = msg_send(env, (ui, g));
        if o != nil {
            let k = crate::frameworks::foundation::ns_string::get_static_str(env, key);
            let _: () = msg_send(env, (dict, sfk, o, k));
        }
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    if arch_cls == nil {
        // [2026-09-16 黄金岛审查修] dict 是本函数 island_alloc_init 出来的 +1,四个出口原来全都直接 return,
        //   于是岛上每 1.5 秒的节拍落盘就泄漏一个 NSMutableDictionary 连同里面约 10 个 NSNumber。
        release(env, dict);
        return None;
    }
    let arch_s = env
        .objc
        .register_host_selector("archivedDataWithRootObject:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (arch_cls, arch_s, dict));
    // dict 的最后一次使用就是上面这次归档,归档结果 data 与它无所有权关系 → 这里放掉,
    // 后面 data==nil / path==nil / 正常写盘三条出口就都平衡了。
    release(env, dict);
    if data == nil {
        return None;
    }
    let path = island_data_path(env, "island_userinfo.dat");
    if path == nil {
        return None;
    }
    let write_s = env
        .objc
        .register_host_selector("writeToFile:atomically:".to_string(), &mut env.mem);
    let ok: bool = msg_send(env, (data, write_s, path, true));
    if ok {
        log_dbg!("[MOLECHEAT] island: 存盘 island_userinfo.dat(任务/剧情/成就/扩地 ok={})", ok);
    } else {
        log!("[MOLECHEAT] island: 存盘 island_userinfo.dat(任务/剧情/成就/扩地 ok={})", ok);
    }
    Some(format!("存盘 island_userinfo.dat(ok={})", ok))
}

/// [P5] 进岛读回 island_userinfo.dat,覆盖到岛 userInfo(在 server-fed/默认值之后、渲染之前)。
fn load_island_userinfo(env: &mut Environment) -> bool {
    // [2026-09-16] A1-02+A2-02 每次进岛先清零沙原碎片标记,只由本次读到的档决定(无档/坏档 = 未置位),
    //   防止上一个岛会话的值串到删档重建或换档之后。
    ISLAND_FRAG_BY_QUEST.store(false, O);
    let path = island_data_path(env, "island_userinfo.dat");
    if path == nil {
        return false;
    }
    let unarch_cls = env.objc.get_known_class("NSKeyedUnarchiver", &mut env.mem);
    if unarch_cls == nil {
        return false;
    }
    let unarch_s = env
        .objc
        .register_host_selector("unarchiveObjectWithFile:".to_string(), &mut env.mem);
    let dict: id = msg_send(env, (unarch_cls, unarch_s, path));
    if dict == nil {
        // [深扫修 2026-09-11] #7 区分无档/坏档(坏档隔离或禁止覆盖)。
        island_note_load_failure(env, path, ISLAND_FILE_USERINFO, "island_userinfo.dat");
        return false;
    }
    island_note_load_ok(ISLAND_FILE_USERINFO);
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return false;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return false;
    }
    let ui_s = env
        .objc
        .register_host_selector("userInfoDataInNewScene".to_string(), &mut env.mem);
    let ui: id = msg_send(env, (nsd, ui_s));
    if ui == nil {
        return false;
    }
    let ofk = env
        .objc
        .register_host_selector("objectForKey:".to_string(), &mut env.mem);
    let iv = env
        .objc
        .register_host_selector("intValue".to_string(), &mut env.mem);
    for (setter, key) in [
        ("setNextQuestId:", "nextQuestId"),
        ("setCurQuestId:", "curQuestId"),
        ("setNextStoryId:", "nextStoryId"),
        ("setExtendMap:", "extendMap"),
        ("setBuildValue:", "buildValue"),
        ("setCurTotalWorkersCount:", "curTotalWorkersCount"),
    ] {
        // [扫描修 2026-09-15] F10-7 固定键名改 get_static_str(以前每键一个从不释放的 +1 串,每次进岛泄漏约 9 个)。
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, key);
        let num: id = msg_send(env, (dict, ofk, k));
        if num != nil {
            let v: i32 = msg_send(env, (num, iv));
            let s = env.objc.register_host_selector(setter.to_string(), &mut env.mem);
            let _: () = msg_send(env, (ui, s, v));
        }
    }
    // [2026-09-16] A1-02+A2-02 读回沙原碎片「按任务进度兜底」标记(见 ISLAND_FRAG_BY_QUEST;函数开头已清零)。
    {
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, ISLAND_FRAG_BY_QUEST_KEY);
        let num: id = msg_send(env, (dict, ofk, k));
        if num != nil {
            let v: i32 = msg_send(env, (num, iv));
            ISLAND_FRAG_BY_QUEST.store(v != 0, O);
        }
    }
    {
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, "curQuestResult");
        let num: id = msg_send(env, (dict, ofk, k));
        if num != nil {
            let dv = env
                .objc
                .register_host_selector("doubleValue".to_string(), &mut env.mem);
            let v: f64 = msg_send(env, (num, dv));
            // [审计修] 打工类任务(questType 7)的开始时刻;4294967295.0 哨兵与计数值由 cf_fix_residue 自动避开。
            let v = cf_fix_residue(v, now_cf_secs()).unwrap_or(v);
            let s = env
                .objc
                .register_host_selector("setCurQuestResult:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (ui, s, v));
        }
    }
    for (setter, key, needs_retain) in [
        // ★ -[NewSceneUserInfoData setNpcs:]@0x323ac0 是**裸赋值**(`str r2,[r0,r1]; bx lr`,属性
        //   `T@"NSMutableArray",N,Vnpcs_` 没有 `&`)→ 不 retain。而这里给它的数组是解档出来的、
        //   只被 NSKeyedUnarchiver 持有;解档器一 dealloc(或它自己的对象表被释放)数组就没了,
        //   岛上 NPC 数据变野指针。以前不崩只是因为解档器本身泄漏、从来没 dealloc 过——那个泄漏
        //   现在已修(ns_keyed_unarchiver.rs unarchiveObjectWithData:),这里必须自己补上所有权。
        //   [2026-09-16 黄金岛审查修]
        ("setNpcs:", "npcs", true),
        // -[NewSceneUserInfoData setAchieveAlreadyUnlock:]@0x323ae0 走 _objc_setProperty(属性带 `&`),
        //   自带 retain,不能再补一次,否则泄漏。
        ("setAchieveAlreadyUnlock:", "achieveAlreadyUnlock", false),
    ] {
        let k = crate::frameworks::foundation::ns_string::get_static_str(env, key); // [扫描修 2026-09-15] F10-7
        let o: id = msg_send(env, (dict, ofk, k));
        if o != nil {
            if needs_retain {
                retain(env, o);
            }
            let s = env.objc.register_host_selector(setter.to_string(), &mut env.mem);
            let _: () = msg_send(env, (ui, s, o));
        }
    }
    // ★[审计修 2026-09-11] 空闲工人数不回读,一律从总数起算。原版每次进岛由服务器 1062 重新下发空闲数
    //   (parseMapDataWithPackageData: setCurIdleWorkerCount: @0x22b18e);而进岛加载时游戏会把所有持久占用自己重扣一遍:
    //   [NewGameManager loadMapObjects:] 的 subAvailableWorker(0x24347e,商店卖货)、DiscoveryShip initWithMapData 的
    //   changeAvailableMolerForTask:(0x360e5c,出海)、endLoadMap 后 1 秒调度的 createIdleWorkers:(进行中的岛任务/每日/
    //   咖啡任务 minusNeededWorkers,0x242008)。离线回读的是"已扣过"的值 → 每进一次岛再扣一遍,实测空闲数 1→0→-1。
    {
        let tot = island_sel(env, "curTotalWorkersCount");
        let t: i32 = msg_send(env, (ui, tot));
        let set_idle = island_sel(env, "setCurIdleWorkerCount:");
        let _: () = msg_send(env, (ui, set_idle, t));
        log!("[MOLECHEAT] island: 空闲工人数从总数起算 = {}(占用由游戏加载时自行重扣)", t);
    }
    // [审计修] 成就解锁时间与 NPC 冷却里的 unix 纪元残留(09-06 旧时钟修复写入)。
    {
        let now_cf = now_cf_secs();
        let s_ach = island_sel(env, "achieveAlreadyUnlock");
        let s_keys = island_sel(env, "allKeys");
        let s_cnt = island_sel(env, "count");
        let s_oai = island_sel(env, "objectAtIndex:");
        let s_num = island_sel(env, "numberWithInt:");
        let s_sfk = island_sel(env, "setObject:forKey:");
        let s_npcs = island_sel(env, "npcs");
        let s_lcd = island_sel(env, "lastCoolDownTime");
        let s_slcd = island_sel(env, "setLastCoolDownTime:");
        let num_cls = env.objc.get_known_class("NSNumber", &mut env.mem);
        let ach: id = msg_send(env, (ui, s_ach));
        if ach != nil && env.objc.object_has_method_named(&env.mem, ach, "setObject:forKey:") {
            let keys: id = msg_send(env, (ach, s_keys));
            let n: crate::mem::GuestUSize = if keys != nil { msg_send(env, (keys, s_cnt)) } else { 0 };
            for i in 0..n {
                let key: id = msg_send(env, (keys, s_oai, i));
                let val: id = msg_send(env, (ach, ofk, key));
                if val == nil || !env.objc.object_has_method_named(&env.mem, val, "intValue") {
                    continue;
                }
                let v: i32 = msg_send(env, (val, iv));
                if let Some(nv) = cf_fix_residue(v as f64, now_cf) {
                    let num: id = msg_send(env, (num_cls, s_num, nv as i32));
                    let _: () = msg_send(env, (ach, s_sfk, num, key));
                    log!("[MOLECHEAT] island: 成就解锁时间纪元修正 {} → {}", v, nv as i32);
                }
            }
        }
        let npcs: id = msg_send(env, (ui, s_npcs));
        if npcs != nil {
            let n: crate::mem::GuestUSize = msg_send(env, (npcs, s_cnt));
            for i in 0..n {
                let npc: id = msg_send(env, (npcs, s_oai, i));
                if npc == nil || !env.objc.object_has_method_named(&env.mem, npc, "lastCoolDownTime") {
                    continue;
                }
                let v: f64 = msg_send(env, (npc, s_lcd));
                if let Some(nv) = cf_fix_residue(v, now_cf) {
                    let _: () = msg_send(env, (npc, s_slcd, nv));
                    log!("[MOLECHEAT] island: NPC 冷却时间纪元修正 {} → {}", v, nv);
                }
            }
        }
    }
    log!("[MOLECHEAT] island: 读回 island_userinfo.dat(任务/剧情/成就/扩地进度恢复)");
    true
}

/// [P2b] 快照 TMMapData → mapData 的类型 key(只在"全表按 seqId 找不到、需要新增条目"时才用)。
/// ★订正(2026-09 审计,objc 元数据 superclass 实读):15 个 TMMapData* 类**全部直接继承 TMMapDataBase、互为兄弟**,
/// 并不存在"餐厅/公寓继承 TMMapDataShop"——判定顺序无所谓,"28" 也不能当父类兜底。表外的类(Building/装饰/
/// 黄鸭等)返回 None,由落盘时 merge_new_island_objects_into_mapdata 用活对象 [obj type] 定 key(=loadMapObjects: 读的键)。
fn island_class_to_key(env: &mut Environment, snap: id) -> Option<&'static str> {
    let isk = env
        .objc
        .register_host_selector("isKindOfClass:".to_string(), &mut env.mem);
    for (cls_name, key) in [
        ("TMMapDataRestaurant", "29"),
        ("TMMapDataApartment", "32"),
        ("TMMapDataCafeShop", "41"),
        ("TMMapDataShip", "39"),
        ("TMMapDataSuperShellTree", "40"),
        ("TMMapDataShop", "28"),
    ] {
        let cls = env.objc.get_known_class(cls_name, &mut env.mem);
        if cls != nil {
            let is: bool = msg_send(env, (snap, isk, cls));
            if is {
                return Some(key);
            }
        }
    }
    None
}

/// [2026-09-06 审计修] 在 mapData 的**全部** key 数组里按 objectSequenceId 找对象,返回(数组, 下标)。
/// 为什么要全表搜:mapData 的 key 是**对象 type**(loadMapObjects: 的 67-case 跳表键),而
/// island_class_to_key 只认得 6 个经营类;普通建筑/装饰/黄鸭等落在别的 key 上,按 key 定位必然落空。
/// 而 seqId 在整张 mapData 内唯一(NewSceneCommand.currentMaxSequenceId_ 全局自增),全表搜是安全的。
fn island_find_by_seqid(env: &mut Environment, md: id, seqid: i32) -> Option<(id, crate::mem::GuestUSize)> {
    if md == nil || seqid == 0 {
        return None;
    }
    let ak_s = env
        .objc
        .register_host_selector("allKeys".to_string(), &mut env.mem);
    let keys: id = msg_send(env, (md, ak_s));
    if keys == nil {
        return None;
    }
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let oai = env
        .objc
        .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
    let ofk = env
        .objc
        .register_host_selector("objectForKey:".to_string(), &mut env.mem);
    let seq_s = env
        .objc
        .register_host_selector("objectSequenceId".to_string(), &mut env.mem);
    let nk: crate::mem::GuestUSize = msg_send(env, (keys, cnt_s));
    for ki in 0..nk {
        let k: id = msg_send(env, (keys, oai, ki));
        if k == nil {
            continue;
        }
        let arr: id = msg_send(env, (md, ofk, k));
        if arr == nil {
            continue;
        }
        let n: crate::mem::GuestUSize = msg_send(env, (arr, cnt_s));
        for i in 0..n {
            let old: id = msg_send(env, (arr, oai, i));
            if old == nil {
                continue;
            }
            let oseq: i32 = msg_send(env, (old, seq_s));
            if oseq == seqid {
                return Some((arr, i));
            }
        }
    }
    None
}

/// [2026-09-06 审计修] 取 [NewSceneData sharedInstance].mapData(nil 安全)。
fn island_mapdata(env: &mut Environment) -> id {
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return nil;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return nil;
    }
    let md_s = env
        .objc
        .register_host_selector("mapData".to_string(), &mut env.mem);
    msg_send(env, (nsd, md_s))
}

/// [2026-09-16 黄金岛审查修] 取 [NewSceneData sharedInstance].userInfoDataInNewScene(nil 安全),
/// 与 island_mapdata 同构。岛上的等级/任务/剧情/工人/建设值都挂在这个对象上。
fn island_userinfo_data(env: &mut Environment) -> id {
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return nil;
    }
    let sh = island_sel(env, "sharedInstance");
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return nil;
    }
    let ui_s = island_sel(env, "userInfoDataInNewScene");
    msg_send(env, (nsd, ui_s))
}

/// [P2b 经营进度回写] 升级餐厅/雇用公寓/出海等改的是活建筑,游戏把快照喂 setModObjectToServer:
/// (离线被吞、从不写回 mapData)→ 退岛 archive 的只是进岛初始态、经营进度丢。这里把快照按
/// objectSequenceId 写回 [NewSceneData mapData][key] 数组(find→replace,无则 add),使 island_map.dat
/// 能存到最新经营态。全程 nil-guard;seqId==0(未分配)或非核心经营类则跳过(安全 no-op,不污染)。
fn writeback_island_object(env: &mut Environment, snap: id) {
    if snap == nil {
        return;
    }
    let seq_s = env
        .objc
        .register_host_selector("objectSequenceId".to_string(), &mut env.mem);
    let seqid: i32 = msg_send(env, (snap, seq_s));
    if seqid == 0 {
        return;
    }
    let md = island_mapdata(env);
    if md == nil {
        return;
    }
    // ★先全表按 seqId 找(2026-09-06 审计修):原来只在 island_class_to_key 给出的那一个 key 里找,
    //   而该表只认 6 个经营类 → 建设庄园买的普通建筑/装饰/黄鸭等的经营态改动全部静默丢弃
    //   (且原方法还被 return true 吞掉,连原版的缓冲都没进)。seqId 全局唯一,全表搜是精确的。
    if let Some((arr, idx)) = island_find_by_seqid(env, md, seqid) {
        let rep = env.objc.register_host_selector(
            "replaceObjectAtIndex:withObject:".to_string(),
            &mut env.mem,
        );
        let _: () = msg_send(env, (arr, rep, idx, snap));
        // [扫描修 2026-09-15] F10-6 岛上每次升级/雇用/出海都会走这里,逐次日志降为 log_dbg!。
        log_dbg!(
            "[MOLECHEAT] island: 经营态写回 mapData seqId={} (replace @{})",
            seqid,
            idx
        );
        return;
    }
    // 全表都没有 → 这是个还没进 mapData 的对象,需要新建条目,此时才需要知道该放进哪个 key。
    let key = match island_class_to_key(env, snap) {
        Some(k) => k,
        None => {
            // 类不在表内:留给退岛时的 merge_new_island_objects_into_mapdata 用活对象的
            // [obj type] 定 key(那是 loadMapObjects: 读 mapData 用的同一个键),这里安全跳过。
            return;
        }
    };
    // [扫描修 2026-09-15] F10-7 key 来自 island_class_to_key(&'static str)→ 用 get_static_str,不再泄漏 +1 串。
    let keystr = crate::frameworks::foundation::ns_string::get_static_str(env, key);
    let ofk = env
        .objc
        .register_host_selector("objectForKey:".to_string(), &mut env.mem);
    let mut arr: id = msg_send(env, (md, ofk, keystr));
    if arr == nil {
        arr = island_alloc_init(env, "NSMutableArray");
        if arr == nil {
            return;
        }
        let sfk = env
            .objc
            .register_host_selector("setObject:forKey:".to_string(), &mut env.mem);
        let _: () = msg_send(env, (md, sfk, arr, keystr));
    }
    let add = env
        .objc
        .register_host_selector("addObject:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (arr, add, snap));
    log_dbg!(
        "[MOLECHEAT] island: 经营态写回 mapData[key={}] seqId={} (add)",
        key,
        seqid
    );
}

/// [审计修 2026-09-11] 当前 CFAbsoluteTime 秒数(2001-01-01 纪元)。游戏的"服务器时间"就是这个纪元。
/// [扫描修 2026-09-15] F7-5 加上开发者「时间旅行」偏移 crate::libc::time::time_offset_secs()。
///   根因:W13 让 CFAbsoluteTimeGetCurrent/time()/gettimeofday/NSDate 等 guest 墙钟源统一加偏移;这里若不加,
///   黄金岛 getCurrentServerTime 钩子(岛上计时)、作物瞬熟算的 beginTime 目标、cf_fix_residue 的"未来"判据
///   都会和游戏读到的时钟差一个偏移(岛计时整体落后、残留修正误判)。偏移只增不减、在线模式由 mole_dev 拒绝设置,
///   所以无条件相加即可;单调时钟(Instant/mach_absolute_time)不受影响,本文件的节拍节流仍用 Instant。
fn now_cf_secs() -> f64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(978307200.0);
    unix - 978307200.0 + crate::libc::time::time_offset_secs() as f64
}

/// [审计修 2026-09-11] unix 纪元残留 → CFAbsoluteTime。2026-09-06~11 之间旧的时钟修复误把 unix 秒当服务器时间,
/// 可能在存档里留下"比真实时间快约 31 年"的时间戳:DiscoveryShip 负差值不会自愈,会一直卡到 2057 年。
/// 规则:v > now+半个纪元差 且 v < 4294967295(NewSceneQuest finish 的哨兵值)→ 减 978307200。0 与小值一律不动:
/// 0 有"未开始/冷却已结束"语义;旧小基准值的差值为正=到时即完成,属良性(取证结论)。
/// [复核修 2026-09-15] R7-2:判据从"比现在晚 1 天以上"收紧到"比现在晚半个纪元差(978307200/2 秒≈15.5 年)以上"。
///   根因:开发者「时间旅行」偏移只在进程内生效、不落盘;前进超过 1 天后岛上存下的时间戳,重启后都比现实晚 1 天以上,
///   旧判据会把它们当 unix 残留减掉 978307200(变成约 1995 年),并随下次落盘写进 island_*.dat,不可逆。
///   真正的残留比 CF 时间超前约 31 年,新判据仍能命中(残留写下后 15.5 年内都能识别,2026-09 的残留到 2042 年仍可修);
///   时间旅行累计前进不到 15.5 年不会误判。
fn cf_fix_residue(v: f64, now_cf: f64) -> Option<f64> {
    const RESIDUE_MIN_LEAD: f64 = 978307200.0 * 0.5;
    if v > now_cf + RESIDUE_MIN_LEAD && v < 4294967295.0 {
        Some(v - 978307200.0)
    } else {
        None
    }
}

/// [深扫修 2026-09-11] #11「作物瞬熟」:-[Farm innerupdate:](及 FlowerFarm/FruitFarm 继承)的前置钩子。
/// 反汇编 0x48590-0x4874e:
///   · 0x485aa 读 farmState_(ivar 槽 0xb033e0),≠4(非生长中)直接去 unschedule,不算时间;
///   · 0x485ce elapsed = CFAbsoluteTimeGetCurrent − beginTime(Object ivar,经 __nl_symbol_ptr 0x9c8064 → 槽 0xb03358);
///     elapsed<0 时把 beginTime 重置成 now;
///   · 0x48614-0x4863a 先比 elapsed ≥ matureTime+witherTime(f32,槽 0xb033d0/0xb033d4)→ 枯萎(unschedule + cropWitherHandler:0),
///     再比 elapsed ≥ matureTime → cropStage_(槽 0xb033d8)=4 + cropMatureHandler;否则按 elapsed/(mature*0.25) 算生长阶段。
/// 做法:生长中且未成熟时,把 beginTime 写成 now − matureTime − 0.5,让真方法这一拍算出的 elapsed 刚好越过成熟点、
/// 又远小于 matureTime+witherTime(要求 witherTime>2s;否则不动,免得把作物推进枯萎分支——开着「永不枯萎」时枯萎事件
/// 被吞、innerupdate: 已被 unschedule,地块会卡死不成熟)。偏移一律从 guest 的 _OBJC_IVAR 槽现读(兼容 touchHLE 非脆弱
/// ivar 修正写回),不写死。只写一个 double ivar,不发消息、不碰寄存器;beginTime 会随 map.dat 落盘,关掉开关后作物保持
/// 已成熟,之后按原版计时枯萎(与"瞬熟"语义一致)。
fn farm_instant_mature(env: &mut Environment) {
    let recv = env.cpu.regs()[0];
    // [2026-09-16] G-03 主体拆成单地块接口 farm_instant_mature_at(菜单「一键收获全部」共用),钩子行为不变。
    let _ = farm_instant_mature_at(env, recv);
}

/// [2026-09-16] G-03 Farm 相关 ivar 偏移 [beginTime, farmState_, cropStage_, matureTime, witherTime],从 guest 的 _OBJC_IVAR 槽现读
/// (兼容 touchHLE 非脆弱 ivar 修正写回)。原样从 farm_instant_mature 里抽出来给钩子和菜单共用;槽内容不符或偏移越界返回 None。
fn farm_ivar_offsets(env: &Environment) -> Option<[u32; 5]> {
    // __nl_symbol_ptr 0x9c8064 静态绑定到 _OBJC_IVAR_$_Object.beginTime(0xb03358);不符说明二进制不对,直接放弃。
    let begin_slot: u32 = env.mem.read(ConstPtr::<u32>::from_bits(0x9c8064));
    if begin_slot != 0xb03358 {
        return None;
    }
    let offs: [u32; 5] = [
        env.mem.read(ConstPtr::<u32>::from_bits(begin_slot)),
        env.mem.read(ConstPtr::<u32>::from_bits(0xb033e0)),
        env.mem.read(ConstPtr::<u32>::from_bits(0xb033d8)),
        env.mem.read(ConstPtr::<u32>::from_bits(0xb033d0)),
        env.mem.read(ConstPtr::<u32>::from_bits(0xb033d4)),
    ];
    // Farm instanceSize=404;偏移越界说明槽没按预期初始化,放弃(宁可不瞬熟也不乱写内存)。
    if offs.iter().any(|&o| o == 0 || o >= 0x1000) {
        return None;
    }
    Some(offs)
}

/// [2026-09-16] G-03 读地块的 (farmState_, cropStage_),菜单「一键收获全部」据此分类。只读内存、不发消息;偏移读不到返回 None。
pub(crate) fn farm_state_stage(env: &Environment, recv: u32) -> Option<(i32, i32)> {
    if recv == 0 {
        return None;
    }
    let [_, off_state, off_stage, _, _] = farm_ivar_offsets(env)?;
    let state: i32 = env.mem.read(ConstPtr::<i32>::from_bits(recv + off_state));
    let stage: i32 = env.mem.read(ConstPtr::<i32>::from_bits(recv + off_stage));
    Some((state, stage))
}

/// [2026-09-16] G-03 单地块「作物瞬熟」,算法与原 farm_instant_mature 逐行相同(说明见上)。
/// 返回 true = 这块地生长中、未成熟,且 beginTime 已在成熟点之前(本来就过了,或刚拨过去),下一次 innerupdate: 就会成熟;
/// 返回 false = 不是生长中、已成熟、偏移或时长异常,什么都没写。
pub(crate) fn farm_instant_mature_at(env: &mut Environment, recv: u32) -> bool {
    if recv == 0 {
        return false;
    }
    let Some([off_begin, off_state, off_stage, off_mature, off_wither]) = farm_ivar_offsets(env)
    else {
        return false;
    };
    let state: i32 = env.mem.read(ConstPtr::<i32>::from_bits(recv + off_state));
    if state != 4 {
        return false; // 非生长中:真方法自己 unschedule,不关我们的事
    }
    let stage: i32 = env.mem.read(ConstPtr::<i32>::from_bits(recv + off_stage));
    if stage == 4 {
        return false; // 已成熟
    }
    let mature: f32 = env.mem.read(ConstPtr::<f32>::from_bits(recv + off_mature));
    let wither: f32 = env.mem.read(ConstPtr::<f32>::from_bits(recv + off_wither));
    if !(mature > 0.0) || !(wither > 2.0) {
        return false;
    }
    let begin_ptr: MutPtr<f64> = Ptr::from_bits(recv + off_begin);
    let begin: f64 = env.mem.read(begin_ptr);
    // now_cf_secs 与 touchHLE 的 CFAbsoluteTimeGetCurrent 同源(SystemTime::now + 时间旅行偏移,见 F7-5),真方法紧接着取的 now 只会≥它。
    let target = now_cf_secs() - mature as f64 - 0.5;
    if begin <= target {
        return true; // 本来就已过成熟点,交给原版
    }
    env.mem.write(begin_ptr, target);
    true
}

fn island_sel(env: &mut Environment, name: &str) -> SEL {
    env.objc.register_host_selector(name.to_string(), &mut env.mem)
}

/// 岛上 mapData 里的全部 TMMapData 对象(key → 数组 → 对象),附精确类名。key 间顺序不保证,
/// 但同一 objectId 的对象总在同一个 key 数组里、相对顺序随归档保持。
fn island_all_objects(env: &mut Environment) -> Vec<(id, String)> {
    let mut out = Vec::new();
    let md = island_mapdata(env);
    if md == nil {
        return out;
    }
    let ak = island_sel(env, "allKeys");
    let cnt = island_sel(env, "count");
    let oai = island_sel(env, "objectAtIndex:");
    let ofk = island_sel(env, "objectForKey:");
    let keys: id = msg_send(env, (md, ak));
    if keys == nil {
        return out;
    }
    let nk: crate::mem::GuestUSize = msg_send(env, (keys, cnt));
    for ki in 0..nk {
        let k: id = msg_send(env, (keys, oai, ki));
        let arr: id = msg_send(env, (md, ofk, k));
        if arr == nil {
            continue;
        }
        let n: crate::mem::GuestUSize = msg_send(env, (arr, cnt));
        for i in 0..n {
            let obj: id = msg_send(env, (arr, oai, i));
            if obj == nil {
                continue;
            }
            let cls = crate::objc::ObjC::read_isa(obj, &env.mem);
            let name = env.objc.get_class_name(cls).to_string();
            out.push((obj, name));
        }
    }
    out
}

/// [审计修 2026-09-11] 岛布局里的绝对时间字段:(类名, [(getter, setter, 是否 double)])。类型与"是否绝对时间"均经
/// objc 元数据与存档路径 +[NewGameManager saveTMMapDataFromObject:] 逐字段取证;次数/时长字段(harvestTimes/
/// outputTimes/touchTimes/TransObject duration_)刻意不列入。
const ISLAND_TIME_FIELDS: &[(&str, &[(&str, &str, bool)])] = &[
    ("TMMapDataRestaurant", &[("beginUpgradeTime", "setBeginUpgradeTime:", false), ("lastCoolTime", "setLastCoolTime:", false)]),
    ("TMMapDataShop", &[("beginTime", "setBeginTime:", true)]),
    ("TMMapDataApartment", &[("lastMoleFinishTrainingTime", "setLastMoleFinishTrainingTime:", false)]),
    ("TMMapDataShip", &[("beginDiscoverTime", "setBeginDiscoverTime:", true), ("beginFixTime", "setBeginFixTime:", true)]),
    ("TMMapDataSuperShellTree", &[("purchaseTime", "setPurchaseTime:", false)]),
    ("TMMapDataBuilding", &[("beginTime", "setBeginTime:", true), ("coolingTime", "setCoolingTime:", true), ("gameCoolTime", "setGameCoolTime:", true)]),
    ("TMMapDataSpacials", &[("coolingTime", "setCoolingTime:", true)]),
    ("TMMapDataTransObject", &[("beginTime", "setBeginTime:", true)]),
    ("TMMapDataYellowDuck", &[("purchaseTime", "setPurchaseTime:", true), ("lastTransformTime", "setLastTransformTime:", true), ("coolingTime", "setCoolingTime:", true)]),
];

/// [审计修 2026-09-11] 读档后修正岛布局里的 unix 纪元残留时间戳(见 cf_fix_residue)。
fn migrate_island_timestamps(env: &mut Environment) {
    let now_cf = now_cf_secs();
    let mut fixed = 0;
    for (obj, cname) in island_all_objects(env) {
        let Some((_, fields)) = ISLAND_TIME_FIELDS.iter().find(|(c, _)| *c == cname) else {
            continue;
        };
        for &(getter, setter, is_double) in fields.iter() {
            if !env.objc.object_has_method_named(&env.mem, obj, getter) {
                continue;
            }
            let g = island_sel(env, getter);
            let st = island_sel(env, setter);
            if is_double {
                let v: f64 = msg_send(env, (obj, g));
                if let Some(nv) = cf_fix_residue(v, now_cf) {
                    let _: () = msg_send(env, (obj, st, nv));
                    log!("[MOLECHEAT] island: 时间戳纪元修正 {}.{} {} → {}", cname, getter, v, nv);
                    fixed += 1;
                }
            } else {
                let v: u32 = msg_send(env, (obj, g));
                if let Some(nv) = cf_fix_residue(v as f64, now_cf) {
                    let _: () = msg_send(env, (obj, st, nv as u32));
                    log!("[MOLECHEAT] island: 时间戳纪元修正 {}.{} {} → {}", cname, getter, v, nv as u32);
                    fixed += 1;
                }
            }
        }
    }
    if fixed > 0 {
        log!("[MOLECHEAT] island: 岛布局 unix 纪元残留已修正 {} 处", fixed);
    }
}

/// [审计修 2026-09-11] 船与咖啡馆的旁路存档 island_ships.dat。
/// 取证:-[TMMapDataShip encodeWithCoder:]@0xcd860 只编 6 个键,**shipState_ 与 showGiftsList_ 不在 NSCoding 里**
/// (原版靠服务器 1062 下发,parseMapDataWithPackageData: 0x22b3e8/0x22b420);TMMapDataCafeShop 连 NSCoding
/// 方法都没有,isNew_ 也丢。后果:每次重进岛船都退回"坏了"(shipState=1)要重修;出海归来没当场领的奖品清空;
/// 若退岛时正在出海,重进后本会话跳过"船在海上"分支、不调度 innerUpdate,船一直点不动。
/// 礼物元素是 DiscoverRewardData(无 NSCoding,只需 rewardObjId:原版上行包 encodeMapdata: 也只传这个),
/// 故只存 rewardObjId 的 int 列表,读回时用 DiscoverRewardData 重建(不能用 NSNumber:initShowGiftsListData:
/// 会对元素发 rewardObjId,NSNumber 当 no-op 返回 0,匹配不到奖励却照样挂领奖旗 = 空奖励)。
/// 定位用 (类别, objectId, 同 objectId 内序号),不用 seqId(seqId 不入档,读档时会重新分配)。
/// [扫描修 2026-09-15] F10-6 返回落盘摘要供 island_flush 汇总;F10-7 put 闭包的键与 "gifts" 改用 get_static_str
///   (以前每船 3-5 个 +1 串从不释放)。
fn save_island_ships(env: &mut Environment) -> Option<String> {
    let path = island_data_path(env, "island_ships.dat");
    if path == nil {
        return None;
    }
    // [深扫修 2026-09-11] #7 船档描述的是 island_map.dat 里那批船/咖啡馆:布局坏档仍在保护中(当前内存是默认岛)时,
    //   船档也不能用默认岛的船状态覆盖;自身坏档保护中同理。
    if (ISLAND_LOAD_FAILED.load(O) & ISLAND_FILE_MAP) != 0 {
        if (ISLAND_BLOCK_LOGGED.fetch_or(ISLAND_FILE_SHIPS, O) & ISLAND_FILE_SHIPS) == 0 {
            log!("[MOLECHEAT] island: 跳过落盘 island_ships.dat(island_map.dat 坏档保护中,当前是默认岛)");
        }
        return None;
    }
    if island_save_blocked(env, path, ISLAND_FILE_SHIPS, "island_ships.dat") {
        return None;
    }
    let out = island_alloc_init(env, "NSMutableArray");
    if out == nil {
        return None;
    }
    let num_cls = env.objc.get_known_class("NSNumber", &mut env.mem);
    let n_int = island_sel(env, "numberWithInt:");
    let sfk = island_sel(env, "setObject:forKey:");
    let add = island_sel(env, "addObject:");
    let cnt = island_sel(env, "count");
    let oai = island_sel(env, "objectAtIndex:");
    let oid_s = island_sel(env, "objectId");
    let mut seen: Vec<(i32, i32)> = Vec::new();
    let mut total = 0;
    for (obj, cname) in island_all_objects(env) {
        let kind = match cname.as_str() {
            "TMMapDataShip" => 1,
            "TMMapDataCafeShop" => 2,
            _ => continue,
        };
        let oid: i32 = msg_send(env, (obj, oid_s));
        let ord = seen.iter().filter(|&&(k, o)| k == kind && o == oid).count() as i32;
        seen.push((kind, oid));
        let entry = island_alloc_init(env, "NSMutableDictionary");
        if entry == nil {
            continue;
        }
        let put = |env: &mut Environment, key: &'static str, v: i32| {
            let num: id = msg_send(env, (num_cls, n_int, v));
            let k = crate::frameworks::foundation::ns_string::get_static_str(env, key); // [扫描修 2026-09-15] F10-7
            let _: () = msg_send(env, (entry, sfk, num, k));
        };
        put(env, "kind", kind);
        put(env, "objectId", oid);
        put(env, "ord", ord);
        if kind == 1 {
            let ss = island_sel(env, "shipState");
            let state: i32 = msg_send(env, (obj, ss));
            put(env, "shipState", state);
            let gs = island_sel(env, "showGiftsList");
            let gifts: id = msg_send(env, (obj, gs));
            let garr = island_alloc_init(env, "NSMutableArray");
            if garr != nil {
                if gifts != nil {
                    let gn: crate::mem::GuestUSize = msg_send(env, (gifts, cnt));
                    for j in 0..gn {
                        let g: id = msg_send(env, (gifts, oai, j));
                        if g == nil || !env.objc.object_has_method_named(&env.mem, g, "rewardObjId") {
                            continue;
                        }
                        let rs = island_sel(env, "rewardObjId");
                        let rid: i32 = msg_send(env, (g, rs));
                        let num: id = msg_send(env, (num_cls, n_int, rid));
                        let _: () = msg_send(env, (garr, add, num));
                    }
                }
                let k = crate::frameworks::foundation::ns_string::get_static_str(env, "gifts"); // [扫描修 2026-09-15] F10-7
                let _: () = msg_send(env, (entry, sfk, garr, k));
                release(env, garr);
            }
        } else {
            let ns = island_sel(env, "isNew");
            let is_new: u8 = msg_send(env, (obj, ns));
            put(env, "isNew", is_new as i32);
        }
        let _: () = msg_send(env, (out, add, entry));
        release(env, entry);
        total += 1;
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    let arch_s = island_sel(env, "archivedDataWithRootObject:");
    let data: id = msg_send(env, (arch_cls, arch_s, out));
    release(env, out);
    if data == nil {
        return None;
    }
    let write_s = island_sel(env, "writeToFile:atomically:");
    let ok: bool = msg_send(env, (data, write_s, path, true));
    if ok {
        log_dbg!("[MOLECHEAT] island: 存盘 island_ships.dat(船/咖啡馆 {} 个 ok={})", total, ok);
    } else {
        log!("[MOLECHEAT] island: 存盘 island_ships.dat(船/咖啡馆 {} 个 ok={})", total, ok);
    }
    Some(format!("存盘 island_ships.dat(船/咖啡馆 {} 个 ok={})", total, ok))
}

/// [审计修 2026-09-11] 读回 island_ships.dat,在建筑实例化(loadNewScene: → loadMapFromData:forNPC:)之前回填到
/// [NewSceneData mapData] 里的同一批 TMMapData 对象上(setMapData: 是浅 mutableCopy,对象同源,直接发 setter 即生效)。
fn load_island_ships(env: &mut Environment) {
    let path = island_data_path(env, "island_ships.dat");
    if path == nil {
        return;
    }
    let unarch_cls = env.objc.get_known_class("NSKeyedUnarchiver", &mut env.mem);
    let unarch_s = island_sel(env, "unarchiveObjectWithFile:");
    let arr: id = msg_send(env, (unarch_cls, unarch_s, path));
    if arr == nil {
        // [深扫修 2026-09-11] #7 区分无档/坏档(坏档隔离或禁止覆盖)。
        island_note_load_failure(env, path, ISLAND_FILE_SHIPS, "island_ships.dat");
        return;
    }
    island_note_load_ok(ISLAND_FILE_SHIPS);
    let cnt = island_sel(env, "count");
    let oai = island_sel(env, "objectAtIndex:");
    let ofk = island_sel(env, "objectForKey:");
    let iv = island_sel(env, "intValue");
    let oid_s = island_sel(env, "objectId");
    let objs = island_all_objects(env);
    let n: crate::mem::GuestUSize = msg_send(env, (arr, cnt));
    let mut restored = 0;
    for i in 0..n {
        let entry: id = msg_send(env, (arr, oai, i));
        if entry == nil {
            continue;
        }
        // [扫描修 2026-09-15] F10-7 键名固定 → get_static_str(以前每次进岛每船 3-4 个 +1 串从不释放)。
        let get = |env: &mut Environment, key: &'static str| -> Option<i32> {
            let k = crate::frameworks::foundation::ns_string::get_static_str(env, key);
            let num: id = msg_send(env, (entry, ofk, k));
            if num == nil {
                None
            } else {
                Some(msg_send(env, (num, iv)))
            }
        };
        let (Some(kind), Some(oid), Some(ord)) = (get(env, "kind"), get(env, "objectId"), get(env, "ord")) else {
            continue;
        };
        let want = if kind == 1 { "TMMapDataShip" } else { "TMMapDataCafeShop" };
        let mut hit: Option<id> = None;
        let mut seen = 0;
        for &(obj, ref cname) in objs.iter() {
            if cname != want {
                continue;
            }
            let o: i32 = msg_send(env, (obj, oid_s));
            if o != oid {
                continue;
            }
            if seen == ord {
                hit = Some(obj);
                break;
            }
            seen += 1;
        }
        let Some(obj) = hit else { continue };
        if kind == 1 {
            if let Some(state) = get(env, "shipState") {
                if state == 1 || state == 2 {
                    let s2 = island_sel(env, "setShipState:");
                    let _: () = msg_send(env, (obj, s2, state));
                }
            }
            let gk = crate::frameworks::foundation::ns_string::get_static_str(env, "gifts"); // [扫描修 2026-09-15] F10-7
            let gifts: id = msg_send(env, (entry, ofk, gk));
            let gn: crate::mem::GuestUSize = if gifts != nil { msg_send(env, (gifts, cnt)) } else { 0 };
            if gn > 0 {
                let rd_cls = env.objc.get_known_class("DiscoverRewardData", &mut env.mem);
                let garr = island_alloc_init(env, "NSMutableArray");
                if rd_cls != nil && garr != nil {
                    let alloc_s = island_sel(env, "alloc");
                    let init_s = island_sel(env, "init");
                    let set_rid = island_sel(env, "setRewardObjId:");
                    let add = island_sel(env, "addObject:");
                    for j in 0..gn {
                        let num: id = msg_send(env, (gifts, oai, j));
                        let rid: i32 = msg_send(env, (num, iv));
                        let a: id = msg_send(env, (rd_cls, alloc_s));
                        let rd: id = msg_send(env, (a, init_s));
                        let _: () = msg_send(env, (rd, set_rid, rid));
                        let _: () = msg_send(env, (garr, add, rd));
                        release(env, rd);
                    }
                    let sg = island_sel(env, "setShowGiftsList:");
                    let _: () = msg_send(env, (obj, sg, garr));
                    release(env, garr);
                }
            }
        } else if let Some(is_new) = get(env, "isNew") {
            if is_new != 0 {
                let sn = island_sel(env, "setIsNew:");
                let _: () = msg_send(env, (obj, sn, true));
            }
        }
        restored += 1;
    }
    log!("[MOLECHEAT] island: 读回 island_ships.dat(船/咖啡馆状态恢复 {} 个)", restored);
}

/// [审计修 2026-09-11] 兜底唯一会永久卡死的船状态组合:isSailing=1 且 beginDiscoverTime≤0 且
/// (searchMapId<1 或 onBoardMoleNum≤0)。此时 checkIsDiscoverFinished 恒 NO、也没有 innerUpdate 去清 isSailing,
/// processTouched 在 isSailing≠0 时直接返回 → 每次读档船都点不动(取证 0x361f6a-0x361f96 / 0x361620)。
fn fix_stuck_ships(env: &mut Environment) {
    for (obj, cname) in island_all_objects(env) {
        if cname != "TMMapDataShip" {
            continue;
        }
        let s1 = island_sel(env, "isSailing");
        let s2 = island_sel(env, "beginDiscoverTime");
        let s3 = island_sel(env, "searchMapId");
        let s4 = island_sel(env, "onBoardMoleNum");
        let sailing: u8 = msg_send(env, (obj, s1));
        let begin: f64 = msg_send(env, (obj, s2));
        let smid: i32 = msg_send(env, (obj, s3));
        let onb: i32 = msg_send(env, (obj, s4));
        if sailing != 0 && begin <= 0.0 && (smid < 1 || onb <= 0) {
            let set = island_sel(env, "setIsSailing:");
            let _: () = msg_send(env, (obj, set, false));
            log!(
                "[MOLECHEAT] island: 船状态卡死兜底(isSailing=1 begin={} searchMapId={} onBoard={})→ isSailing=0",
                begin,
                smid,
                onb
            );
        }
    }
}

/// [2026-09-16 黄金岛审查修 I5-01] 补发岛农场任务 4 的完成动作 action 13。
///
/// **病根**:farmquestHV.dat 的 ID=4(「雇一只摩尔」)既没有 req_*、也没有 cli_step,于是
/// `-[QuestData initWithDict:needLevel:timeQuest:]`@0x1126e4 给它的 questType_=0、requireThings 为空。
/// `-[NewSceneQuest checkAction:object:]`@0x32a758 在 0x32a822 用
/// `cmp r4,#0xd / it eq / cmpeq.w r10,#4 / beq finish` 把 **action 13 + curQuestId 4** 写成这条任务的
/// **唯一**完成判据(后面 0x32a8ea 起的泛化段没有任何一项配 action 13,questType 0 更是全不命中)。
/// 而 action 13 全二进制只有两个发出点,都要求 `currentProduceMoleNums >= 1`:
/// `-[ApartmentView innerupdate:]`@0x3256c8(0x3257d8 `cmp r0,#1`)与 `-[ApartmentView onChooseUse]`@0x325344
/// (0x32537e `cmp r0,#1`)。我们的「公寓雇用即时出摩尔」钩子把在产数压回旧值(恒 0),这两条路径永久不可达
/// → **91 条岛任务链在第 4 条硬停**,后面 87 条任务、story 4 起的全部剧情、任务 81/83 奖励的沙原碎片
/// 31005/31007 全部拿不到。原版 accept 里那条旁路(0x329378,curQuestId∈{3,4} 且工人数 ≥ 80)新号不可达。
///
/// **做法**:不动「即时出摩尔」这个已拍板的设计等价(见 2026-06 记录),只把它吃掉的那个信号补回来。
/// 每拍廉价自检「在岛 + curQuestId==4 + 工人数 ≥1」,满足就对 `+[NewSceneQuest sharedInstance]` 发一次
/// `checkAction:13 object:0`(签名 `v16@0:4i8i12`,**两个参数都是 int**,与游戏自己的 0x325b7c `movs r2,#0xd`
/// / 0x325b82 `movs r3,#0` 完全一致),然后置一次性标志。这样「先雇摩尔后接任务 4」的顺序也能覆盖
/// (原版只在产出完成那一刻发,顺序反了就永远卡死)。
/// checkAction:object: 是纯本地状态机(gameMode∈{0,6} 早退、questState!=1 早退),不发包;
/// action 13 只与 curQuestId==4 配对,别的任务不受影响。
fn island_resend_quest4_action(env: &mut Environment) {
    if ISLAND_QUEST4_SENT.load(O) || !ON_ISLAND.load(O) {
        return;
    }
    let ui = island_userinfo_data(env);
    if ui == nil {
        return;
    }
    let cq_s = island_sel(env, "curQuestId");
    let cur_quest: i32 = msg_send(env, (ui, cq_s));
    if cur_quest != 4 {
        return; // 不是这条任务,不做任何事(也不置位:玩家可能还没走到)
    }
    let tw_s = island_sel(env, "curTotalWorkersCount");
    let workers: i32 = msg_send(env, (ui, tw_s));
    if workers < 1 {
        return; // 还没雇到摩尔,原版此时也不该完成
    }
    let q_cls = env.objc.get_known_class("NewSceneQuest", &mut env.mem);
    if q_cls == nil {
        return;
    }
    let sh = island_sel(env, "sharedInstance");
    // +[NewSceneQuest sharedInstance]@0x327e08 在 curSceneId==1 时返回 nil;岛上 curSceneId 已被现有钩子
    // 强制成 10,但仍旧 nil 守卫。
    let quest: id = msg_send(env, (q_cls, sh));
    if quest == nil {
        return;
    }
    let ca_s = island_sel(env, "checkAction:object:");
    let _: () = msg_send(env, (quest, ca_s, 13i32, 0i32));
    ISLAND_QUEST4_SENT.store(true, O);
    island_mark_dirty();
    log!("[MOLECHEAT] island: 岛任务 4(雇一只摩尔)完成信号补发 checkAction:13 —— 即时雇用钩子吃掉了原版的发出点,任务链不再卡在第 4 条");
}

/// [2026-09-06 审计修] 岛存档统一落盘。原来这四件套只挂在 `gobackMainVillage` 一个点上,
/// 而岛上 HUD 的「返回」/「串门」按钮(`-[NewSceneVillageMenuLayer onButtonReturnSelected:]`@0x25a97c 等)
/// **直接调 startNewSceneFrom:10→1**、根本不经过 gobackMainVillage → 走那条路离岛,本次上岛盖的建筑、
/// 升的餐厅、雇的摩尔、出海收获全部静默蒸发。现在把它挂到全局出口上,覆盖所有离岛路径。
/// 幂等:save_island_map 自带 count==0 不写盘的护栏,重复调用安全。
/// [扫描修 2026-09-15] F10-6 以前每次落盘打 5 行(节拍 1 行 + 四个 save_* 各 1 行),建岛期间 1.5s 一组持续刷屏。
///   现在四个 save_* 只返回摘要,这里汇总成【一行】log!:`island: <reason> → 存盘 island_userinfo.dat(..) / 存盘 island_map.dat(..) / …`。
///   每个实际写入的文件名仍以「存盘 island_xxx.dat」原样出现(无头测试依赖该关键字);没写的文件不出现(与以前一致)。
fn island_flush(env: &mut Environment, reason: &str) {
    // 先清脏标记:落盘过程中若又有新变化(理论上 merge/归档本身不会触发),会重新置脏、下个节拍再存。
    ISLAND_FLUSHING.store(true, O);
    ISLAND_DIRTY.store(false, O);
    // 先让游戏自己把主存档(经济/等级)落盘,再存我们的岛档。注:saveUserinfoToLocal@0x21dcac 归档的是【主村】
    // UserInfoData(GameData.userInfoData_),与岛 NewSceneUserInfoData 无关,岛进度由下面 save_island_userinfo 负责。
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls != nil {
        let sh = env
            .objc
            .register_host_selector("sharedInstance".to_string(), &mut env.mem);
        let nsd: id = msg_send(env, (nsd_cls, sh));
        if nsd != nil {
            let save_ui = env
                .objc
                .register_host_selector("saveUserinfoToLocal".to_string(), &mut env.mem);
            let _: () = msg_send(env, (nsd, save_ui));
        }
    }
    let s_ui = save_island_userinfo(env);
    // ★必须在真 startNewSceneFrom→unloadMap 清空 ObjectManager 活表【之前】,此刻活表满载岛对象。
    let merged = merge_new_island_objects_into_mapdata(env);
    let s_map = save_island_map(env);
    let s_ships = save_island_ships(env);
    let s_frag = save_island_fragments(env);
    ISLAND_LAST_FLUSH.with(|c| c.set(Some(Instant::now())));
    ISLAND_FLUSHING.store(false, O);
    // [扫描修 2026-09-15] F10-6 汇总成一行(见函数注释)。
    let parts: Vec<String> = [s_ui, s_map, s_ships, s_frag].into_iter().flatten().collect();
    let body = if parts.is_empty() {
        "本次没有需要写入的岛档".to_string()
    } else {
        parts.join(" / ")
    };
    if merged > 0 {
        log!("[MOLECHEAT] island: {} → {}(合并新放置建筑 {} 个)", reason, body, merged);
    } else {
        log!("[MOLECHEAT] island: {} → {}", reason, body);
    }
}

/// [审计修] 标记岛存档需要落盘(纯原子操作,任何 hook 里都能安全调用,不碰寄存器)。
fn island_mark_dirty() {
    if ON_ISLAND.load(O) && !ISLAND_FLUSHING.load(O) {
        ISLAND_DIRTY.store(true, O);
    }
}

/// [审计修] 启动岛存档节拍:与调试悬浮窗 moleHudTick 同一模式——performSelector:withObject:afterDelay: 排到
/// 运行循环的 perform 相位执行,**完全不在 drawScene 帧栈里**(本仓血泪:帧栈里做 msg_send 会饿死运行循环、
/// 甚至触发 cocos2d 调度器重入活锁)。GameManager 不实现 moleIslandTick,由 intercept 接住。
fn start_island_tick(env: &mut Environment) {
    if ISLAND_TICK_RUNNING.swap(true, O) {
        // ★[审查修 2026-09-11] 闩锁自愈:节拍链可能已断而闩锁仍是 true(例如在岛上关掉"可建筑黄金岛·热点开关",
        //   排队的那一拍落到开关块外被当 no-op 丢弃)。距上一拍超过 3 秒就判定链已死、重新排程;链还活着就不重复开链。
        let stale = ISLAND_LAST_TICK
            .with(|c| c.get())
            .map_or(true, |t| t.elapsed().as_secs() >= 3);
        if !stale {
            return;
        }
        log!("[MOLECHEAT] island: 岛存档节拍链已断(>3s 未触发)→ 重新排程");
    } else {
        log!("[MOLECHEAT] island: 岛存档节拍已启动(每秒检查脏标记,节流 1.5s 落盘)");
    }
    ISLAND_LAST_TICK.with(|c| c.set(Some(Instant::now())));
    schedule_island_tick(env);
}

fn schedule_island_tick(env: &mut Environment) {
    let gm_cls = env.objc.get_known_class("GameManager", &mut env.mem);
    let smgr = env
        .objc
        .register_host_selector("sharedManager".to_string(), &mut env.mem);
    let gm: id = msg_send(env, (gm_cls, smgr));
    if gm == nil {
        ISLAND_TICK_RUNNING.store(false, O);
        return;
    }
    let tick = env
        .objc
        .register_host_selector("moleIslandTick".to_string(), &mut env.mem);
    let perform = env.objc.register_host_selector(
        "performSelector:withObject:afterDelay:".to_string(),
        &mut env.mem,
    );
    let _: () = msg_send(env, (gm, perform, tick, nil, 1.0f64));
}

/// [2026-09-06 审计修] 删除接管:原版 `-[NetworkManager deleteObjectFromServer:]` 发 1061 告诉服务器
/// "这个对象没了"。离线包被吞、mapData 从不删条目 → **拆掉/一键收纳掉的建筑下次进岛全部原地复活**,
/// 反复收纳还能凭空刷道具(仓库给了、地上还在)。这里按 seqId 从 mapData 全表移除,再吞掉原方法。
/// 调用者含 NewScenePorter removeEditObject / WrapperManager storeOnekey:(一键收纳,批量)/
/// SuperShellTree onChooseDelete,补在这一臂能一并覆盖。
fn delete_island_object(env: &mut Environment, snap: id) {
    if snap == nil {
        return;
    }
    let seq_s = env
        .objc
        .register_host_selector("objectSequenceId".to_string(), &mut env.mem);
    let seqid: i32 = msg_send(env, (snap, seq_s));
    if seqid == 0 {
        return;
    }
    let md = island_mapdata(env);
    if md == nil {
        return;
    }
    if let Some((arr, idx)) = island_find_by_seqid(env, md, seqid) {
        let rm = env
            .objc
            .register_host_selector("removeObjectAtIndex:".to_string(), &mut env.mem);
        let _: () = msg_send(env, (arr, rm, idx));
        // [扫描修 2026-09-15] F10-6 一键收纳 storeOnekey: 会批量逐个走到这里,逐次日志降为 log_dbg!。
        log_dbg!(
            "[MOLECHEAT] island: 删除写回 mapData seqId={} (removed @{})",
            seqid,
            idx
        );
    } else {
        // 会话内新放置、还没落过盘就被拆掉的对象本就不在 mapData 里,属正常;其余情况值得排查。
        log_dbg!(
            "[MOLECHEAT] island: 删除写回 seqId={} 在 mapData 中未找到(若非本局新放置的对象,请排查)",
            seqid
        );
    }
}

/// [扫描修 2026-09-15] F10-6 返回本次合并的新放置对象个数(由 island_flush 汇总进一行日志);F10-7 动态键串用完即释放。
fn merge_new_island_objects_into_mapdata(env: &mut Environment) -> i32 {
    let om_cls = env.objc.get_known_class("ObjectManager", &mut env.mem);
    if om_cls == nil {
        return 0;
    }
    let sm = env
        .objc
        .register_host_selector("sharedManager".to_string(), &mut env.mem);
    let om: id = msg_send(env, (om_cls, sm));
    if om == nil {
        return 0;
    }
    let objs_s = env
        .objc
        .register_host_selector("objects".to_string(), &mut env.mem);
    let objs: id = msg_send(env, (om, objs_s));
    if objs == nil {
        return 0;
    }
    let av_s = env
        .objc
        .register_host_selector("allValues".to_string(), &mut env.mem);
    let all: id = msg_send(env, (objs, av_s));
    if all == nil {
        return 0;
    }
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let n: crate::mem::GuestUSize = msg_send(env, (all, cnt_s));
    if n == 0 {
        return 0;
    }
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return 0;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return 0;
    }
    let md_s = env
        .objc
        .register_host_selector("mapData".to_string(), &mut env.mem);
    let md: id = msg_send(env, (nsd, md_s));
    if md == nil {
        return 0;
    }
    let ngm_cls = env.objc.get_known_class("NewGameManager", &mut env.mem);
    if ngm_cls == nil {
        return 0;
    }
    let save_snap = env
        .objc
        .register_host_selector("saveTMMapDataFromObject:".to_string(), &mut env.mem);
    let type_s = env
        .objc
        .register_host_selector("type".to_string(), &mut env.mem);
    let seq_s = env
        .objc
        .register_host_selector("objectSequenceId".to_string(), &mut env.mem);
    let oai = env
        .objc
        .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
    let ofk = env
        .objc
        .register_host_selector("objectForKey:".to_string(), &mut env.mem);
    let sfk = env
        .objc
        .register_host_selector("setObject:forKey:".to_string(), &mut env.mem);
    let add_s = env
        .objc
        .register_host_selector("addObject:".to_string(), &mut env.mem);
    let mut merged = 0i32;
    for i in 0..n {
        let obj: id = msg_send(env, (all, oai, i));
        if obj == nil {
            continue;
        }
        // 活对象 → TMMapData 快照(原版编码器,按 class/type 各写各字段;Firework 返 nil)。
        let snap: id = msg_send(env, (ngm_cls, save_snap, obj));
        if snap == nil {
            continue;
        }
        let seqid: i32 = msg_send(env, (snap, seq_s));
        if seqid == 0 {
            continue; // 未分配 seqId,无法去重/持久化
        }
        // key:精确6类(island_class_to_key)优先,否则活对象 type 字符串(=mapData key)。
        let key: String = match island_class_to_key(env, snap) {
            Some(k) => k.to_string(),
            None => {
                let t: i32 = msg_send(env, (obj, type_s));
                if t <= 0 {
                    continue;
                }
                t.to_string()
            }
        };
        let keystr = crate::frameworks::foundation::ns_string::from_rust_string(env, key);
        let mut arr: id = msg_send(env, (md, ofk, keystr));
        if arr == nil {
            arr = island_alloc_init(env, "NSMutableArray");
            if arr == nil {
                release(env, keystr); // [扫描修 2026-09-15] F10-7 提前 continue 也要平衡 +1
                continue;
            }
            let _: () = msg_send(env, (md, sfk, arr, keystr));
        }
        // [扫描修 2026-09-15] F10-7 键串本轮已用完(objectForKey: 只读;setObject:forKey: 会 copy 键)→ 释放 from_rust_string 的 +1。
        release(env, keystr);
        // 去重:该 seqId 已在数组(种子/经营回写已存)→ 跳过,绝不重复加。
        let an: crate::mem::GuestUSize = msg_send(env, (arr, cnt_s));
        let mut dup = false;
        for j in 0..an {
            let old: id = msg_send(env, (arr, oai, j));
            let oseq: i32 = msg_send(env, (old, seq_s));
            if oseq == seqid {
                dup = true;
                break;
            }
        }
        if dup {
            continue;
        }
        let _: () = msg_send(env, (arr, add_s, snap));
        merged += 1;
    }
    if merged > 0 {
        // [扫描修 2026-09-15] F10-6 节拍每次落盘都会走到;个数已并入 island_flush 的汇总行,这里降为 log_dbg!。
        log_dbg!(
            "[MOLECHEAT] island: 退岛合并 {} 个新放置建筑进 mapData(持久化)",
            merged
        );
    }
    merged
}

/// [岛持久化·读档补发 seqId] `TMMapDataBase encodeWithCoder:` 只存 objectId/baseTile/isFlip(IDA 0xcc5d4),
/// **objectSequenceId 不在 NSCoding 键里** → 从 island_map.dat 读回来的每个对象 seqId 都是 0。
/// 而经营回写(writeback_island_object)与退岛合并(merge_new_island_objects_into_mapdata)都以
/// seqId 为键、且 `seqId==0 → 跳过`,于是**第二次进岛起,餐厅升级/公寓雇佣/出海状态全部不再落盘**
/// (2026-09-06 无头实测:雇佣 +1 后退岛,存档里 moleNumInWaitingQueue_ 仍为 0)。
/// 首进用默认岛时种子给的是 90001-90008,这里对读档对象做同样的事:把所有 seqId==0 的对象
/// 从 max(90000, 已有最大) 起顺序补发。seqId 本就是会话内主键(原版由服务器 1062 下发、不入本地档),
/// 读档时重发与原版语义一致;之后 restore_seqid_cursor 会把游标抬到新最大值防新放置撞号。
fn assign_island_seqids(env: &mut Environment) {
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return;
    }
    let md_s = env
        .objc
        .register_host_selector("mapData".to_string(), &mut env.mem);
    let md: id = msg_send(env, (nsd, md_s));
    if md == nil {
        return;
    }
    let av_s = env
        .objc
        .register_host_selector("allValues".to_string(), &mut env.mem);
    let vals: id = msg_send(env, (md, av_s));
    if vals == nil {
        return;
    }
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let oai = env
        .objc
        .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
    let seq_s = env
        .objc
        .register_host_selector("objectSequenceId".to_string(), &mut env.mem);
    let n: crate::mem::GuestUSize = msg_send(env, (vals, cnt_s));
    // 第一遍:已有最大 seqId(读档一般全 0;混合情况也不撞)
    let mut max_seq: i32 = 90000;
    let mut zero_objs: Vec<id> = Vec::new();
    for i in 0..n {
        let arr: id = msg_send(env, (vals, oai, i));
        if arr == nil {
            continue;
        }
        let an: crate::mem::GuestUSize = msg_send(env, (arr, cnt_s));
        for j in 0..an {
            let obj: id = msg_send(env, (arr, oai, j));
            if obj == nil {
                continue;
            }
            let seq: i32 = msg_send(env, (obj, seq_s));
            if seq > max_seq {
                max_seq = seq;
            } else if seq == 0 {
                zero_objs.push(obj);
            }
        }
    }
    if zero_objs.is_empty() {
        return;
    }
    let total = zero_objs.len();
    for obj in zero_objs {
        max_seq += 1;
        obj_set_int(env, obj, "setObjectSequenceId:", max_seq);
    }
    log!(
        "[MOLECHEAT] island: 读档对象补发 seqId ×{}(→{}),经营回写/退岛合并恢复有效",
        total,
        max_seq
    );
}

/// [P3-a 跨会话 seqId 防撞] 进岛(load 或默认注入)后,把 NewSceneCommand.currentMaxSequenceId_ 抬到
/// 当前 mapData 里所有对象 objectSequenceId 的最大值——否则新建筑 seqId 来自 getCurrentSequenceId
/// (currentMaxSequenceId_+1,跨会话重启归 0)→ 第二次进岛新放置 seqId 从 1 自增,会与上次持久的
/// 低号(或种子 90001+)无关但与【上一会话的新放置】撞号 → merge/writeback 去重误判覆盖。抬高游标后
/// 新建筑 seqId 永远 > 已存最大 = 全局单调唯一。NewSceneCommand 实例=[[NetworkManager sharedInstance]
/// commandController](getter 实证);currentMaxSequenceId_ ivar 偏移=32(实读 _OBJC_IVAR);★只抬高不调小
/// (max>cur 才写)=零副作用,全程 nil-guard。
fn restore_seqid_cursor(env: &mut Environment) {
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return;
    }
    let sh = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, sh));
    if nsd == nil {
        return;
    }
    let md_s = env
        .objc
        .register_host_selector("mapData".to_string(), &mut env.mem);
    let md: id = msg_send(env, (nsd, md_s));
    if md == nil {
        return;
    }
    let av_s = env
        .objc
        .register_host_selector("allValues".to_string(), &mut env.mem);
    let vals: id = msg_send(env, (md, av_s));
    if vals == nil {
        return;
    }
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let oai = env
        .objc
        .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
    let seq_s = env
        .objc
        .register_host_selector("objectSequenceId".to_string(), &mut env.mem);
    let n: crate::mem::GuestUSize = msg_send(env, (vals, cnt_s));
    let mut max_seq: u32 = 0;
    for i in 0..n {
        let arr: id = msg_send(env, (vals, oai, i));
        if arr == nil {
            continue;
        }
        let an: crate::mem::GuestUSize = msg_send(env, (arr, cnt_s));
        for j in 0..an {
            let obj: id = msg_send(env, (arr, oai, j));
            if obj == nil {
                continue;
            }
            let seq: i32 = msg_send(env, (obj, seq_s));
            if seq > 0 && (seq as u32) > max_seq {
                max_seq = seq as u32;
            }
        }
    }
    if max_seq == 0 {
        return;
    }
    let nm_cls = env.objc.get_known_class("NetworkManager", &mut env.mem);
    if nm_cls == nil {
        return;
    }
    let nm: id = msg_send(env, (nm_cls, sh));
    if nm == nil {
        return;
    }
    let cc_s = env
        .objc
        .register_host_selector("commandController".to_string(), &mut env.mem);
    let cc: id = msg_send(env, (nm, cc_s));
    if cc == nil {
        return;
    }
    // 直写 currentMaxSequenceId_(ivar 偏移 32,u32);只在比当前大时抬高(单调,绝不调小)。
    let slot: crate::mem::MutPtr<u32> = crate::mem::Ptr::from_bits(cc.to_bits() + 32);
    let cur: u32 = env.mem.read(slot);
    if max_seq > cur {
        env.mem.write(slot, max_seq);
        log!(
            "[MOLECHEAT] island: seqId 游标恢复 currentMaxSequenceId_={}(防跨会话新放置撞号)",
            max_seq
        );
    }
}

fn build_default_island_mapdata(env: &mut Environment) -> bool {
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return false;
    }
    let shared_s = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, shared_s));
    if nsd == nil {
        return false;
    }
    // [P5 地基] 先确保岛 userInfo 载体存在(NPC/任务/剧情/成就),持久化与默认两条路径都要。
    ensure_island_userinfo(env, nsd);
    start_island_tick(env);
    // [P5 内容持久化] 读回岛专属进度(任务/剧情/成就/扩地/建设值/NPC),覆盖到载体上。
    // [审查修 2026-09-13] D2 删掉读档前算的 userinfo_on_disk。根因:load 遇到坏档会先改名隔离,隔离成功后内存里已是
    //   init 默认进度,按"读档前磁盘上有档"判成老玩家就不补 newGame;首个节拍把默认进度写成新 island_userinfo.dat 后,
    //   以后每次进岛 had_userinfo 恒真,开场剧情永久不播。newGame 判据改为读档之后看 ISLAND_FILE_USERINFO 保护位(见下)。
    let had_userinfo = load_island_userinfo(env);
    // [P3 商店空白治本] 建设庄园(NewStyleStoreMainLayer)读 NewSceneData.storeBuildingsArray_/
    //   storeDecorationsArray_、食材商店(ShopItemsLayer)读 5 个食材桶——这些桶 init 时全空,【只由
    //   LoadingHoliday case11 的 loadFileWithType:1 andSceneId:10 解 propertyHV.dat 本地填】(★岛的
    //   NewSceneData store 数组主村启动期根本没碰,只岛 case11 填)。离线状态机活锁可能到不了 case11 →
    //   桶空 → 商店空白。这里直接 host 侧补一发(幂等:objectsData_ 非空即跳过),绕过状态机时序强制
    //   本地填满 469 件建筑/装饰 + 食材桶。布局持久化与默认两条路径都要(catalog 与布局无关)。
    {
        let lf = env
            .objc
            .register_host_selector("loadFileWithType:andSceneId:".to_string(), &mut env.mem);
        let _: () = msg_send(env, (nsd, lf, 1i32, 10i32));
    }
    // ★[P3 商店空白真因·治本(2026-06-22 runtime 实测 storeBuildings[0]=0、curSceneId=10 坐实)]:
    //   loadFileWithType:andSceneId: 只在 objectsData_.count==0 时才加载 propertyHV 填 catalog(store
    //   数组)。但 resetNewSceneDataExceptObjectData(退岛/重置)清空 storeBuildingsArray/storeDecorations
    //   /食材桶却【保留 objectsData_】→ 再进岛时 loadFileWithType 的 guard 见 objectsData_ 非空即跳过
    //   propertyHV → store 数组恒空 → 建设庄园/食材店物品网格全空(curSceneId=10 没错、外层6桶都在,纯
    //   内层空,买不了)。修:查 storeBuildingsArray[0],若空则强制 loadPropertyWithType:andSceneId:
    //   (0x21e11c,无 guard,重跑 parseObjectData 重填空的 store 数组)。此时其余桶也被 reset 一并清空,
    //   重填一次不 dup(store 空 ⟺ 其余桶空,因 reset 一起清)。
    {
        let sba_s = env
            .objc
            .register_host_selector("storeBuildingsArray".to_string(), &mut env.mem);
        let sba: id = msg_send(env, (nsd, sba_s));
        let cnt_s = env
            .objc
            .register_host_selector("count".to_string(), &mut env.mem);
        let oai_s = env
            .objc
            .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
        let outer: u32 = if sba != nil {
            msg_send(env, (sba, cnt_s))
        } else {
            0
        };
        let inner0: u32 = if sba != nil && outer > 0 {
            let b: id = msg_send(env, (sba, oai_s, 0u32));
            if b != nil {
                msg_send(env, (b, cnt_s))
            } else {
                0
            }
        } else {
            0
        };
        if inner0 == 0 {
            let lp = env
                .objc
                .register_host_selector("loadPropertyWithType:andSceneId:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (nsd, lp, 1i32, 10i32));
            log!("[MOLECHEAT] island: store 数组空(reset 清+objectsData_ guard 跳过)→ 强制 loadPropertyWithType 重填 catalog");
        }
    }
    // ★[P3 gameMode seed·补全原版 LoadingHoliday case4@0x252f38(workflow A 路实证)]:进岛后
    //   NewGameManager.gameMode 的"正常浏览态=1"靠原版 case4 `[NewGameManager setGameMode:
    //   [GameManager gameMode]]`(主村 GameManager.gameMode 在 startGame: 里=1)拷过来 seed;离线进岛
    //   常没完整跑到 case4(case2/3 是硬网络门)→ gameMode 残留 init 的 -1 → 所有 gameMode==1 严判失效:
    //   ①布兰的家 RestaurantView(0x249769)/②公寓 ApartmentView(0x3263fc)面板入口【直读
    //   NewGameManager.gameMode==1】(curSceneId 路由对它们无效!)③食材店 ShopItemsLayer(0x24be80)
    //   读 currentGameMode==1(curSceneId=10 修复后已正确路由到 NewGameManager.gameMode)。这里在进岛
    //   数据就绪点等价补一发:读主村 GameManager.gameMode 透传(异常≤0 兜底 1=岛浏览态),一次性、
    //   非每帧(gameMode 有合法瞬态 9 临时/11 编辑放置/0 串门,绝不每帧钉死 1)。与现有 3 个 LR 门 hook
    //   叠加无害;runtime 验证 gameMode=1 已落实后,那 3 个零散 LR hook 可化简删除(A 路结论)。
    {
        let sm_sel = env
            .objc
            .register_host_selector("sharedManager".to_string(), &mut env.mem);
        let ngm_cls = env.objc.get_known_class("NewGameManager", &mut env.mem);
        let gm_cls = env.objc.get_known_class("GameManager", &mut env.mem);
        let ngm: id = msg_send(env, (ngm_cls, sm_sel));
        let gm: id = msg_send(env, (gm_cls, sm_sel));
        if ngm != nil && gm != nil {
            let gm_get = env
                .objc
                .register_host_selector("gameMode".to_string(), &mut env.mem);
            let gmode: i32 = msg_send(env, (gm, gm_get));
            // ★[审计修 2026-09-11] 一律 seed 1(岛浏览态)。原来 `gmode>0 → 原样透传` 会把主村的合法瞬态
            //   6(-[VillageMenuLayer updateUI]@0x608ea 置)/9(临时)/11(编辑放置)拷进岛:整个岛会话里任务判定
            //   (checkAction:object: 的 0/6 门)与布兰的家/公寓/食材店(==1 门)全部静默 bail,重进岛才恢复。
            //   原版 case4 有 `lastSceneId==1` 前置(0x252f36)天然免疫,一键进岛绕过了它。0=串门离线不存在。
            let seed = 1;
            let set_gm = env
                .objc
                .register_host_selector("setGameMode:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (ngm, set_gm, seed));
            log!(
                "[MOLECHEAT] island: gameMode seed(补原版 case4)NewGameManager.gameMode={}(主村 GameManager={})",
                seed,
                gmode
            );
        }
    }
    // [P1 离线持久化] 先试读 island_map.dat;读到有效布局就用它、跳过默认岛注入(沙原碎片仍补)。
    if load_island_map(env) {
        assign_island_seqids(env); // [P2b/P3a 修] 读档对象没有 seqId(不在 NSCoding 键里)→ 补发,否则回写/合并全被 seqId==0 守卫跳过
        load_island_fragments(env); // [P4-b] 先恢复玩家买到的碎片
        // [扫描修 2026-09-15] F1-3/F5-10 纠错:补的是沙原碎片(31005/31007 原版是岛任务 81/83 奖励、商店不卖),不是火山。
        inject_sandgarden_fragments(env, nsd); // [2026-09-16] 再兜底沙原碎片:老档补齐 4 块,其余只补任务 81/83 已完成却缺的 31005/31007(去重)
        migrate_island_timestamps(env); // [审计修] unix 纪元残留 → CFAbsoluteTime
        load_island_ships(env); // [审计修] 船 shipState/待领奖品、咖啡馆 isNew(不在 NSCoding 里)
        fix_stuck_ships(env); // [审计修] 唯一会永久卡死的船状态组合兜底
        restore_seqid_cursor(env); // [P3-a] 抬 seqId 游标到已存最大,防新放置撞号
        return true;
    }
    // ★[审计修 2026-09-11] 全新岛补"新岛"标志 newGame|=1。原版置位点是 -[NewSceneCommand parseMapDataWithPackageData:atIndex:]
    //   (0x22bd34,条件:服务器下发的岛剧情进度 nextStoryId==0 且非串门);唯一消费者 -[NewGameManager checkActiveStoryQuest]
    //   (endLoadMap 末尾调用)见到 bit0 就 [[NewSceneStory sharedInstance] startFromScratch] 播开场剧情并清位。离线 NewSceneUserInfoData
    //   init 的 nextStoryId 默认是 1,永远满足不了原版条件 → 新玩家的开场剧情走 activate 路径还要过等级/任务门,可能不播。
    //   读档岛绝不能设:startFromScratch 会强播第 1 节、播完 setNextStoryId:2 把进度回退。置脏让首个节拍尽快写出
    //   island_userinfo.dat,防止崩溃后再进岛重播。
    // [审查修 2026-09-13] D2 判据改为(读档之后):没读到有效 island_userinfo.dat,且 ISLAND_FILE_USERINFO 保护位未置位。
    //   · 文件不存在,或坏档已改名隔离成功(原档挪到 .corrupt,内存与之后落盘的都是全新进度)→ 位已清,按全新岛补 newGame;
    //   · 隔离失败、原坏档仍在原路径(本会话落盘被阻塞,玩家修好文件后还能恢复旧进度)→ 位保持置位,不补,免得给老玩家重播。
    //   顺带省掉一次读档前的 pathForDataFile: + fileExistsAtPath:。
    if !had_userinfo && (ISLAND_LOAD_FAILED.load(O) & ISLAND_FILE_USERINFO) == 0 {
        let gm_cls = env.objc.get_known_class("GameManager", &mut env.mem);
        let sm_s = island_sel(env, "sharedManager");
        let gm: id = msg_send(env, (gm_cls, sm_s));
        let gmode: i32 = if gm != nil {
            let g = island_sel(env, "gameMode");
            msg_send(env, (gm, g))
        } else {
            -1
        };
        if gmode != 0 && gmode != 6 {
            let ng = island_sel(env, "newGame");
            let cur: i32 = msg_send(env, (nsd, ng));
            let sng = island_sel(env, "setNewGame:");
            let _: () = msg_send(env, (nsd, sng, cur | 1));
            ISLAND_DIRTY.store(true, O);
            log!("[MOLECHEAT] island: 全新岛(无 island_userinfo.dat 或坏档已隔离)→ newGame|=1,进岛将播开场剧情");
        }
    }
    let dict = island_alloc_init(env, "NSMutableDictionary");
    if dict == nil {
        return false;
    }

    // ★Bug C(商店空格子)治本:商店目录 propertyHV 主村启动期已加载(5 桶×4 食材 30201-30220,
    // workflow 解密实证),但默认岛原来【只放 1 个商店 30101】→ 只它可逛、且 getShopItemsIds: 只
    // 服务 shopId∈[30101,30105]、点别的建筑返 0 格 = 全空。这里放全 5 个商店 30101-30105(各对应
    // 一个食材桶),同 key "28" 用 island_put_append 追加(原 island_put 会覆盖只剩1个)。
    // currentLevel 一律用已知安全值 4(商品锁已由 getLockType4ShopItem:shop:→0 全放开,level 不
    // 影响商品列表;避免高 level/99 的进岛卡死险)。baseTile 5 格错开不叠图。
    const ISLAND_SHOPS: [(i32, f32, f32); 5] = [
        (30101, 22.0, 42.0),
        (30102, 27.0, 42.0),
        (30103, 32.0, 42.0),
        (30104, 22.0, 47.0),
        (30105, 27.0, 47.0),
    ];
    for &(oid, tx, ty) in ISLAND_SHOPS.iter() {
        let shop = island_alloc_init(env, "TMMapDataShop");
        if shop != nil {
            obj_set_int(env, shop, "setObjectId:", oid);
            // [P2b 持久化命门] 非0 seqId:升级/操作回写靠 objectSequenceId 匹配;种子建筑 seqId=0 会被
            // 回写的 seqId==0 守卫跳过=升级丢。用 90001+ 高位(新建筑 seqId 从小自增,几乎不撞)。
            obj_set_int(env, shop, "setObjectSequenceId:", 90000 + (oid - 30100));
            island_set_point(env, shop, "setBaseTile:", tx, ty);
            obj_set_int(env, shop, "setIsFlip:", 0);
            island_set_double(env, shop, "setBeginTime:", 0.0);
            obj_set_int(env, shop, "setIsShopping:", 0);
            obj_set_int(env, shop, "setIsUpgrading:", 0);
            obj_set_int(env, shop, "setCurrentLevel:", 4); // 已知安全(非99/非0)
            obj_set_int(env, shop, "setSaleItemId:", 0);
            obj_set_int(env, shop, "setProperty:", 0);
            island_put_append(env, dict, "28", shop);
        }
    }
    // 物件2 餐厅 TMMapDataRestaurant 30002 @(11,39) → key "29"
    let rest = island_alloc_init(env, "TMMapDataRestaurant");
    if rest != nil {
        obj_set_int(env, rest, "setObjectId:", 30002);
        obj_set_int(env, rest, "setObjectSequenceId:", 90006); // [P2b] 非0 seqId,升级回写命门
        island_set_point(env, rest, "setBaseTile:", 11.0, 39.0);
        obj_set_int(env, rest, "setIsFlip:", 0);
        obj_set_int(env, rest, "setBeginUpgradeTime:", 0);
        obj_set_int(env, rest, "setProperty:", 1);
        // ★Bug B(摩尔公寓雇用恒弹"升级布兰的家")治本:餐厅 level 决定 moleUpperLimit。
        // levelupHV.dat 餐厅 30002 最低 level=1(→上限16),【没有 level 0】→ 注入 0 时
        // getUpgradeDataWithId:30002 andLevel:0 查无行 → moleUpperLimit=0 → 公寓雇用门
        // `produce+work >= 0` 恒真 → 永远弹框。改 1(workflow 解密 levelupHV 实证)。
        obj_set_int(env, rest, "setCurrentLevel:", 1);
        obj_set_int(env, rest, "setConstructValue:", 0);
        obj_set_int(env, rest, "setIslandValue:", 0);
        island_put(env, dict, "29", rest);
    }
    // 物件3 公寓/训练屋 TMMapDataApartment 30001 @(15,26) → key "32"
    let apt = island_alloc_init(env, "TMMapDataApartment");
    if apt != nil {
        obj_set_int(env, apt, "setObjectId:", 30001);
        obj_set_int(env, apt, "setObjectSequenceId:", 90007); // [P2b] 非0 seqId,雇用回写命门
        island_set_point(env, apt, "setBaseTile:", 15.0, 26.0);
        obj_set_int(env, apt, "setIsFlip:", 0);
        obj_set_int(env, apt, "setMoleNumInWaitingQueue:", 0);
        obj_set_int(env, apt, "setLastMoleFinishTrainingTime:", 0);
        island_put(env, dict, "32", apt);
    }
    // [P4-a 航海] 默认岛注入 1 艘探险船 DiscoveryShip(objectId 34001,mapData key "39")。其余字段
    //   (isFixing/isSailing/searchMapId/onBoardMoleNum/beginFixTime/beginDiscoverTime)默认 0 = 原版
    //   "需修船"初态(玩家点船→修船→出海,原版正确流程)。在 mapData 里→随 island_map.dat 持久,出海
    //   状态(isSailing_/searchMapId_/beginDiscoverTime_)一并存。DiscoveryShipView 面板无 gameMode 门。
    let ship = island_alloc_init(env, "TMMapDataShip");
    if ship != nil {
        obj_set_int(env, ship, "setObjectId:", 34001);
        obj_set_int(env, ship, "setObjectSequenceId:", 90008); // 非0 seqId,出海状态回写命门
        island_set_point(env, ship, "setBaseTile:", 37.0, -30.0); // 原版 addDiscoveryShipOnMap 水域坐标
        island_put(env, dict, "39", ship);
    }

    let set_s = env
        .objc
        .register_host_selector("setMapData:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (nsd, set_s, dict));

    // ★Bug D(探险地图碎片)补偿:mapFragments 离线无回包→恒空→探险船凑不齐;原来无条件注入沙原 4 块 31005-31008。
    //   已抽成 inject_sandgarden_fragments,持久化路径也复用。
    //   [2026-09-16] A1-02+A2-02 已降级为兜底:真新岛档不送商店可买的 31006/31008,31005/31007 按任务 81/83 进度补,规则见该函数注释。
    // [扫描修 2026-09-15] F5-10 纠错:-[NewSceneData activatedAdventureMap] 判的是 12 槽 / 3 张图(0x222f12 cmp #0xb),
    //   不是"只判这 4 槽";这里只保证沙原一张图可探险,火山(31009-31012)仍靠商店购买 + 咖啡任务 16/17。
    load_island_fragments(env); // [P4-b] 先恢复玩家买到的碎片(默认岛首进通常无,空过)
    inject_sandgarden_fragments(env, nsd); // [2026-09-16] 再兜底沙原碎片(真新岛档:31006/31008 走商店购买,31005/31007 按任务 81/83 进度补)
    restore_seqid_cursor(env); // [P3-a] 默认岛种子 seqId 90001-90008,抬游标到 90008 防新放置撞号

    log!("[MOLECHEAT] island: injected default mapData (5 shops 30101-30105 / restaurant 30002 / apartment 30001 / ship 34001)");
    true
}

/// 调试菜单「进入黄金岛(一键)」入口准备:只开启 NewScene 岛功能。随后 mole_menu 调
/// `[村庄层 enterNewIslands]` 走游戏自然进岛链——开窗(enterNewIslands hook)、异步 SUCC
/// (gate#1)、注入 mapData(getAllObjects hook)、解 state1 活锁(updateLoading hook)
/// 全部由本模块 intercept 自动接管。不要直接调 startNewSceneFrom(会绕过前置、网络门 bail)。
pub fn island_arm_entry() {
    // [扫描修 2026-09-15] F11-10 在线模式下离线岛总闸由 intercept 强制关闭,这里置 true 下一条消息就会被复位,
    //   等于假动作;直接不置(在线进岛走私服 1062 原版路径,由 mole_menu 的在线分支处理)。
    if ONLINE_MODE.load(O) {
        return;
    }
    ENABLE_NEWSCENE_ISLAND.store(true, O);
}

/// 岛会话是否活跃(进岛窗口开着或已在岛上)。菜单据此判断 isChangeSceneButtonSelected 卡 1 能否安全复位。
pub fn island_session_active() -> bool {
    ISLAND_ENTER_WINDOW.load(O) > 0
        || ON_ISLAND.load(O)
        || ISLAND_LOADING.load(O)
        || ISLAND_EXITING.load(O)
}

/// 本次进岛请求是否已走到 gate#1(=真 enterNewIslands 通过了前置门)。
pub fn island_gate1_hit() -> bool {
    ISLAND_GATE1_HIT.load(O)
}

// 曾有 force_gamemode_standby(把岛上 NewGameManager.gameMode 顶成 1),因会暂停 cocos2d director 冻结整岛而删除,勿复活。

// ===== 死循环看门狗(进岛卡死定位)=====
// 进岛卡死 = guest 陷入死循环、永远到不了下一帧 drawScene。看门狗在 run_inner 的每个
// yield 点检查:若 drawScene 帧计数 >3 秒没推进(=卡住),就自动 dump 当前 PC/LR/寄存器
// + FP 回溯链(rate-limit 1/秒),把死循环位置打到日志。仅 ENABLE_NEWSCENE_ISLAND 开时
// 启用(常态零开销)。比 GDB 省事:无需导航/中断,卡死自动抓现场。
static WD_FRAME: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static WD_SEEN_FRAME: Cell<u64> = const { Cell::new(0) };
    static WD_SEEN_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    static WD_LAST_DUMP: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 每帧 drawScene 调用:推进看门狗帧计数(证明游戏还在出帧)。
pub fn watchdog_frame() {
    WD_FRAME.fetch_add(1, O);
}

/// 在 run_inner 每个 yield 点调用:若帧计数 >3 秒没推进(卡死),dump 死循环现场。
pub fn watchdog_check(env: &mut Environment) {
    // ★只在岛上(进岛窗口开 / 已在岛)才看门狗。ENABLE 现已默认 ON,若仍只 gate ENABLE,
    // 主村/启动期任何正常的慢帧(首屏解码等)都会误报死循环。岛会话外一律早退。
    if !island_session_active() {
        return;
    }
    let now = Instant::now();
    let cur = WD_FRAME.load(O);
    if cur != WD_SEEN_FRAME.with(|c| c.get()) {
        WD_SEEN_FRAME.with(|c| c.set(cur));
        WD_SEEN_AT.with(|c| c.set(Some(now)));
        return;
    }
    let Some(t0) = WD_SEEN_AT.with(|c| c.get()) else {
        WD_SEEN_AT.with(|c| c.set(Some(now)));
        return;
    };
    if now.duration_since(t0).as_secs() < 3 {
        return;
    }
    // 卡死 >3 秒:rate-limit 1/秒 dump。
    let do_dump = WD_LAST_DUMP.with(|c| match c.get() {
        Some(t) if now.duration_since(t).as_millis() < 1000 => false,
        _ => {
            c.set(Some(now));
            true
        }
    });
    if !do_dump {
        return;
    }
    let regs = *env.cpu.regs();
    log!(
        "[WATCHDOG] guest 卡死 ~{}s — PC=0x{:08x} LR=0x{:08x} SP=0x{:08x} R0=0x{:08x} R1=0x{:08x} R4=0x{:08x}",
        now.duration_since(t0).as_secs(),
        regs[15],
        regs[14],
        regs[13],
        regs[0],
        regs[1],
        regs[4],
    );
    // FP 回溯链(保存的 LR):[fp]=上层 fp,[fp+4]=上层 lr。
    let mut fp = regs[crate::abi::FRAME_POINTER];
    let mut bt = String::new();
    for _ in 0..10 {
        if fp == 0 || fp & 3 != 0 {
            break;
        }
        let lr_ptr: ConstPtr<u32> = Ptr::from_bits(fp + 4);
        let saved_lr: u32 = env.mem.read(lr_ptr);
        bt.push_str(&format!(" 0x{:08x}", saved_lr));
        let fp_ptr: ConstPtr<u32> = Ptr::from_bits(fp);
        let next_fp: u32 = env.mem.read(fp_ptr);
        if next_fp <= fp {
            break;
        }
        fp = next_fp;
    }
    log!("[WATCHDOG] 回溯(LR链):{}", bt);
}

/// Flip a cheat on/off by its menu key.
pub fn toggle(key: &str) {
    match key {
        "free_shop" => FREE_SHOP.store(!FREE_SHOP.load(O), O),
        "kill_anticheat" => KILL_ANTICHEAT.store(!KILL_ANTICHEAT.load(O), O),
        "force_vip" => FORCE_VIP.store(!FORCE_VIP.load(O), O),
        "gold_x10" => GOLD_MULT.store(if GOLD_MULT.load(O) > 1 { 1 } else { 10 }, O),
        "xp_x10" => XP_MULT.store(if XP_MULT.load(O) > 1 { 1 } else { 10 }, O),
        "instant_crop" => INSTANT_CROP.store(!INSTANT_CROP.load(O), O),
        "no_wither" => NO_WITHER.store(!NO_WITHER.load(O), O),
        "no_cooldown" => NO_COOLDOWN.store(!NO_COOLDOWN.load(O), O),
        "instant_build" => INSTANT_BUILD.store(!INSTANT_BUILD.load(O), O),
        "all_unlock" => ALL_UNLOCK.store(!ALL_UNLOCK.load(O), O),
        "max_facility" => MAX_FACILITY.store(!MAX_FACILITY.load(O), O),
        "harvest_mult" => HARVEST_MULT.store(!HARVEST_MULT.load(O), O),
        "free_quest" => FREE_QUEST.store(!FREE_QUEST.load(O), O),
        "seabed_best" => SEABED_BEST.store(!SEABED_BEST.load(O), O),
        "minigame_reward" => MINIGAME_REWARD.store(!MINIGAME_REWARD.load(O), O),
        "all_achieve" => ALL_ACHIEVE.store(!ALL_ACHIEVE.load(O), O),
        "magic_bypass" => MAGIC_BYPASS.store(!MAGIC_BYPASS.load(O), O),
        "fix_golden_island" => FIX_GOLDEN_ISLAND.store(!FIX_GOLDEN_ISLAND.load(O), O),
        "golden_win" => {
            let v = !GOLDEN_WIN.load(O);
            GOLDEN_WIN.store(v, O);
            CARIBBEAN_DIRTY.store(true, O); // re-apply island fields on next read
            if v {
                FIX_GOLDEN_ISLAND.store(true, O); // "sail to finish" needs the fix on
            }
        }
        "enable_newscene_island" => {
            // [扫描修 2026-09-15] F11-10 在线模式下总闸每条消息都被 intercept 强制关闭,翻转没有意义且会误导(菜单显示已开、
            //   实际无效)。拒绝翻转并说明;is_on 也如实返回 false。
            if ONLINE_MODE.load(O) {
                log!("[MOLECHEAT] 在线模式:可建筑黄金岛由私服 1062 原版流程驱动,离线岛总闸保持关闭(开关不生效)");
            } else {
                ENABLE_NEWSCENE_ISLAND.store(!ENABLE_NEWSCENE_ISLAND.load(O), O)
            }
        }
        // 破解功能"按需复刻"开关 —— 改字节标志后置 dirty,下次 intercept 应用补丁。
        "kill_jailbreak" => {
            KILL_JAILBREAK.store(!KILL_JAILBREAK.load(O), O);
            CRACK_PATCHES_DIRTY.store(true, O);
        }
        "fix_divine" => {
            FIX_DIVINE.store(!FIX_DIVINE.load(O), O);
            CRACK_PATCHES_DIRTY.store(true, O);
        }
        "enter_holiday" => {
            ENTER_HOLIDAY.store(!ENTER_HOLIDAY.load(O), O);
            CRACK_PATCHES_DIRTY.store(true, O);
        }
        "store_no_vip" => {
            STORE_NO_VIP.store(!STORE_NO_VIP.load(O), O);
            CRACK_PATCHES_DIRTY.store(true, O);
        }
        "enter_newislands" => {
            ENTER_NEWISLANDS.store(!ENTER_NEWISLANDS.load(O), O);
            CRACK_PATCHES_DIRTY.store(true, O);
        }
        "skip_parse_check" => {
            SKIP_PARSE_CHECK.store(!SKIP_PARSE_CHECK.load(O), O);
            CRACK_PATCHES_DIRTY.store(true, O);
        }
        _ => {
            log!("[MOLECHEAT] unknown toggle key {}", key);
        }
    }
    log!("[MOLECHEAT] {} -> {}", key, is_on(key));
}

pub fn is_on(key: &str) -> bool {
    match key {
        "free_shop" => FREE_SHOP.load(O),
        "kill_anticheat" => KILL_ANTICHEAT.load(O),
        "force_vip" => FORCE_VIP.load(O),
        "gold_x10" => GOLD_MULT.load(O) > 1,
        "xp_x10" => XP_MULT.load(O) > 1,
        "instant_crop" => INSTANT_CROP.load(O),
        "no_wither" => NO_WITHER.load(O),
        "no_cooldown" => NO_COOLDOWN.load(O),
        "instant_build" => INSTANT_BUILD.load(O),
        "all_unlock" => ALL_UNLOCK.load(O),
        "max_facility" => MAX_FACILITY.load(O),
        "harvest_mult" => HARVEST_MULT.load(O),
        "free_quest" => FREE_QUEST.load(O),
        "seabed_best" => SEABED_BEST.load(O),
        "minigame_reward" => MINIGAME_REWARD.load(O),
        "all_achieve" => ALL_ACHIEVE.load(O),
        "magic_bypass" => MAGIC_BYPASS.load(O),
        "fix_golden_island" => FIX_GOLDEN_ISLAND.load(O),
        "golden_win" => GOLDEN_WIN.load(O),
        // [扫描修 2026-09-15] F11-10 在线模式如实显示"关"(总闸被 intercept 强制关闭)。
        "enable_newscene_island" => ENABLE_NEWSCENE_ISLAND.load(O) && !ONLINE_MODE.load(O),
        "kill_jailbreak" => KILL_JAILBREAK.load(O),
        "fix_divine" => FIX_DIVINE.load(O),
        "enter_holiday" => ENTER_HOLIDAY.load(O),
        "store_no_vip" => STORE_NO_VIP.load(O),
        "enter_newislands" => ENTER_NEWISLANDS.load(O),
        "skip_parse_check" => SKIP_PARSE_CHECK.load(O),
        _ => false,
    }
}

// ============================================================================
// 破解功能"按需复刻"层(香草基底)。把无限贝壳破解包的 inline 字节补丁做成运行时可开关
// 的菜单功能:每个开关 ON 时把破解作者的【精确字节】写到模拟内存对应 vaddr(并失效
// dynarmic JIT 缓存),OFF 时还原香草原字节 —— 逐字节复刻破解、可开可关、可验证。
// 字节表由 vanilla vs cracked 自动 diff 生成(勿手改)。不含贝壳写死 0xb9ce0:它不是可开关的功能。
// [2026-09-16] X2-01 原先这里写的「由 UserInfoData.initWithCoder hook 忠于存档处理」已过时:那个钩子在 be464e6(F1-05)
// 已删除。现在由下方 restore_cracked_vipgold 在 guest 代码运行前无条件检查,只在加载的是旧破解版二进制时写回
// 原版字节,永远不会写入破解字节。
// ============================================================================
#[derive(Clone, Copy, PartialEq)]
enum CrackGroup {
    Jailbreak,
    DivineFix,
    Holiday,
    StoreVip,
    Island,
    ParseSkip,
    /// 庄园持久化:NOP 掉 -[GameData saveMapData:] 的第4道闸(m_isLoadMap!=0→bail,0x768fa BNE.W)。
    /// 仅在线模式开(MAP_SYNC_PATCH);活图 objects.count=111 满图,其余4道闸都过,卡这一道→map 发 0B。
    MapSync,
}
struct CrackPatch {
    vaddr: u32,
    group: CrackGroup,
    vanilla: &'static [u8],
    cracked: &'static [u8],
}

/// 越狱检测去除(各 SDK 的 isJailbroken→NO)。touchHLE 下本无越狱痕迹,多为冗余,留作完整覆盖。
static KILL_JAILBREAK: AtomicBool = AtomicBool::new(false);
/// 修复占卜功能(@萌新迎风听雨 实测:占卜要正常,需 enterMiniGame 进门 + DivineGame 免费
/// 两组补丁【同时】生效,故合并为一个开关)。涵盖 MiniGameManager.enterMiniGame:stage: 绕门
/// + DivineGame.firstCostPlay / costGoldToDivine 免费。**默认开** —— 占卜开箱即用。
static FIX_DIVINE: AtomicBool = AtomicBool::new(true);
/// 节日村进入(HolidayVillageLayer.onEnter 去门)。
static ENTER_HOLIDAY: AtomicBool = AtomicBool::new(false);
/// 商城免 VIP 购买等级(NewStyleStoreMainLayer.purchaseCallback 去判断)。
static STORE_NO_VIP: AtomicBool = AtomicBool::new(false);
/// 进新岛门(VillageLayer.enterNewIslands 去 beq)。**默认 ON**:保留我们已稳定的黄金岛
/// 行为(破解包一直这么跑),换香草基底后关掉它可能把进岛门重新关上。
static ENTER_NEWISLANDS: AtomicBool = AtomicBool::new(true);
/// 跳过对象数据校验(GameData.parseObjectData: 一处取值强制 0)。默认 OFF=香草真值。
static SKIP_PARSE_CHECK: AtomicBool = AtomicBool::new(false);
/// 庄园持久化补丁(NOP saveMapData 第4道闸)开关。默认 OFF=香草;在线登录 arm 时置 ON(见 fire_online_login
/// 上游),让客户端能把活图整包经 updateInfoToServer 发上来。离线单机永不开,零污染。
static MAP_SYNC_PATCH: AtomicBool = AtomicBool::new(false);
/// 任一破解开关变更后置位;下次 intercept 把补丁写入/还原到模拟内存。初始 true=启动即按默认态应用。
static CRACK_PATCHES_DIRTY: AtomicBool = AtomicBool::new(true);

// 自动生成自 vanilla vs cracked diff —— 请勿手改字节
static CRACK_PATCHES: &[CrackPatch] = &[
    CrackPatch{vaddr:0x37650, group:CrackGroup::Island, vanilla:&[0x74,0xd0], cracked:&[0x00,0xbf]},
    CrackPatch{vaddr:0x6f1ea, group:CrackGroup::ParseSkip, vanilla:&[0x15,0xf0,0xb2,0xcf], cracked:&[0x4f,0xf0,0x00,0x00]},
    CrackPatch{vaddr:0x21638e, group:CrackGroup::DivineFix, vanilla:&[0x10,0xf0,0xff,0x0f,0x00,0xf0,0x91,0x80], cracked:&[0x00,0xbf,0x00,0xbf,0x00,0xbf,0x00,0xbf]},
    CrackPatch{vaddr:0x21718e, group:CrackGroup::DivineFix, vanilla:&[0x10,0xf0,0xff,0x0f,0x00,0xf0,0x95,0x80], cracked:&[0x00,0xbf,0x00,0xbf,0x00,0xbf,0x00,0xbf]},
    CrackPatch{vaddr:0xf4102, group:CrackGroup::DivineFix, vanilla:&[0x01,0x2b,0x40,0xf0,0x70,0x81,0x47,0xf6,0x50,0x40,0xc0,0xf2,0x9e,0x00,0x48,0xf2,0xfe,0x46,0xc0,0xf2,0x9f,0x06,0x78,0x44,0x7e,0x44,0x05,0x68,0x30,0x68,0x29,0x46,0x91,0xf3,0x16,0xe0,0x47,0xf6,0xae,0x51,0xc0,0xf2,0x9e,0x01,0x79,0x44,0x09,0x68,0x91,0xf3,0x0e,0xe0,0x10,0xf0,0xff,0x0f,0x00,0xf0,0x59,0x81,0x48,0xf2,0xac,0x50,0x29,0x46,0xc0,0xf2,0x9f,0x00,0x78,0x44,0x00,0x68,0x91,0xf3,0x00,0xe0,0x48,0xf2,0x34,0x61,0xc0,0xf2,0x9e,0x01,0x79,0x44,0x09,0x68,0x90,0xf3,0xf8], cracked:&[0x28,0xe0,0x47,0xf6,0x5c,0x50,0xc0,0xf2,0x9e,0x00,0x48,0xf6,0x6a,0x32,0xc0,0xf2,0x9f,0x02,0x78,0x44,0x7a,0x44,0x01,0x68,0x10,0x68,0x91,0xf3,0x18,0xe0,0x40,0xf2,0x04,0x41,0xc0,0xf2,0xa1,0x01,0x79,0x44,0x0e,0x68,0x4a,0xf6,0x90,0x51,0xc0,0xf2,0x9e,0x01,0x79,0x44,0xa0,0x51,0xa0,0x59,0x09,0x68,0x91,0xf3,0x08,0xe0,0x49,0xf2,0xf4,0x60,0xc0,0xf2,0x9e,0x00,0x4a,0xf6,0xb2,0x52,0xc0,0xf2,0x9e,0x02,0x78,0x44,0x7a,0x44,0x62,0xe0,0x01,0x2b,0x40,0xf0,0x46,0x81,0xd2,0xe7,0xe1]},
    CrackPatch{vaddr:0x2393ec, group:CrackGroup::Holiday, vanilla:&[0x23,0xd0], cracked:&[0x00,0xbf]},
    CrackPatch{vaddr:0x23940a, group:CrackGroup::Holiday, vanilla:&[0x1a,0xd0], cracked:&[0x00,0xbf]},
    CrackPatch{vaddr:0x239429, group:CrackGroup::Holiday, vanilla:&[0xd1], cracked:&[0xe0]},
    CrackPatch{vaddr:0x3b22c0, group:CrackGroup::StoreVip, vanilla:&[0x2b,0xd1], cracked:&[0x00,0xbf]},
    CrackPatch{vaddr:0x2fb9ec, group:CrackGroup::Jailbreak, vanilla:&[0x06], cracked:&[0x00]},
    CrackPatch{vaddr:0x4850ca, group:CrackGroup::Jailbreak, vanilla:&[0x07], cracked:&[0x00]},
    CrackPatch{vaddr:0x4f6d00, group:CrackGroup::Jailbreak, vanilla:&[0x45,0xf2,0xd8,0x30,0xc0,0xf2,0x5e,0x00,0x45,0xf6,0xa2,0x1a,0xc0,0xf2,0x5f,0x0a], cracked:&[0x40,0xf2,0x00,0x00,0xc0,0xf2,0x00,0x00,0x5c,0xe0,0x00,0xbf,0x00,0xbf,0x00,0xbf]},
    CrackPatch{vaddr:0x562c16, group:CrackGroup::Jailbreak, vanilla:&[0x07], cracked:&[0x00]},
    CrackPatch{vaddr:0x5757d8, group:CrackGroup::Jailbreak, vanilla:&[0x01], cracked:&[0x00]},
    CrackPatch{vaddr:0x606bb0, group:CrackGroup::Jailbreak, vanilla:&[0x04,0x00,0xa0,0xe1], cracked:&[0x00,0x00,0xa0,0xe3]},
    CrackPatch{vaddr:0x6b60d6, group:CrackGroup::Jailbreak, vanilla:&[0x05,0xd0], cracked:&[0x00,0xbf]},
    CrackPatch{vaddr:0x74c984, group:CrackGroup::Jailbreak, vanilla:&[0x01], cracked:&[0x00]},
    CrackPatch{vaddr:0x7c8de6, group:CrackGroup::Jailbreak, vanilla:&[0x01], cracked:&[0x00]},
    CrackPatch{vaddr:0x7c8e1c, group:CrackGroup::Jailbreak, vanilla:&[0x01], cracked:&[0x00]},
    CrackPatch{vaddr:0x85aaa0, group:CrackGroup::Jailbreak, vanilla:&[0x01,0x26,0x2a,0xf0,0x56,0xeb,0x10,0xf0,0xff,0x0f,0x18,0xbf,0x01], cracked:&[0x00,0x26,0x2a,0xf0,0x56,0xeb,0x10,0xf0,0xff,0x0f,0x18,0xbf,0x00]},
    // 庄园持久化:NOP -[GameData saveMapData:]@0x768fa 的 `BNE.W loc_7902C`(第4道闸 m_isLoadMap!=0→bail)。
    // 原字节 42 f0 97 83 = BNE.W;改成两个 16位 NOP(00 bf 00 bf)→落空不 bail→序列化活图 111 对象。
    // 仅在线模式(MAP_SYNC_PATCH)生效;离线为香草字节零改动。
    CrackPatch{vaddr:0x768fa, group:CrackGroup::MapSync, vanilla:&[0x42,0xf0,0x97,0x83], cracked:&[0x00,0xbf,0x00,0xbf]},
];

fn crack_group_on(g: CrackGroup) -> bool {
    match g {
        CrackGroup::Jailbreak => KILL_JAILBREAK.load(O),
        CrackGroup::DivineFix => FIX_DIVINE.load(O),
        CrackGroup::Holiday => ENTER_HOLIDAY.load(O),
        CrackGroup::StoreVip => STORE_NO_VIP.load(O),
        CrackGroup::Island => ENTER_NEWISLANDS.load(O),
        CrackGroup::ParseSkip => SKIP_PARSE_CHECK.load(O),
        CrackGroup::MapSync => MAP_SYNC_PATCH.load(O),
    }
}

/// 把各破解开关的当前状态写入模拟内存(ON→破解字节,OFF→香草字节)并失效 JIT 缓存。
/// 仅在 CRACK_PATCHES_DIRTY 时由 intercept 调用一次。写 __TEXT 是 host 侧直写(绕过 guest 只读页)。
fn apply_crack_patches(env: &mut Environment) {
    for p in CRACK_PATCHES {
        let bytes: &[u8] = if crack_group_on(p.group) { p.cracked } else { p.vanilla };
        let n = bytes.len() as u32;
        let ptr: MutPtr<u8> = Ptr::from_bits(p.vaddr);
        env.mem.bytes_at_mut(ptr, n).copy_from_slice(bytes);
        env.cpu.invalidate_cache_range(p.vaddr, n);
    }
    log!(
        "[MOLECHEAT] 破解补丁应用: 越狱={} 修复占卜={} 节日村={} 商城免VIP={} 进新岛={} 跳校验={}",
        KILL_JAILBREAK.load(O), FIX_DIVINE.load(O), ENTER_HOLIDAY.load(O),
        STORE_NO_VIP.load(O), ENTER_NEWISLANDS.load(O), SKIP_PARSE_CHECK.load(O)
    );
}

/// [2026-09-16] X2-01 旧破解版游戏包的「贝壳写死」强制还原,不受任何开关控制。
/// 为什么:v0.0.4 及更早的安卓 APK 内置的是无限贝壳破解包。首次启动复制到外部存储后,旧版 ensure_bundled_moleworld
///   从不覆盖,覆盖升级上来的老用户至今仍在跑破解二进制。破解包把 -[UserInfoData initWithCoder:]@0xb99f4 里
///   VA 0xb9ce0 的原版 `add r2,pc; mov r1,r6; blx`(即 [coder decodeIntForKey:@"vipGold"])换成
///   `movw r0,#0xffff; movt r0,#0x1f`(r0=2097151),后面的 encryptInt: → setNewVipGold: 两版相同。结果每次读档贝壳
///   都回满,花掉或买进的贝壳重启就失效。以前靠 messages.rs 的 initWithCoder: 钩子按存档真实值补救,F1-05(be464e6)
///   删掉钩子后这批用户没了兜底,CRACK_PATCHES 也不管这一处。lib.rs 已改成换 APK 后重新复制游戏包,这里再兜底一次:
///   复制失败退回旧拷贝,或者玩家自己放了旧破解包时,贝壳也不会被写死。
/// 做法:8 字节恰好等于破解版才写回原版字节并失效 JIT 缓存;香草基底(桌面、iOS、新复制的安卓包)什么都不做,也不打日志。
/// 时机:lib.rs 的 main() 在 Environment::new 返回之后、env.run() 之前调用。此时各二进制已装入内存并完成链接,
///   而 guest 代码(静态初始化器、_start → UIApplicationMain → 读档)要等 run() 恢复主线程协程才开始执行,
///   所以一定早于第一次 initWithCoder:。这里也不在任何帧栈上,不发 msg_send,dynarmic 和解释器都还没翻译过这段指令。
pub fn restore_cracked_vipgold(env: &mut Environment) {
    const VADDR: u32 = 0xb9ce0;
    const CRACKED: [u8; 8] = [0x4f, 0xf6, 0xff, 0x70, 0xc0, 0xf2, 0x1f, 0x00];
    const VANILLA: [u8; 8] = [0x7a, 0x44, 0x31, 0x46, 0xcb, 0xf3, 0x34, 0xe2];
    // 任何 app 启动都会调到这里。别的 app 的空页段如果盖住这个地址,bytes_at 会 panic;盖住就不可能是本游戏
    // (本游戏 __TEXT 从 0x4000 开始),直接跳过。
    if VADDR < env.mem.null_segment_size() {
        return;
    }
    let ptr: MutPtr<u8> = Ptr::from_bits(VADDR);
    if env.mem.bytes_at(ptr, 8) != &CRACKED[..] {
        return;
    }
    env.mem.bytes_at_mut(ptr, 8).copy_from_slice(&VANILLA);
    env.cpu.invalidate_cache_range(VADDR, 8);
    log!(
        "[MOLECHEAT] 检测到旧破解版游戏包(0xb9ce0 处贝壳写死为 2097151),已写回原版 decodeIntForKey:@\"vipGold\",贝壳按存档真实值读取"
    );
}

/// [MoleWorld] 在线进村存档 mapExtend 写错的修复开关。mapExtend 低5位=已扩展地图区域位掩码;
/// -[VillageLayer curVisibleArea] 取 `(unsigned __int8)mapExtend & 0x1F` 查可视区矩形。在线下发
/// 的 userinfo.mapExtend=6(只2区)却配满图内容(到 y148)→ 查到小/空可视区 → 拖动摄像机夹值
/// 震荡闪屏错位。强制 mapExtend getter 返回 0x1F(满图全区=不闪存档 287 的有效低字节)消除矛盾。
/// MOLE_FIX_MAPEXTEND=1 启用(确认阶段);确认后改默认策略。
/// ★[深扫修 2026-09-11] #12 语义改成与 ui43_mode 一致的 `!= "0"`:以前 `var_os().is_some()` 让 MOLE_FIX_MAPEXTEND=0
///   也算开启,与启动器注释"设 0 可关"矛盾。此前不敢改,是因为启动器靠 export 它来"保住 any_enabled 为真";
///   现在 any_enabled 已与环境变量脱钩(见下),设 0 只会关掉 mapExtend 修复本身,不再连带关掉常驻钩子。
/// ★[2026-09-16] F1-02 覆盖范围收窄:以前 getter 对全部 23 个调用点恒返回 0x1F,经 -[UserInfoData encodeWithCoder:] 每次存档
///   都把 0x1F 永久写进 userinfo.dat,并直接放开未修桥/梯的扩地摆放、扩地成就与任务判定。现在只对 VillageLayer
///   setBkg/curVisibleArea/curWalkableArea/curBornArea 这 4 个取景调用点返回 真值|0x1F(按调用者 LR 精确匹配,见
///   MAPEXTEND_VIEW_LRS 与 intercept 里的 mapExtend 臂),其余调用点一律读真值。开关默认值未改(交用户决定);
///   已被旧逻辑写成 0x1F 的存档无法自动还原。
fn fix_mapextend_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    // [同步 iOS 2026-09-16] 移植自 iOS 分支 c9ad2b6:桌面启动器已把 MOLE_FIX_MAPEXTEND 默认置 1;iOS 没有启动器
    // 和环境变量,默认开(MOLE_FIX_MAPEXTEND=0 可关)。其它平台行为不变。
    *V.get_or_init(|| {
        std::env::var("MOLE_FIX_MAPEXTEND")
            .map(|v| v != "0")
            .unwrap_or(cfg!(target_os = "ios"))
    })
}

/// [2026-09-16] F1-02 mapExtend 取景覆盖只认这 4 个调用者返回址(Thumb 返回址 = blx 地址 + 4 | 1,re.py annot 逐个核对):
///   -[VillageLayer setBkg] blx@0x334b8、curVisibleArea blx@0x350a4、curWalkableArea blx@0x351d8、curBornArea blx@0x3535c。
const MAPEXTEND_VIEW_LRS: [u32; 4] = [0x334bd, 0x350a9, 0x351dd, 0x35361];

/// objc/messages.rs 进入 intercept 的总闸。
/// ★[深扫修 2026-09-11] #12 无条件返回 true。
///   根因:intercept 里除了作弊开关,还有不受任何开关控制、必须常驻的钩子——去广告(checkPromptForLoadingNewApp /
///   showMoreGame* / AutoPopZhongXinLayer)、NewSceneTimer getCurrentServerTime 离线时钟、在线登录链(米米号注入 /
///   逐帧取包 / 地图上传 / moleHudTick)、moleIslandTick 节拍、以及本次新增的 GameData loadUserInfoData 偏好兜底。
///   旧实现只看作弊开关,默认能为真全靠 FIX_DIVINE/ENTER_NEWISLANDS/ENABLE_NEWSCENE_ISLAND 三个默认开;玩家在菜单把
///   它们关掉(且没开别的作弊)后,CRACK_PATCHES_DIRTY 被 swap 回 false,此后 intercept 永远不再被调用,常驻钩子全部
///   静默失效(发行包都不设 MOLE_FIX_MAPEXTEND,必中)。
///   为什么直接返回 true 最稳:默认配置下它本来就恒真,零行为/零性能变化;热路径开销由 intercept_wants 的零分配粗筛兜住,
///   不靠这里省;逐项补条件的写法以后每加一个常驻钩子都要记得同步,漏一个就复发。保留函数签名,调用方(messages.rs)不用改。
pub fn any_enabled() -> bool {
    true
}

// 曾有 any_cheat_toggle_on(旧总闸开关清单),因 any_enabled 已恒真、全仓零引用而删除,勿复活。

/// [扫描修 2026-09-15] F10-8 调试悬浮窗开关:默认【关】,MOLE_HUD 设为非 "0" 才开(只解析一次)。
/// 根因:以前 `unwrap_or(true)` 出厂即开,在线进 state 7 后每秒 24+ 次跨宿主 msg_send 外加一个泄漏串,
///   而 CCLabelTTF 文字至今不可见 = 纯开销。两个联网 .command 启动器的 MOLE_HUD 默认值也已同步改为 0。
fn hud_enabled() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("MOLE_HUD").map(|v| v != "0").unwrap_or(false))
}

/// Intercept a `[class sel ...]` message. Returns `true` if fully handled (the
/// caller must `return` without dispatching); `false` to let the real method
/// run (possibly with an argument register tweaked in place).
/// Schedule one HUD refresh ~1s out via performSelector:afterDelay: (run-loop perform phase). The
/// moleHudTick intercept runs update_debug_hud then calls this again, forming a 1s repeating timer
/// that lives entirely OUTSIDE the drawScene frame stack (so it never starves the run-loop / drops
/// the cf_stream Open event the way per-frame drawScene-stack msg_sends did).
fn schedule_hud_tick(env: &mut Environment) {
    let gm_cls = env.objc.get_known_class("GameManager", &mut env.mem);
    let smgr = env
        .objc
        .register_host_selector("sharedManager".to_string(), &mut env.mem);
    let gm: id = msg_send(env, (gm_cls, smgr));
    if gm == nil {
        return;
    }
    let tick = env
        .objc
        .register_host_selector("moleHudTick".to_string(), &mut env.mem);
    let perform = env.objc.register_host_selector(
        "performSelector:withObject:afterDelay:".to_string(),
        &mut env.mem,
    );
    let _: () = msg_send(env, (gm, perform, tick, nil, 1.0f64));
}

/// Draw/refresh the debug HUD overlay (connection state / RTT / packet counters) over whatever
/// scene is running. Mirrors the game's own HUD idiom (a CCLabelTTF on a CCLayer added to the
/// running scene at a high z; cf. TestLayer@0x1444a0). It self-heals across scene swaps: if the
/// tagged layer is gone (scene changed) it rebuilds, otherwise it just updates the label text.
/// 默认关,MOLE_HUD=1(非 "0")才开。armv7 ObjC ABI: float args to objc_msgSend are raw f32 bit
/// patterns in core registers; CGPoint = two consecutive 32-bit slots.
fn update_debug_hud(env: &mut Environment, mimi: u32) {
    // [扫描修 2026-09-15] F10-8 以前每个 1s 节拍都 std::env::var 一次;改读缓存。
    if !hud_enabled() {
        return;
    }
    let dir_cls = env.objc.get_known_class("CCDirector", &mut env.mem);
    let shared_dir = env
        .objc
        .register_host_selector("sharedDirector".to_string(), &mut env.mem);
    let dir: id = msg_send(env, (dir_cls, shared_dir));
    if dir == nil {
        return;
    }
    let running = env
        .objc
        .register_host_selector("runningScene".to_string(), &mut env.mem);
    let scene: id = msg_send(env, (dir, running));
    if scene == nil {
        return;
    }
    let nm_cls = env.objc.get_known_class("NetworkManager", &mut env.mem);
    let shared = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nm: id = msg_send(env, (nm_cls, shared));
    let state: i32 = if nm == nil {
        -1
    } else {
        let st = env.objc.register_host_selector("state".to_string(), &mut env.mem);
        msg_send(env, (nm, st))
    };
    let state_label = match state {
        0 => "空闲",
        1 => "连接中",
        2 => "请求连接",
        4 => "已连接",
        6 => "发送中",
        7 => "在线就绪",
        8 => "错误/断开",
        9 => "登录完成",
        _ => "?",
    };
    let sent = PKTS_SENT.load(O);
    let recv = PKTS_RECV.load(O);
    let rtt = LAST_RTT_MS.load(O);
    let pending = sent.saturating_sub(recv);
    // SAFE to read here: the HUD runs in the run-loop perform phase (the moleHudTick timer), NOT in
    // the packet-handler critical path, so these msg_sends can't clobber any in-flight method's args.
    // count: did the 1001 map unarchive (gzipInflate→NSKeyedUnarchiver) into a non-empty dict?
    // byte_B409B0: did the native 1234-reply handler set the fresh-login flag (the village-branch gate)?
    let map_count: i64 = {
        let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
        let gd: id = msg_send(env, (gd_cls, shared));
        let rmd: id = if gd == nil {
            nil
        } else {
            let s = env
                .objc
                .register_host_selector("remoteMapData".to_string(), &mut env.mem);
            msg_send(env, (gd, s))
        };
        let md: id = if rmd == nil {
            nil
        } else {
            let s = env
                .objc
                .register_host_selector("mapdata".to_string(), &mut env.mem);
            msg_send(env, (rmd, s))
        };
        if md == nil {
            -1
        } else {
            let dc = env.objc.get_known_class("NSDictionary", &mut env.mem);
            let ik = env
                .objc
                .register_host_selector("isKindOfClass:".to_string(), &mut env.mem);
            let isd: bool = msg_send(env, (md, ik, dc));
            if isd {
                let c = env
                    .objc
                    .register_host_selector("count".to_string(), &mut env.mem);
                let n: u32 = msg_send(env, (md, c));
                n as i64
            } else {
                -2
            }
        }
    };
    let b409: u8 = env.mem.read(crate::mem::ConstPtr::<u8>::from_bits(0xb409b0));
    // Which scene is actually on screen? -1 dir nil / -2 scene nil / 0 = NOT InGameScene (still title)
    // / 1 = InGameScene (village transitioned). Distinguishes "replaceScene didn't switch" from
    // "switched but InGameScene renders nothing".
    let scene_is_ingame: i32 = {
        let cd = env.objc.get_known_class("CCDirector", &mut env.mem);
        let sdir = env
            .objc
            .register_host_selector("sharedDirector".to_string(), &mut env.mem);
        let dir: id = msg_send(env, (cd, sdir));
        if dir == nil {
            -1
        } else {
            let rss = env
                .objc
                .register_host_selector("runningScene".to_string(), &mut env.mem);
            let scene: id = msg_send(env, (dir, rss));
            if scene == nil {
                -2
            } else {
                let igc = env.objc.get_known_class("InGameScene", &mut env.mem);
                let ik = env
                    .objc
                    .register_host_selector("isKindOfClass:".to_string(), &mut env.mem);
                let isig: bool = msg_send(env, (scene, ik, igc));
                if isig {
                    1
                } else {
                    0
                }
            }
        }
    };
    if LAST_MAP_COUNT.swap(map_count as i32, O) != map_count as i32 {
        log!(
            "[MOLECHEAT] 在线诊断(HUD,安全): remoteMapData.mapdata.count={} byte_B409B0={} runningScene_isInGame={}",
            map_count,
            b409,
            scene_is_ingame
        );
    }
    let text = format!(
        "[摩尔私服 DEBUG]\n米米号 {}\n状态 {} ({})\n延迟 {} ms\n发包 {}  收包 {}\n在途/丢 {}\n地图 {}  B409 {}",
        mimi, state_label, state, rtt, sent, recv, pending, map_count, b409
    );
    let ns_text = crate::frameworks::foundation::ns_string::from_rust_string(env, text);
    let get_tag = env
        .objc
        .register_host_selector("getChildByTag:".to_string(), &mut env.mem);
    let set_str = env
        .objc
        .register_host_selector("setString:".to_string(), &mut env.mem);
    let hud: id = msg_send(env, (scene, get_tag, 9000i32));
    if hud != nil {
        let lbl: id = msg_send(env, (hud, get_tag, 9001i32));
        if lbl != nil {
            let _: () = msg_send(env, (lbl, set_str, ns_text));
        }
        // [扫描修 2026-09-15] F10-7 -[CCLabelTTF setString:]@0x2ccde0 对参数 copy 后自存,不持有我们的 +1 → 释放。
        release(env, ns_text);
        return;
    }
    // Build it: a CCLayer holding one multi-line CCLabelTTF, anchored bottom-left.
    let set_tag = env
        .objc
        .register_host_selector("setTag:".to_string(), &mut env.mem);
    let node = env
        .objc
        .register_host_selector("node".to_string(), &mut env.mem);
    let layer_cls = env.objc.get_known_class("CCLayer", &mut env.mem);
    let hud: id = msg_send(env, (layer_cls, node));
    if hud == nil {
        release(env, ns_text); // [扫描修 2026-09-15] F10-7
        return;
    }
    let _: () = msg_send(env, (hud, set_tag, 9000i32));
    let lbl_cls = env.objc.get_known_class("CCLabelTTF", &mut env.mem);
    // [扫描修 2026-09-15] F10-7 字体名是固定串 → get_static_str(原 from_rust_string 的 +1 从不释放)。
    let font = crate::frameworks::foundation::ns_string::get_static_str(env, "Times New Roman");
    let label_with = env.objc.register_host_selector(
        "labelWithString:fontName:fontSize:".to_string(),
        &mut env.mem,
    );
    let lbl: id = msg_send(env, (lbl_cls, label_with, ns_text, font, 18.0f32.to_bits()));
    // [扫描修 2026-09-15] F10-7 initWithString:fontName:fontSize:@0x2ccd1c 内部 setString: 会 copy 文本 → 此后不再用 ns_text,释放 +1。
    release(env, ns_text);
    if lbl == nil {
        return;
    }
    let set_anchor = env
        .objc
        .register_host_selector("setAnchorPoint:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (lbl, set_anchor, 0u32, 0u32)); // (0,0) = bottom-left
    let set_pos = env
        .objc
        .register_host_selector("setPosition:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (lbl, set_pos, 8.0f32.to_bits(), 8.0f32.to_bits()));
    let set_color = env
        .objc
        .register_host_selector("setColor:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (lbl, set_color, 0x00_FF00u32)); // green ccColor3B
    let _: () = msg_send(env, (lbl, set_tag, 9001i32));
    let add_child = env
        .objc
        .register_host_selector("addChild:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (hud, add_child, lbl));
    let add_child_z = env
        .objc
        .register_host_selector("addChild:z:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (scene, add_child_z, hud, 99_999i32));
    log!("[MOLECHEAT] 调试悬浮窗已创建(默认关,MOLE_HUD=1 开启)");
}

/// 热路径粗筛(P0-B):这条消息的 class 或【不绑定 class 的】sel 是否【可能】被 intercept() 命中。
/// 游戏每帧约 16000 次 objc_msgSend 都过这里(any_enabled() 恒真),让 99% 不相关的消息在进
/// intercept(及其两次 to_string 堆分配 + 长比较链)之前就 return false。命中的少数才付出代价。
///
/// ⚠️【不变量——改 intercept() 时必须同步维护,漏一个 → release 下那个 hook 静默失效、破坏游戏】:
///   · CLASSES 必须含 intercept() 里每一个 `class == "X"` 与 `match (class,sel)` 臂里的 X;
///   · SELS 必须含每一个【不绑定具体 class】的 sel(裸 `if sel == "Y"`、`(_, "Y")` 通配臂);
///   · [扫描修 2026-09-15] F10-2 新增第三类「受模式门控的 sel」:intercept 里本身就带 `&& ui43_mode()` 的裸 sel
///     (winSize/onEnter/addChild:*)只在 ui43_mode() 为真时放行,写在下面单独一组;以后给 intercept 加
///     "某模式开才生效"的裸 sel 钩子,也要照此同步门控,别漏进无条件 SELS(白付两次堆分配 + 长比较链);
///   · 集成的 mole_dev / mole_items / mole_activity 各自的 wants 在末尾 OR 进来,由各模块自己维护。
///   class-pinned 的 sel 不必进 SELS——它的 class 已在 CLASSES 里兜住。
/// 当前列表 = 对 intercept 全函数体(1614+)穷举 grep `class ==` / `("X",` / 裸 `sel ==` / `(_,`
/// + 对抗式复查(missed_classes=[])得出(2026-06 性能优化)。
#[inline]
pub fn intercept_wants(class: &str, sel: &str) -> bool {
    matches!(
        class,
        "AsyncSocket"
            | "Building"
            | "Farm"
            // [深扫修 2026-09-11] #11 Farm 的两个子类(运行时类名不沿父类链):永不枯萎/作物瞬熟要覆盖花圃、果树
            | "FlowerFarm"
            | "FruitFarm"
            | "GameManager"
            | "HolidayVillageLayer"
            | "LoadingHoliday"
            | "LoadingLayer"
            | "MVPacketHeader"
            | "MainMenuScene"
            | "NetworkManager"
            | "NewSceneApartment"
            | "SeabedSeekingTreasureMainLayer"
            | "TMADataManager"
            | "TMAHttpManager"
            | "TMA_ASIFormDataRequest"
            | "TMA_ASIHTTPRequest"
            | "TMA_ASINetworkQueue"
            | "TMA_SSKeychain"
            | "TaomeeGetServerIpListManager"
            | "TaomeeUserInfo"
            | "UserInfoData"
            | "AchievementControl"
            | "AchievementItems"
            | "AvatarLayer"
            | "DecorateRoomLayer"
            | "FishingGame"
            | "GameData"
            | "MCNpcActor"
            | "MinerGame"
            | "MusicHallLayer"
            | "NewGameManager"
            | "NewSceneAchievement"
            | "NewSceneData"
            | "NewScenePorter"
            | "NewSceneRestaurant"
            | "NewSceneUserInfoData"
            | "ObjectManager"
            | "Quest"
            | "SystemTimeCheck"
            | "TimeQuest"
            | "UserInfoLayer"
            | "UserVIPInfoData"
            | "WrapperManager"
            | "YaliNpcActor"
            | "iMoleVillageAppDelegate"
            | "ShowAdwallBoardLayer" // [去广告] 淘米广告墙板("快来参战/现在去参战")
            | "AutoPopZhongXinLayer" // [去广告·真凶] 进村自动弹的"中心"促销弹窗(赛尔号/卡丁车跨游戏推荐)
            // [扫描修 2026-09-15] F11-3 云存档 compare 前补算远端 upgradePercent(类方法,元类名与类名相同)
            | "GameDataCompareLayer"
            // [扫描修 2026-09-15] F9-4 离线点好友入口给"需要联网"提示
            | "VillageMenuLayer"
            // [2026-09-16 黄金岛审查修] 岛上底部菜单条的同名好友入口(与主村是两个并列类,精确比较不走父类链)
            | "NewSceneVillageMenuLayer"
            // [扫描修 2026-09-15] F9-8 离线微博分享给"连不上网"提示,不进 ShareKit OAuth
            | "SharedInterfaceLayer"
            // [扫描修 2026-09-15] F12-10 左左右右(沙滩WC)开始前一次性操作提示
            | "WashRoomLevelChoose"
    ) || matches!(
        sel,
        "drawScene"
            | "mainLoop"
            | "moleHudTick"
            | "showWithTarget:"
            | "showWithTarget:selector:"
            | "checkPromptForLoadingNewApp" // [去广告] 赛尔号跨游戏广告弹窗触发器(GameManager)
            // [2026-09-16] B-05 删掉 getMoleCartAdImageFromServer:intercept 里对它只有一个不可达的诊断臂(唯一调用者就是上面的触发器)
            // [去广告·真凶] 淘米「更多游戏」跨游戏推荐弹窗的展示方法(赛尔号/摩尔卡丁车整屏弹窗)
            | "showMoreGameOnRootView:withScale:andOrientationSupported:"
            | "showMoreGameOnRootView:withScale:"
            | "showMoreGameWithScale:andOrientationSupported:"
            | "showMoreGameWithScale:"
            | "showMoreGameWithUrl:"
            | "onServerListResult:"
            | "showAccountManagerViewWithDelegate:andUserID:"
            | "enterLoadingWithDelegate:nextSceneId:"
            | "loadNewScene:"
            | "gobackMainVillage"
            | "deleteObjectFromServer:" // [审计修] 岛上删除/收纳建筑 → 从 mapData 移除
            | "startNewSceneFrom:toScene:" // [审计修] 离岛全局出口,统一落盘
            | "moleIslandTick" // [审计修] 岛存档节拍(GameManager 不实现,intercept 接住)
            | "getCurrentServerTime" // [审计修] 离线时钟返回 CFAbsoluteTime
            | "enterNewIslands"
            | "getAllObjectsListFromServerWithStartId:"
            | "getMatureTime"
            | "isReachable"
            | "sendPacket:commandId:"
            | "sendAllBufferDatas"
            | "sendAllBuffDataInNewSceneLoading"
            | "generateRandomRewardId"
            | "onTaomeeLoginViewDidUnloadWithUserID:password:returnCode:"
            // [P3 商店空白真因] -[SceneMannager curSceneId]:离线进岛后常卡在过场态 2(非10),
            //   loadObjectsDataByType: 据它选数据源→返回空→建设庄园/食材店空格。在岛上强制 10。
            | "curSceneId"
            // [扫描修 2026-09-15] F9-4/F9-8/F11-3/F12-10 新增钩子都绑定具体类,已由上面 CLASSES 兜住,SELS 无需新增
    ) || (ui43_mode()
        // [扫描修 2026-09-15] F10-2 受模式门控的 sel:intercept 里这几个臂本来就要求 ui43_mode()。以前无条件放行,
        //   默认启动器(未设 MOLE_UI43)下每帧几十次 winSize/addChild/onEnter 白白 to_string ×2 + 走完整条比较链。
        //   ui43_mode() 是 OnceLock,初始化后只是一次原子读,几乎零成本。
        && matches!(
            sel,
            "winSize" // [宽屏适配·UI 4:3 虚拟化] MOLE_UI43=1 时返回 1024x768(见 intercept)
                | "onEnter" // [宽屏适配·居中偏移 v2] 白名单 UI 根层进场:自身整体右移居中并登记
                | "addChild:" // [宽屏适配·居中偏移 v2] 已右移根层的迟到全宽背景子节点当场拉伸铺满
                | "addChild:z:"
                | "addChild:z:tag:"
                | "addSubview:" // [宽屏适配·居中偏移 v2] 挂到 EAGLView 上的 UIKit 子视图随根层右移
        ))
        // [同步 iOS 2026-09-16] 启动第一屏竖屏 winSize 修正:不论是否开 UI43,只在缓存可能还是竖屏时放行(闩住后一次原子读)。
        || (sel == "winSize" && WINSIZE_STALE.load(O))
        // [2026-09-16] 宽屏宽版底图锚点对齐(不依赖 UI43;is_widescreen() 只读两个 OnceLock)。
        || (sel == "addChild:z:tag:" && crate::window::is_widescreen())
        // [2026-09-16] G-07 / A2-03+G-04 作弊开关新增臂的粗筛,按 F10-2「受开关门控的 sel」写法:只在对应开关开着时放行这几个选择子。
        //   没把 NewSceneQuest/DailyQuest/VipQuest/NewSceneShop/Bridge/Ladder/SpacialObject/YellowDuck、CutFruit/BugGame/Plow/WashRoomGame
        //   加进上面的 CLASSES:那样开关关着时,这些类的每条消息(地图对象每帧的 innerupdate:/visit、小游戏每帧的更新)也要 to_string 两次、
        //   走完整条比较链,还会被 mole_dev/mole_items/mole_activity 的子拦截看到,改变现有路由。门控写法下关着时只多几次原子读。
        //   臂本身仍按类名精确匹配(小游戏按调用点 LR 匹配),别的类的同名方法进来只会被放行。
        //   CLASSES 里的 FishingGame/MinerGame 原本只给已删掉的 getRewardCoin:/getRewardXp: 臂用,现在没有臂再用;保留是为了不改变消息路由。
        || (FREE_QUEST.load(O) && sel == "shellsNeeded")
        || (INSTANT_BUILD.load(O) && sel == "getBuildTime:")
        || (NO_COOLDOWN.load(O) && sel == "getLastCooldownTime")
        || (MINIGAME_REWARD.load(O) && matches!(sel, "gainCoin" | "gainXP"))
        // [扫描修 2026-09-15] 集成:新模块各自的粗筛(各模块保证只做字符串比较,足够廉价)。
        || crate::mole_dev::wants(class, sel)
        || crate::mole_items::wants(class, sel)
        || crate::mole_activity::wants(class, sel)
}

/// [MoleWorld 宽屏适配·UI 4:3 虚拟化] 喂给白名单 UI 的原生设计尺寸(iPad landscape 4:3)。
const UI43_W: f32 = 1024.0;
const UI43_H: f32 = 768.0;
/// [2026-09-16] 宽屏宽版整屏底图 X_wide.png 的宽度(fs.rs 重定向,20d64e9 生成:原画居中、左右各外扩 384)。
const WIDE_BG_W: f32 = 1792.0;
/// [同步 iOS 2026-09-16] cocos2d 缓存的 winSize 是否可能仍是启动时的竖屏值(见 intercept 里「启动第一屏」臂)。
/// 一旦读到横屏就置 false,之后 winSize 在默认模式下不再进 intercept。
static WINSIZE_STALE: AtomicBool = AtomicBool::new(true);
/// [MoleWorld 宽屏适配·居中偏移 v2 · 根层整体右移] 已右移的 UI 根层(对象指针)登记表。
///
/// ★为什么从 v1"子节点逐个 +off"改成 v2"根层自身 position.x += off":
/// v1 让根层自己的坐标系与其子节点错开 off——根层代码拿 convertToGL/硬编码矩形做命中判断、把子节点
/// 摆到触摸点(小游戏鱼钩/放置)全部偏 322pt;商店 MenuView/ItemsView 的 ccTouchBegan 用
/// (0,0,winSize.w=1024,582) 触摸带对【真实】世界坐标做判定,把右 1/3 面板整个拒掉(反汇编 0x3b76b4
/// 实证)。这就是 iOS 上"触摸映射抽风"的真因。v2 下根层及整棵子树保持原 1024 设计坐标(=虚拟世界),
/// 只在【白名单代码与真实世界的交界处】做 ±off 换算——全部集中在 [intercept_fast] 里按 SEL 指针
/// 快判定(零分配,没有任何登记对象时只付一次原子读):
///   · 触摸/世界坐标进入白名单代码:locationInView:/previousLocationInView:/convertToWorldSpace(AR): 的
///     结果 x−off(按调用者 LR 落在白名单类代码段判定,见 [UI43_CODE_RANGES]);
///   · 白名单代码交出世界坐标:convertToNodeSpace(AR):/convertToUI: 的入参 x+off;
///   · 根层自身 position/setPosition:(任何 guest 调用者,含 CCMoveTo 等动作)getter −off / setter +off,
///     游戏侧永远看到虚拟坐标,cocos2d 内部变换直读 position_ ivar 拿真实值;
///   · 挂到 EAGLView 上的 UIKit 子视图(输入框/网页/好友表)见 [UI43_VIEWS]。
/// cocos2d 自己的命中(CCMenu itemForTouch / convertTouchToNodeSpace / 表格)走真实坐标 + 真实变换,天然正确。
///
/// ★宿主发起的消息一律不换算(`from_host`):touchHLE 的宿主 `msg_send` 走 CallFromHost,同样把参数写进
/// r0–r3(所以读寄存器对两种来源都成立),但**不会更新 LR**——run loop 里 LR 是陈旧的 main 返回地址
/// (guest 调 UIApplicationMain 时留下的),按它查白名单会把 UIControl/UIScrollView 宿主实现里的
/// `[touch locationInView:]` 误判成"白名单代码在问"而错扣 322 → UIButton 的 TouchUpInside 变成
/// TouchUpOutside。判据取 `message_type_info.is_some()`:由宿主 `msg_send` 设置,guest 派发时恒为 None
/// (见 objc/messages.rs)。本模块所有"转发真方法"都是宿主 msg_send,因此天然不会自我递归。
/// 登记表以对象指针为键,dealloc 时移除 → 地址复用不会误判;★锁绝不跨 msg_send 持有。
static UI43_ROOTS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
/// 登记表长度镜像(无锁快判定)。
static UI43_ROOTS_LEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn ui43_root_contains(p: u32) -> bool {
    if UI43_ROOTS_LEN.load(O) == 0 {
        return false;
    }
    UI43_ROOTS.lock().unwrap().contains(&p)
}
fn ui43_root_add(p: u32) {
    let mut v = UI43_ROOTS.lock().unwrap();
    if !v.contains(&p) {
        v.push(p);
    }
    UI43_ROOTS_LEN.store(v.len(), O);
}
fn ui43_root_remove(p: u32) {
    let mut v = UI43_ROOTS.lock().unwrap();
    v.retain(|&x| x != p);
    UI43_ROOTS_LEN.store(v.len(), O);
}

/// [MoleWorld 宽屏适配·居中偏移 v2 · UIKit 子视图] 已右移的 UIKit 子视图(对象指针)登记表。
///
/// 13 个白名单面板(留言/送礼留言/漂流瓶/公告板/邀请好友/注册/改昵称/海底寻宝/邀请码/活动码/帮助网页/
/// 乌鸦祭司)把 UITextField/UITextView/UIWebView 按 **1024 设计坐标**直接 addSubview 到
/// `[[CCDirector sharedDirector] openGLView]`;好友/消息/搜索三张 UITableView 由非白名单的
/// ManagerViewController 添加,但 frame 是白名单 VC 用(被虚拟成 1024 的)winSize 算的。这些视图不在
/// cocos 节点树里,根层右移后会与自己的面板底图错开 off,而且 UIKit 命中测试先于 EAGLView →
/// "看得见的输入框点不着、点旁边空白反而激活输入"。
/// 故:添加到 EAGLView 且 frame 完全落在设计区 [0,1024] 内的子视图 → frame.x += off 并登记;登记后
/// setFrame: 入参 +off、frame 返回 −off(只对 guest),键盘避让等游戏侧改位置的代码继续按设计坐标工作。
/// 坐标同向的依据:EAGLView 的 bounds 是横屏 1669×768(UIKit 旋转变换),cocos 走 convertToGL 的
/// Portrait 分支 (x, H−y),故 UIKit 视图 x 与 GL 世界 x 同向同尺度,+off 与根层右移一致。
static UI43_VIEWS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
static UI43_VIEWS_LEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn ui43_view_contains(p: u32) -> bool {
    if UI43_VIEWS_LEN.load(O) == 0 {
        return false;
    }
    UI43_VIEWS.lock().unwrap().contains(&p)
}
fn ui43_view_add(p: u32) {
    let mut v = UI43_VIEWS.lock().unwrap();
    if !v.contains(&p) {
        v.push(p);
    }
    UI43_VIEWS_LEN.store(v.len(), O);
}
fn ui43_view_remove(p: u32) {
    let mut v = UI43_VIEWS.lock().unwrap();
    v.retain(|&x| x != p);
    UI43_VIEWS_LEN.store(v.len(), O);
}

/// [MoleWorld 宽屏适配·重入保护] 正在转发真方法的 **guest 线程**位图。`from_host` 已经挡住了本模块
/// 自己的全部转发(都是宿主 msg_send),这里是防御性兜底:万一某条路径以 guest 身份重入,递归转发会爆栈。
/// ★不能用 thread_local:guest 线程是同一 OS 线程上的协程(environment.rs 的 corosensei::Coroutine),
/// 转发中途 run_inner 会 yield 给别的 guest 线程,OS 线程级标志会让那条线程误判"正在转发"而静默跳过
/// 一次换算(症状 = 偶发单次错位 322 且无日志)。
static UI43_INNER: AtomicU64 = AtomicU64::new(0);
fn ui43_inner_bit(env: &Environment) -> u64 {
    1u64 << ((env.current_thread as u64) & 63)
}
fn ui43_inner_active(env: &Environment) -> bool {
    UI43_INNER.load(O) & ui43_inner_bit(env) != 0
}
fn ui43_inner<R>(env: &mut Environment, f: impl FnOnce(&mut Environment) -> R) -> R {
    let bit = ui43_inner_bit(env);
    let was_set = UI43_INNER.fetch_or(bit, O) & bit != 0;
    let r = f(env);
    if !was_set {
        UI43_INNER.fetch_and(!bit, O);
    }
    r
}

/// [MoleWorld 宽屏适配·UI 4:3 虚拟化] 需要「按 4:3 原设计布局」的 `[CCDirector winSize]` 调用点
/// (返回地址 LR,已清 Thumb 位)。
///
/// 为什么用调用点而不是类名:winSize 的 receiver 运行时是 CCDirectorDisplayLink,拿不到"谁在问";
/// 而 LR 精确指向发起调用的那条指令之后(Thumb-2 `blx` 4 字节,LR=指令地址+4),可唯一定位到具体方法。
///
/// 名单由离线分析生成(全二进制反汇编找 winSize 调用点 → ObjC metadata 的 imp 地址表归属到 类.方法):
/// 共 **464 处调用点 / 264 个类**,其中 **170 个 UI 类的 240 处**纳入 4:3,**94 个类保持真实宽度**。
/// [2026-09-16] 生成器与自检已入库:touchHLE 目录下 `python3 dev-scripts/ui43_gen.py`(依赖 capstone),默认把
/// 生成结果与本文件三张表逐项比对。改三张表先改生成器里的分类数据,再按它的输出同步到这里。生成器实测 stret 调用点
/// 445 处 / 264 类(上面的 464 未能复现);纳入类的真实调用点是 239 处,另 1 处 0x1fffd6 是
/// -[NoticeBoardLayer showWithTarget:selector:] 里 [CCDirector sharedDirector](objc_msgSend)的返回地址,不是
/// winSize 调用点,下面查表永远匹配不上、不影响行为;为与已验收名单逐项一致暂留(见生成器 LEGACY_DEAD_CALLSITES)。
/// 保持真实宽度的是:世界场景与相机(VillageLayer/FriendsVillageLayer/InGameLayer/MoveLayer/CameraLayer
/// 的 checkBounding/zoom/moveToBaseTile,必须真实宽才能 Hor+ 显示更多海洋)、贴边 HUD 与菜单条
/// (VillageMenuLayer/TopMenuLayer,必须真实宽才贴得住屏幕边)、全屏画面(MainMenuScene/Logo/Loading,
/// 现已完美不动它)、世界内移动对象与飘字(Porter/GoldSprite/XPSprite…)、天气粒子(TM*/Partical/Wipe*)、
/// cocos2d 内部(CC*)。
/// 纳入 4:3 的是【多元素复杂布局】UI——不喂设计尺寸就会被 Δ=164pt 拉散(实证:商店网格散架、
/// 捉虫结算 "TOTAL" 截断、切水果卡片末项裁切):商店全套、8 类小游戏及其选关/成就面板、
/// 各节日活动弹窗、好友/礼物/任务/VIP/兑换等面板。
/// [2026-09-16 补 4 处] 白名单小游戏的子对象自己调 winSize 算方向/边界/出生点。当初生成名单时按「世界内移动
/// 对象」排除了,于是拿到真实宽 1188,和所在根层的 1024 虚拟坐标对不上:
///   · 0x147280 -[Fruit initWithType:type:parentNode:initPos:maxTime:minTime:]:initPos.x 与 width/3、2·width/3
///     比较来决定抛射方向。同类 -[Fruit genarateVelocity:] 的 0x147572 读的是 height(stret 缓冲在 sp+4、
///     读 [sp,#8]),不用加;
///   · 0x1a3f84 -[FishObject setFishPosition:isLeft:]:结果写进 ivar winSize(+468),再算鱼的入场点(左侧分支用
///     常量加随机数,右侧分支是否用 width 没逐条核实,纳入无害);
///   · 0x1af8c2 -[BugObject initwithFile:]:写进 ivar winSize(+500)。nextPositionFrom: 按它夹紧虫子 x,Level3/4
///     的虫子会跑到右侧 82pt 屏外点不到,左侧 82pt 却没有虫;0x1af96c 还按 width×常量算 speed,宽屏快约 16%;
///   · 0x35e6ac -[WashRoomActor initWithIndex:type:parentNode:pathType:]:写进 ivar winSize(+488),
///     getRandomOriginalPos 在 0x35e98e 取 width×0.5 算出生点,宽屏偏右 82。
/// 四个类都只由白名单小游戏创建(classref:Fruit←CutFruit、FishObject←FishingGame、BugObject←Level2/3/4、
/// WashRoomActor←WashRoomGame),主村不受影响。ActorManager GenarateScreenPos:(主村全局对象)和
/// GoldSprite/XPSprite/MovableIcon(世界飘字)仍保持真实宽度。★插入时必须保持升序,否则 binary_search 静默失效。
const UI43_CALLSITES: &[u32] = &[
    0xb468, 0xa71fe, 0xc07fa, 0xc93d0, 0xde600, 0xf2386, 0xf29fc, 0xf2b14,
    0xf2f46, 0xfbbe2, 0xfc76a, 0xfdb48, 0xfe6f4, 0xfe91c, 0x10fe60, 0x1102fc,
    0x110754, 0x110c42, 0x111952, 0x123932, 0x129e0a, 0x12d7c2, 0x134144, 0x134a86,
    0x13577e, 0x1358b6, 0x135bae, 0x136024, 0x137042, 0x1371d2, 0x1381d2, 0x13836e,
    0x138e24, 0x139e34, 0x13aab2, 0x13c338, 0x13c6e0, 0x13e318, 0x13f52e, 0x13f82a,
    0x140d86, 0x144532, 0x147280, 0x14cef4, 0x14e130, 0x14f94e, 0x150418, 0x152bd0, 0x156604,
    0x156916, 0x156ab2, 0x156da6, 0x158354, 0x159486, 0x159ac0, 0x164fa6, 0x165146,
    0x16641e, 0x1676d6, 0x168adc, 0x168fa0, 0x169eda, 0x16a06a, 0x16a3d2, 0x16ba8c,
    0x17176a, 0x174ade, 0x177ea2, 0x17b8ba, 0x17e12c, 0x17e51e, 0x17ea02, 0x17ec6c,
    0x17ed3a, 0x17f1ea, 0x17f37a, 0x17f66c, 0x1806c8, 0x180d98, 0x18667c, 0x188bc2,
    0x18a138, 0x18c2bc, 0x18c3e8, 0x18cc24, 0x18d1fa, 0x18e790, 0x190a2e, 0x192de0,
    0x193704, 0x193b36, 0x19c3d0, 0x1a24ec, 0x1a3f84, 0x1a6754, 0x1ac820, 0x1ae7e4, 0x1af46e,
    0x1af8c2, 0x1b11ec, 0x1b1c74, 0x1b2898, 0x1b40da, 0x1bb4a0, 0x1ccf1c, 0x1cf50a, 0x1d0a5c,
    0x1d2274, 0x1d33b4, 0x1d40ee, 0x1e299e, 0x1e50aa, 0x1e6206, 0x1e73d4, 0x1eb2c8,
    0x1f00a2, 0x1f21fc, 0x1f2c3c, 0x1fea1e, 0x1fffb6, 0x1fffd6, 0x1fffec, 0x200314,
    0x210a9a, 0x2126f0, 0x213060, 0x217e7e, 0x233188, 0x235e68, 0x23687a, 0x23f17e,
    0x246ce6, 0x24a802, 0x24d4b2, 0x2553ae, 0x27abfe, 0x2c0942, 0x2d9d7a, 0x2ec99a,
    0x2f68d0, 0x2f8190, 0x301562, 0x30ba98, 0x30f5d2, 0x310186, 0x3107ec, 0x318ef2,
    0x323c0c, 0x32d78e, 0x32ffea, 0x3319a2, 0x3335fe, 0x336bc4, 0x339d5a, 0x345a52,
    0x352f00, 0x3565c6, 0x358390, 0x359bae, 0x35cbfc, 0x35e6ac, 0x36a260, 0x36e3c6, 0x370270,
    0x370c80, 0x371140, 0x375fb6, 0x37794a, 0x3796b4, 0x37af1c, 0x37cb66, 0x37de44,
    0x37fb0a, 0x381434, 0x392f4a, 0x396402, 0x3969a8, 0x397618, 0x39ac00, 0x39ca68,
    0x3a035a, 0x3a3ef8, 0x3a8ddc, 0x3ae616, 0x3af228, 0x3afb16, 0x3b5230, 0x3b770c,
    0x3b786c, 0x3b8864, 0x3bda94, 0x3c18ce, 0x3c3284, 0x3c359e, 0x3c3a0e, 0x3c63f4,
    0x3c7d12, 0x3cae1c, 0x3cff0c, 0x3d8ffa, 0x3da4b0, 0x3dace4, 0x3dc2e0, 0x3df538,
    0x3e12e8, 0x3e21a0, 0x3e3b10, 0x3eced0, 0x3ede0c, 0x3ef0a6, 0x3f032c, 0x3f22d4,
    0x3f6f2e, 0x3f73f8, 0x3fa388, 0x3fa85c, 0x3fed4a, 0x40012a, 0x40088a, 0x401a52,
    0x401b5a, 0x4021f6, 0x40566a, 0x406c8c, 0x40942c, 0x40e86a, 0x40f4a2, 0x410a44,
    0x4147f0, 0x415254, 0x41664c, 0x41ddf4, 0x41ef5e, 0x41f566, 0x420112, 0x4212c4,
    0x4291e4, 0x42a360, 0x42bd38, 0x4318f6, 0x4319aa, 0x434120, 0x43486c, 0x435a72,
];

/// [MoleWorld 宽屏适配·UI 4:3 虚拟化·居中偏移] 需要整体右移居中的 UI 根层(运行时类名,含父类链匹配)。
/// 由离线分析生成:纳入 4:3 的 170 个类里剔除 Item/Cell/Sprite/Object/Control/Manager 等子节点或非节点类,
/// 剩 162 个"层/场景/视图"根类。按字典序排列供二分查找。
/// [2026-09-16 补 5 个] 共用任务框布局表(`[ResourceManager getPoint:@"quest_box"]` 等,npcdialogback.png 底图)
/// 的弹框:WiltWarningLayer(作物枯萎了)、HelpQuestLayer、TimeQuestLayer、VipQuestLayer、OscarDialogueLayer。
/// 它们从不调 winSize,坐标全来自 1024 设计布局表,所以按 winSize 调用点生成的名单漏掉了它们 → 宽屏下贴左不居中
/// (同模板的 QuestLayer/DailyQuestLayer/LevelUpLayer 早在名单里)。已核实五个类都没有自己的触摸处理和
/// locationInView:/convertTo* 调用(按钮走 CCMenu 真实变换),所以不需要补 UI43_CODE_RANGES。
/// [2026-09-16 补 4 个] 布局表类另补 4 个剧情对话层:StoryLayer(农场剧情)、TimeStoryLayer(限时任务剧情)、
/// VipStoryLayer(VIP 剧情)、NewSceneStoryLayer(黄金岛剧情)。它们同样是 CCLayer 直接子类、从不调 winSize:
/// -[StoryLayer nextStep]@0x114950(另三类在 0x1dde8c/0x385f7c/0x32e3bc,同一套代码)的对话条、左右 NPC、
/// 箭头、点击提示全按 getPoint:@"story_*" 摆放,point_sizeiPad.plist 里是 1024 设计坐标(如 story_right_npc
/// =(910,30)、story_right_arrow=(824,147))→ 宽屏下整体贴左 82pt,右侧露出村庄。挂法与名单里已有的层相同
/// (前三个在 -[InGameScene init] 里 addChild,NewSceneStoryLayer 在 -[GameNewScene addMainVillageLayer:]
/// 里和 NewSceneLevelUp/OscarDialogueLayer 挂到同一父节点)。四个类的 ccTouchesEnded:withEvent:
/// (0x115584/0x1deac0/0x386bb0/0x32efd4)只调 nextStep、不读坐标,所以同样不需要补 UI43_CODE_RANGES。
/// 整屏插图 story%d_wide(1792 宽)直接挂在层上,走 ui43_stretch_child 的 WIDE-KEEP 分支,不会被压扁。
/// ★插入时按字节序(与 &str 的 Ord 一致),否则 binary_search 静默失效。
const UI43_OFFSET_CLASSES: &[&str] = &[
    "AcceptFriendsLayer", "AccountBindingLayer", "AchieveSystemLayer", "AchivementLayer",
    "ActionCenterLayer", "ActionCodeLayer", "ActionLevelLayer", "ActivityBulletinLayer",
    "ActivityCaribbeanBasePopLayer", "ActivityFlameWarsSelectLayer", "ActivityForecastLayer", "ActivityForecastSecondLayer",
    "ActivityHalloweenBasePopLayer", "ActivityXmasBasePopLayer", "Activity_Alice_BasePopLayer", "Activity_FlameWars_BasePopLayer",
    "Activity_FlameWars_MainLayer", "Activity_IceCream_BasePopLayer", "Activity_Shrek_BasePopLayer", "Activity_Totoro_BasePopLayer",
    "AnimalsRecyclerView", "AnniversaryMainLayer", "AnniversarySubLayer", "ApartmentView",
    "ApplyHongKongTourLayer", "AroundTheWorldMainLayer", "AutumnMainLayer", "AvatarLayer",
    "BugAchivement", "BugGame", "BugLevelBase", "BugLevelChoose",
    "CafeShopLayer", "CandyhouseLayer", "CaribbeanMainLayer", "ChangeRewardLayer",
    "ChooseVillageHelp", "ChooseVillageLayer", "ChoosingPagesMainLayer", "CommonChristmasFatherGiftLayer",
    "CropInfoView", "CrowPriestMessageLayer", "CustomerServiceLayer", "CutFruit",
    "CutFruitAchivement", "CutFruitLevelChoose", "DailyQuestLayer", "DailySignLayer",
    "DecorateRoomLayer", "DiscountInfoLayer", "DivineGame", "DriftBottleMessageLayer",
    "EasterEggGetRewardLayer", "EasterEggMainLayer", "ExchangeCenterLayer", "FinalRewardAnimation",
    "FirstChargeGiftsLayer", "FishingAchivement", "FishingGame", "FishingLevelChoose",
    "FlyKiteGetRewardLayer", "FlyKiteIntroductionsLayer", "FlyKiteMainLayer", "FriendsViewController",
    "FuncIntroLayer", "GameDataCompareLayer", "GamePlayGoView", "GetItemRewardFromHaiwangLayer",
    "GetLastRewardLayer", "GiftAndMessageLayer", "GiftLayer", "GiftViewLayer",
    "GoodsViewLayer", "GreenRiceBallMainLayer", "GreenhouseLayer", "GuessWorldCupMainLayer",
    "HalloweenMainLayer", "HelpLayer", "HelpQuestLayer", "HouseRecyclerView",
    "IceSummerMainLayer", "InviteFriendsLayer", "JunkShopLayer", "LeaveMessageLayer",
    "LeoAdvanceLayer", "Level1", "Level2", "Level3",
    "Level4", "LevelChooseLayer", "LevelUpLayer", "MagicNumberView",
    "MessageBox", "MessageBoxGift", "MessageViewController", "MessagesLayer",
    "MinerAchivement", "MinerGame", "MinerLevelChoose", "MiniBase",
    "MusicHallLayer", "NaramGetTodayRewardLayer", "NaramSpringIntroduceLayer", "NaramSpringMainLayer",
    "NewRewardsLayer", "NewSceneLevelUp", "NewSceneQuestLayer", "NewSceneStoryLayer", "NewSceneTestLayer",
    "NewStyleStoreItemsView", "NewStyleStoreMainLayer", "NewStyleStoreMenuView", "NoticeBoardLayer",
    "OpenTreasureChestMainLayer", "OptionLayer", "OscarDialogueLayer", "PaintingAchivement",
    "PaintingGame", "PaintingLevelChoose", "PaybackObjectsTableLayer", "PersonalTargetLayer",
    "Plow", "PlowAchivement", "PlowLevelChoose", "PopularItemsPKAdvanceLayer",
    "PopularItemsPKMainLayer", "PopularItemsPKVoteLayer", "PromoteSalesMainLayer", "PromoteShowItemsLayer",
    "QiXiAdvanceLayer", "QuestLayer", "QuestionnaireLayer", "ReceiveGiftLayer",
    "RegisterView", "RequestCodeLayer", "RestaurantView", "RewardLayer",
    "SeabedSeekingTreasureExchageRewardLayer", "SeabedSeekingTreasureMainLayer", "SeabedSeekingTreasureRuleLayer", "SealExchangeLayer",
    "SeekViewController", "ShopItemsLayer", "ShoppingView", "ShowActivityRuleLayer",
    "ShowFreeShellsLayer", "ShowMoreFriendsLayer", "ShowRuleLayer", "SpringPoemGetRewardLayer",
    "SpringPoemIntroduceLayer", "SpringPoemMainLayer", "SpringPoemPageLayer", "StoryLayer", "TeamTargetLayer",
    "TestLayer", "TimeQuestLayer", "TimeStoryLayer", "TourLineLayer", "TreasureHuntPopLayer",
    "TreasureRewardLayer", "VIPFunctionsLayer", "VIPLayer", "VerifyInviteCodeLayer",
    "VipQuestLayer", "VipStoryLayer", "WashRoomAchievement", "WashRoomGame", "WashRoomLevelChoose",
    "WaterTowerRewardView", "WiltWarningLayer", "XmasMainLayer",
];

/// [MoleWorld 宽屏适配·虚拟世界换算] 白名单 UI 类(含其子类,按父类链 ≤6 层)全部方法的代码地址区间
/// (已合并、升序、[start,end)),离线生成:`dev-scripts/ui43_gen.py` 直接遍历 __objc_classlist /
/// __objc_catlist 的 class_ro_t.baseMethods 拿到 imp→类.方法 的精确归属,再用 LC_FUNCTION_STARTS
/// 截断每个方法的结尾(191 类 3306 方法 → 100 段)。
/// [2026-09-16] 以前这里写的「dev-scripts 的生成器」其实不在仓库里(草稿区脚本,已丢),现已补进上面的路径;
/// 并入下面两个辅助类后是 193 类 3327 个方法入口 → 仍 100 段。补类、改区间都先改生成器再重跑,别手工改地址。
/// ★两个必须踩住的坑:①不能靠 `otool -ov` 文本行的大小写猜类名(会把 app delegate 的方法记到
/// CommonChristmasFatherGiftLayer 名下);②不能拿"下一个 imp"当方法结尾,那会把方法之间的非 ObjC
/// 代码(含 `main` @0xe890)吞进区间——宿主发消息时 LR 正是 main 里 `blx _UIApplicationMain` 的返回
/// 地址,一旦落在区间内就会把 UIKit 控件的触摸坐标也错扣 off。生成后自检:LC_FUNCTION_STARTS 里
/// 落在区间内的非白名单函数起点必须为 0。
/// 调用者 LR 落在区间内 = "白名单代码在问",此时触摸/世界坐标要按虚拟世界 ±off 换算。
/// [2026-09-16 扩 2 段] 两个不在类名单里、但只在白名单小游戏里用的触摸辅助类,并入紧挨着的下一段
/// (首尾正好相接,段数不变):
///   · TouchTrailLayer [0x143b6c,0x1444a0)(9 个方法):CutFruit 在 ccTouchBegan/Moved 里把触摸原样转发给它;
///     它在 0x143c30/0x143e58 调 locationInView:,再拿去 checkLists:touchPos: 和水果的虚拟坐标比对
///     (-[Fruit checkAreaTouched:] 0x148cac CGRectContainsPoint)→ 宽屏下切中判定和刀光都偏右 82pt。
///     前面的 0x1430c0..0x143b6c 是 CCBlade(刀光绘制),不纳入;
///   · BackgroundSprite [0x17d6e8,0x17e0ac)(12 个方法):ccTouchEnded:withEvent: 在 0x17d9b0 取 locationInView:
///     放进新建的 CCNode,回调 Level1-4 PrintMessage: 把拍打精灵 beat 摆过去 → 特效偏右 82pt。它的命中判定走
///     containsTouchLocation: 里的 convertTouchToNodeSpace:(不在换算表),修前修后都对。前面的
///     0x17d5d0..0x17d6e8 是 SeabedSeekingTreasureData,不纳入。
/// 两段都已按 LC_FUNCTION_STARTS 核对,只含该类的函数起点(自检时把这两个类当白名单);段内没有
/// convertToWorldSpace/convertToNodeSpace/convertToUI 调用,不会引入 +off 误伤;classref 只在 CutFruit、Level1-4。
const UI43_CODE_RANGES: &[(u32, u32)] = &[
    (0xb2c0, 0xe890), (0xa70fc, 0xa7eb8), (0xc06c8, 0xc5a30), (0xc92a4, 0xcbda8),
    (0xde4cc, 0xdf040), (0xf2298, 0xf32b8), (0xfbb64, 0xfbd88), (0xfc628, 0x1001d4),
    (0x10fdd0, 0x111858), (0x1118b0, 0x112234), (0x123804, 0x123d9c), (0x128844, 0x12ad80),
    (0x12d698, 0x12dda0), (0x133e4c, 0x1430c0), (0x143b6c, 0x147050), (0x14ce50, 0x151580),
    (0x152b50, 0x1596f8), (0x159a28, 0x165b38), (0x166388, 0x17d5d0), (0x17d6e8, 0x180b54),
    (0x180c6c, 0x182f90), (0x186500, 0x18c7e8), (0x18cb90, 0x1900f8), (0x190998, 0x193e24),
    (0x19c330, 0x19cf10), (0x1a2448, 0x1a3ef8), (0x1a66a8, 0x1a8e58), (0x1ab468, 0x1ae628),
    (0x1ae6d8, 0x1af850), (0x1b10e4, 0x1b35ec), (0x1b3f58, 0x1b89dc), (0x1baef0, 0x1bd7fc),
    (0x1cce58, 0x1d4480), (0x1e60b0, 0x1e7814), (0x1eb158, 0x1ef3c4), (0x1efd78, 0x1f49ac),
    (0x1fe968, 0x203124), (0x210a10, 0x212ec8), (0x212f7c, 0x218dec), (0x233040, 0x23669c),
    (0x23f050, 0x2401c8), (0x246be8, 0x24a58c), (0x24a618, 0x250a60), (0x2552a8, 0x2573b8),
    (0x27a92c, 0x27dc40), (0x2c0640, 0x2c3394), (0x2d9ce0, 0x2da998), (0x2ec868, 0x2edf60),
    (0x2f67b4, 0x2f6b90), (0x2f8058, 0x2f8a20), (0x3012d8, 0x3029d4), (0x30b920, 0x30cc74),
    (0x30f3a8, 0x310548), (0x3105d4, 0x318c98), (0x323b08, 0x326828), (0x32b660, 0x32e130),
    (0x32ff58, 0x331298), (0x331700, 0x332c88), (0x3334c0, 0x333f78), (0x336388, 0x339c00),
    (0x339c90, 0x33f69c), (0x3435f4, 0x345fcc), (0x352d60, 0x353fb0), (0x35645c, 0x3573f8),
    (0x358310, 0x35e040), (0x36a144, 0x36a584), (0x3700cc, 0x371028), (0x371088, 0x374028),
    (0x375e78, 0x378800), (0x379578, 0x37aac4), (0x37ae84, 0x37cadc), (0x37dd08, 0x37f930),
    (0x37fa18, 0x381138), (0x392ba8, 0x396c68), (0x396e98, 0x39da08), (0x3a0010, 0x3a19b4),
    (0x3a3e58, 0x3a50b8), (0x3a8938, 0x3aba8c), (0x3ae4e0, 0x3b2cc8), (0x3b4130, 0x3b90f4),
    (0x3b9130, 0x3be850), (0x3c1400, 0x3ca780), (0x3ca988, 0x3d8b40), (0x3da020, 0x3db924),
    (0x3dc180, 0x3dd244), (0x3df49c, 0x3e0418), (0x3e1038, 0x3e300c), (0x3e39ac, 0x3e702c),
    (0x3ece50, 0x3ed52c), (0x3edd00, 0x3f6a10), (0x3f6e84, 0x3fff5c), (0x4000a0, 0x4017c8),
    (0x4019a8, 0x413914), (0x4146e8, 0x414d54), (0x4151c4, 0x41592c), (0x416570, 0x41dc20),
    (0x41dce0, 0x4229e8), (0x428e88, 0x42f488), (0x4317b4, 0x432ca4), (0x433f28, 0x43aff4),
];
fn ui43_lr_in_wl(lr: u32) -> bool {
    let i = UI43_CODE_RANGES.partition_point(|&(s, _)| s <= lr);
    i > 0 && lr < UI43_CODE_RANGES[i - 1].1
}

/// [MoleWorld 宽屏适配·居中偏移] 4:3 虚拟窗口整体右移量 = (真实 landscape 宽 − 1024) / 2。
/// 1188 宽 → 82pt;原生 4:3(1024)→ 0(不偏移)。
fn ui43_offset_x(env: &Environment) -> f32 {
    let (_pw, ph) = env.window().device_family().portrait_size();
    ((ph as f32 - UI43_W) / 2.0).max(0.0)
}

/// [MoleWorld 宽屏适配·居中偏移] 对象(或其父类链 ≤6 层)是否属于 UI 根层白名单。
fn ui43_class_hit(env: &Environment, obj: id) -> bool {
    if obj == nil {
        return false;
    }
    let mut cls = crate::objc::ObjC::read_isa(obj, &env.mem);
    for _ in 0..6 {
        if cls == nil {
            return false;
        }
        let hit = {
            let name = env.objc.get_class_name(cls);
            UI43_OFFSET_CLASSES.binary_search(&name).is_ok()
        };
        if hit {
            return true;
        }
        cls = env.objc.get_superclass(cls);
    }
    false
}

/// [MoleWorld 宽屏适配·居中偏移] 对象(或其父类链 ≤6 层)是否为指定类的实例。
fn ui43_is_kind(env: &Environment, obj: id, want: &str) -> bool {
    if obj == nil {
        return false;
    }
    let mut cls = crate::objc::ObjC::read_isa(obj, &env.mem);
    for _ in 0..6 {
        if cls == nil {
            return false;
        }
        if env.objc.get_class_name(cls) == want {
            return true;
        }
        cls = env.objc.get_superclass(cls);
    }
    false
}

/// [MoleWorld 宽屏适配·居中偏移] 一次性注册本模块用到的选择子。
struct Ui43Sels {
    pos: SEL,
    set_pos: SEL,
    cs: SEL,
    ap: SEL,
    sx: SEL,
    set_sx: SEL,
    children: SEL,
    count: SEL,
    oai: SEL,
    parent: SEL,
    rel_ap: SEL,
    set_ap: SEL,
}
fn ui43_sels(env: &mut Environment) -> Ui43Sels {
    let mut r = |n: &str| env.objc.register_host_selector(n.to_string(), &mut env.mem);
    Ui43Sels {
        pos: r("position"),
        set_pos: r("setPosition:"),
        cs: r("contentSize"),
        ap: r("anchorPoint"),
        sx: r("scaleX"),
        set_sx: r("setScaleX:"),
        children: r("children"),
        count: r("count"),
        oai: r("objectAtIndex:"),
        parent: r("parent"),
        rel_ap: r("isRelativeAnchorPoint"),
        set_ap: r("setAnchorPoint:"),
    }
}

/// [MoleWorld 宽屏适配·诊断] [UI43] 逐节点日志:MOLE_UI43_DEBUG=1 开、=0 关。
/// **iOS 真机默认开**(没有环境变量,而 v2 虚拟世界方案仍在验收期;量很小:面板进场/铺底/子视图右移
/// 各一行,坐标换算前 40 次 + 之后每 200 次一行)。桌面默认关。验收结束后把 iOS 也改回默认关。
fn ui43_debug() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("MOLE_UI43_DEBUG").map(|v| v != "0").unwrap_or(cfg!(target_os = "ios")))
}

/// [MoleWorld 宽屏适配·诊断] 对象的运行时类名(nil → "nil")。
fn ui43_cls_name(env: &Environment, obj: id) -> String {
    if obj == nil {
        return "nil".to_string();
    }
    let cls = crate::objc::ObjC::read_isa(obj, &env.mem);
    if cls == nil {
        return "?".to_string();
    }
    env.objc.get_class_name(cls).to_string()
}

/// [MoleWorld 宽屏适配·居中偏移 v2] 全宽背景铺满:非白名单、**无子节点的叶子** CCSprite/CCLayerColor、
/// 有效宽 ≥900 = 整屏底图 → `setScaleX:` 横向拉到真实宽(木纹/面板底图拉 16% 肉眼不可见),并把
/// **左边缘**放到根层局部坐标 −off(根层已右移 off,对应世界 x=0)。其余子节点一律不动:它们在根层
/// 局部坐标里就是原 1024 设计坐标,随根层整体右移即居中。
///
/// ★左边缘公式必须按 cocos2d 的 `nodeToParentTransform`(0x2d3910 实证)推:
///   · 相对锚点(CCSprite 默认 YES): T(pos)·S·T(−a)        → left = pos.x − ap.x·real_w
///   · 非相对锚点(CCLayer/CCLayerColor 默认 NO): T(+a)·T(pos)·S·T(−a) → left = pos.x + ap.x·(cs.w − real_w)
/// 即**非相对锚点也照样绕锚点缩放**,只是多了一次 +a 预平移。初版把它当成"position 就是左边"
/// (nx = −off),于是 1024 宽、锚点 0.5 的半透明遮罩(RewardLayer/ReceiveGiftLayer/DiscountInfoLayer
/// 的 `[CCLayerColor layerWithColor:width:winSize.width height:]`)被推到 −645,屏幕右侧 322pt 不被遮罩。
/// 两个分支都只依赖 ap/cs/real_w/off,与当前 position 无关 ⇒ 幂等,addChild 链重复触发无害。
fn ui43_stretch_child(env: &mut Environment, ch: id, off: f32, real_w: f32, s: &Ui43Sels) {
    if ch == nil || ui43_class_hit(env, ch) {
        return;
    }
    if !(ui43_is_kind(env, ch, "CCSprite") || ui43_is_kind(env, ch, "CCLayerColor")) {
        return;
    }
    // [2026-09-16] 文字标签不是底图:CCLabelTTF/CCLabelBMFont/CCLabelAtlas 都继承 CCSprite 链,按 1024 宽
    //   dimensions 建的整行文字(实测一行 w=1024 的 CCLabelTTF)会被当成全宽底图横向拉 16%,字形变宽、居中点偏移。
    if ui43_is_kind(env, ch, "CCLabelTTF")
        || ui43_is_kind(env, ch, "CCLabelBMFont")
        || ui43_is_kind(env, ch, "CCLabelAtlas")
    {
        return;
    }
    let cs: CGSize = msg_send(env, (ch, s.cs));
    let sx: f32 = msg_send(env, (ch, s.sx));
    let kids: id = msg_send(env, (ch, s.children));
    let nkids: crate::mem::GuestUSize = if kids == nil {
        0
    } else {
        msg_send(env, (kids, s.count))
    };
    if !(cs.width * sx >= 900.0 && cs.width > 1.0 && nkids == 0) {
        return;
    }
    let pos: CGPoint = msg_send(env, (ch, s.pos));
    let ap: CGPoint = msg_send(env, (ch, s.ap));
    let rel: bool = msg_send(env, (ch, s.rel_ap));
    // [2026-09-16] 只拉宽、不压窄:本来就不窄于真实宽的底图(宽屏宽版底图 X_wide.png 是 1792 宽;
    //   头像面板底图游戏自己拉到 1228.8)以前也按 real_w/cs.w 重设 scaleX,会被横向压扁——
    //   钓鱼 fishbgiPad_wide.png 被压到 0.66 倍。这类图保持原缩放,只在没盖住整屏时平移补齐
    //   (局部坐标需要盖住 [−off, real_w−off]);左右边缘按与下面同一套 nodeToParentTransform 公式、
    //   用当前 scaleX 推算。居中的宽图本来就盖满 → 不动,幂等。
    let eff_w = cs.width * sx;
    if eff_w >= real_w - 0.5 {
        let left = if rel {
            pos.x - ap.x * eff_w
        } else {
            pos.x + ap.x * cs.width * (1.0 - sx)
        };
        let shift = if left > -off {
            -off - left
        } else if left + eff_w < real_w - off {
            real_w - off - (left + eff_w)
        } else {
            0.0
        };
        if shift != 0.0 {
            let _: () = msg_send(env, (ch, s.set_pos, CGPoint { x: pos.x + shift, y: pos.y }));
        }
        if ui43_debug() {
            let cname = ui43_cls_name(env, ch);
            log!(
                "[UI43]     child {} WIDE-KEEP eff_w={} sx={} left={} shift={}",
                cname, eff_w, sx, left, shift
            );
        }
        return;
    }
    let nx = if rel {
        ap.x * real_w - off
    } else {
        ap.x * (real_w - cs.width) - off
    };
    let _: () = msg_send(env, (ch, s.set_sx, real_w / cs.width));
    let _: () = msg_send(env, (ch, s.set_pos, CGPoint { x: nx, y: pos.y }));
    if ui43_debug() {
        let cname = ui43_cls_name(env, ch);
        let (cw, px, py) = (cs.width, pos.x, pos.y);
        log!(
            "[UI43]     child {} STRETCH w={} sx={}→{} pos=({},{})→({},{}) rel={}",
            cname, cw, sx, real_w / cw, px, py, nx, py, rel
        );
    }
}

/// [MoleWorld 宽屏适配·居中偏移 v2] CCLayerColor 根层的色块四边形:根层右移后,自身 (0,0)-(w,h) 的色块
/// 只盖世界 [off, off+w];直接改写 ivar `squareVertices_` 的 x 分量为局部 [−off, real_w−off] = 整屏铺满,
/// 不动 contentSize(游戏侧读到的仍是设计尺寸)。布局按 `-[CCLayerColor setContentSize:]` 反汇编
/// (0x2cd690)实证:v[i] = (x@+8i, y@+8i+4),只写 v1.x/v2.y/v3.x/v3.y,值 = 点 × CC_CONTENT_SCALE_FACTOR。
/// ★缩放因子从**纵向** v2.y/contentSize.height 反推:我们从不改 y 分量,所以本函数幂等
/// (用横向反推的话第二次会拿被自己改过的 x 当基准,把色块越推越偏)。
fn ui43_extend_color_quad(env: &mut Environment, obj: id, off: f32, real_w: f32, s: &Ui43Sels) {
    let name = "squareVertices_".to_string();
    let Some(iv) = env.objc.object_lookup_ivar(&env.mem, obj, &name) else {
        return;
    };
    let base: MutPtr<f32> = iv.cast();
    let cs: CGSize = msg_send(env, (obj, s.cs));
    let cur_h: f32 = env.mem.read(base + 5); // v[2].y = contentSize.height × scale
    let scale = if cs.height > 1.0 && cur_h > 1.0 {
        cur_h / cs.height
    } else {
        1.0
    };
    let x0 = -off * scale;
    let x1 = (real_w - off) * scale;
    env.mem.write(base, x0);
    env.mem.write(base + 4, x0);
    env.mem.write(base + 2, x1);
    env.mem.write(base + 6, x1);
    if ui43_debug() {
        let cw = cs.width;
        log!("[UI43]     root CCLayerColor quad x: [{}..{}] (scale={}, cs.w={})", x0, x1, scale, cw);
    }
}

/// [MoleWorld 宽屏适配·居中偏移 v2] 祖先链里有没有"已经右移过"的层。
/// ★不能只看直接父节点:cocos2d 的 onEnter 是自顶向下派发(`-[CCNode onEnter]` 先被发给自己、
/// 方法体里再 `makeObjectsPerformSelector:@selector(onEnter)` 给孩子),所以根层总是先登记;但白名单层
/// 可能挂在一个**非白名单容器**下面(实证:SpringPoemMainLayer → CCClipZoneLayer(非白名单)→ 三个
/// SpringPoemPageLayer(白名单);ActivityBulletinLayer 把 DailySignLayer 加到自己的背板 ivar 节点上),
/// 只看直接父节点会把它们当成新根层再右移一次(+322 画到屏外)并把它们的 position 也虚拟化。
fn ui43_has_shifted_ancestor(env: &mut Environment, node: id, s: &Ui43Sels) -> bool {
    let mut p: id = msg_send(env, (node, s.parent));
    for _ in 0..32 {
        if p == nil {
            return false;
        }
        if ui43_root_contains(p.to_bits()) || ui43_class_hit(env, p) {
            return true;
        }
        p = msg_send(env, (p, s.parent));
    }
    false
}

/// [MoleWorld 宽屏适配·居中偏移 v2] `onEnter` 拦截:白名单 UI **根层**(祖先链里没有已右移的层)进场 →
/// 自身 position.x += off(整棵子树居中)+ 登记 + 铺底(CCLayerColor 四边形外扩 / 全宽背景子节点拉伸)。
/// 已登记的根层重新进场只补一次色块外扩(游戏可能中途 setContentSize: 把四边形缩回设计宽);
/// 嵌套白名单子层什么都不做——它已随祖先整体右移。
/// 本拦截在真方法之前、之后放行;msg_send 会 clobber r0–r3,故保存/恢复。
fn ui43_center_on_enter(env: &mut Environment) {
    let recv: id = Ptr::from_bits(env.cpu.regs()[0]);
    if !ui43_class_hit(env, recv) {
        return;
    }
    let off = ui43_offset_x(env);
    if off < 1.0 {
        return;
    }
    let real_w = UI43_W + off * 2.0;
    let rb = recv.to_bits();
    let saved = [
        env.cpu.regs()[0],
        env.cpu.regs()[1],
        env.cpu.regs()[2],
        env.cpu.regs()[3],
    ];
    let s = ui43_sels(env);
    let already = ui43_root_contains(rb);
    let nested = !already && ui43_has_shifted_ancestor(env, recv, &s);
    if !already && !nested {
        let pos: CGPoint = msg_send(env, (recv, s.pos));
        let np = CGPoint { x: pos.x + off, y: pos.y };
        ui43_inner(env, |env| {
            let _: () = msg_send(env, (recv, s.set_pos, np));
        });
        ui43_root_add(rb);
    }
    if !nested && ui43_is_kind(env, recv, "CCLayerColor") {
        ui43_extend_color_quad(env, recv, off, real_w, &s);
    }
    if !already && !nested {
        let children: id = msg_send(env, (recv, s.children));
        if children != nil {
            let n: crate::mem::GuestUSize = msg_send(env, (children, s.count));
            for i in 0..n {
                let ch: id = msg_send(env, (children, s.oai, i));
                if ch != nil {
                    ui43_stretch_child(env, ch, off, real_w, &s);
                }
            }
        }
    }
    if ui43_debug() {
        let cn = ui43_cls_name(env, recv);
        let parent: id = msg_send(env, (recv, s.parent));
        let pn = ui43_cls_name(env, parent);
        log!(
            "[UI43] onEnter {} @{:#x} parent={} → {}",
            cn, rb, pn,
            if nested { "NESTED(skip)" } else if already { "ROOT(done)" } else { "ROOT-SHIFT" }
        );
    }
    for (i, v) in saved.iter().enumerate() {
        env.cpu.regs_mut()[i] = *v;
    }
}

/// [MoleWorld 宽屏适配·居中偏移 v2] `addChild:` / `addChild:z:` / `addChild:z:tag:` 拦截(r0=父, r2=子):
/// 父是**已右移根层** → 迟到的全宽背景子节点当场拉伸铺满;普通子节点不用管(局部坐标 = 设计坐标,
/// 随根层整体居中)。[ui43_stretch_child] 写的是绝对值(幂等),所以 addChild 链一次添加触发 2~3 次无害,
/// 不需要 (根,子) 去重表——那种表按裸指针记,子节点释放后地址被新背景复用会让新背景永远拉不开。
fn ui43_on_add_child(env: &mut Environment) {
    let recv: id = Ptr::from_bits(env.cpu.regs()[0]);
    let child: id = Ptr::from_bits(env.cpu.regs()[2]);
    if child == nil || !ui43_root_contains(recv.to_bits()) {
        return;
    }
    let off = ui43_offset_x(env);
    if off < 1.0 {
        return;
    }
    let real_w = UI43_W + off * 2.0;
    let saved = [
        env.cpu.regs()[0],
        env.cpu.regs()[1],
        env.cpu.regs()[2],
        env.cpu.regs()[3],
    ];
    let s = ui43_sels(env);
    ui43_stretch_child(env, child, off, real_w, &s);
    for (i, v) in saved.iter().enumerate() {
        env.cpu.regs_mut()[i] = *v;
    }
}

/// [2026-09-16] 宽屏宽版底图按设计锚点对齐。fs 层在宽屏下把 1024×768 整屏底图换成 X_wide.png(1792×768),
/// 宽图是**原画居中、左右各外扩 384** 生成的。游戏按 1024 宽设计摆放:锚点居中的(钓鱼 fishbg)换图后原画
/// 仍对准设计坐标;锚点贴左的(-[MinerGame setBg]@0x1380d8、-[Plow setBg]@0x15359c 都是
/// setAnchorPoint:(0,0) + setPosition:(0,0),再加到 fakeParent 容器上)换图后原画整体偏右 384,矿石/木桩
/// 与底图错位;叠加 UI43 根层右移后左侧还露出下面的村庄。
/// 修法:加进节点树时(addChild:z:tag: 是 cocos2d 所有 addChild 变体的汇合点)把锚点 x 从设计锚点 a
/// 映射成 (a·1024 + 384)/1792,原画左边缘就回到按 1024 宽设计时的位置。只认 contentSize 恰为 1792×768、
/// scaleX=1、锚点 x 恰为 0 或 1 的 CCSprite:映射后的锚点不再是 0/1,重复触发(子类 addChild 转发 super、
/// 同一精灵再次加入)天然幂等;锚点 0.5 映射后仍是 0.5,不用处理。4:3 下 is_widescreen() 为假,不进这里。
fn wide_bg_align_on_add_child(env: &mut Environment) {
    let child: id = Ptr::from_bits(env.cpu.regs()[2]);
    if child == nil || !ui43_is_kind(env, child, "CCSprite") {
        return;
    }
    let saved = [
        env.cpu.regs()[0],
        env.cpu.regs()[1],
        env.cpu.regs()[2],
        env.cpu.regs()[3],
    ];
    let s = ui43_sels(env);
    let cs: CGSize = msg_send(env, (child, s.cs));
    if (cs.width - WIDE_BG_W).abs() < 0.5 && (cs.height - UI43_H).abs() < 0.5 {
        let sx: f32 = msg_send(env, (child, s.sx));
        let ap: CGPoint = msg_send(env, (child, s.ap));
        if (sx - 1.0).abs() < 1e-3 && (ap.x == 0.0 || ap.x == 1.0) {
            let nx = (ap.x * UI43_W + (WIDE_BG_W - UI43_W) / 2.0) / WIDE_BG_W;
            let _: () = msg_send(env, (child, s.set_ap, CGPoint { x: nx, y: ap.y }));
            static N: AtomicU32 = AtomicU32::new(0);
            if N.fetch_add(1, O) < 20 {
                let (ax, ay) = (ap.x, ap.y);
                log!("[宽屏底图] 1792×768 宽版底图锚点 ({},{}) → ({},{}),原画对齐 1024 设计坐标", ax, ay, nx, ay);
            }
        }
    }
    for (i, v) in saved.iter().enumerate() {
        env.cpu.regs_mut()[i] = *v;
    }
}

/// [MoleWorld 宽屏适配·居中偏移 v2 · UIKit 子视图] `addSubview:` 拦截(r0=父 view, r2=子 view)。
/// 见 [UI43_VIEWS]:挂到 EAGLView 上、且 frame 完全落在 1024 设计区内的子视图 → x += off 并登记。
/// 按真实 winSize 布局的全屏视图(HUD/整屏网页)不落在设计区里,天然不动。
fn ui43_on_add_subview(env: &mut Environment) {
    let recv: id = Ptr::from_bits(env.cpu.regs()[0]);
    let child: id = Ptr::from_bits(env.cpu.regs()[2]);
    if child == nil || recv == nil || ui43_view_contains(child.to_bits()) {
        return;
    }
    let off = ui43_offset_x(env);
    if off < 1.0 || !ui43_is_kind(env, recv, "EAGLView") {
        return;
    }
    let saved = [
        env.cpu.regs()[0],
        env.cpu.regs()[1],
        env.cpu.regs()[2],
        env.cpu.regs()[3],
    ];
    let sel_frame = env.objc.register_host_selector("frame".to_string(), &mut env.mem);
    let sel_set_frame = env.objc.register_host_selector("setFrame:".to_string(), &mut env.mem);
    let f: CGRect = msg_send(env, (child, sel_frame));
    let (x, w) = (f.origin.x, f.size.width);
    if w > 0.0 && x >= -1.0 && x + w <= UI43_W + 1.0 {
        let nf = CGRect {
            origin: CGPoint { x: x + off, y: f.origin.y },
            size: f.size,
        };
        let _: () = msg_send(env, (child, sel_set_frame, nf));
        ui43_view_add(child.to_bits());
        if ui43_debug() {
            let cn = ui43_cls_name(env, child);
            log!("[UI43] addSubview {} @{:#x} frame.x {}→{} (w={})", cn, child.to_bits(), x, x + off, w);
        }
    } else if ui43_debug() {
        let cn = ui43_cls_name(env, child);
        log!("[UI43] addSubview {} @{:#x} SKIP(非设计区) frame=({},{})", cn, child.to_bits(), x, w);
    }
    for (i, v) in saved.iter().enumerate() {
        env.cpu.regs_mut()[i] = *v;
    }
}

/// [MoleWorld 宽屏适配·虚拟世界换算] stret 消息的 CGPoint 入参:r0=返回缓冲区, r1=self, r2=sel,
/// r3=点.x(位模式), [sp]=点.y。
fn ui43_point_arg(env: &Environment, regs: &[u32; 16]) -> CGPoint {
    let y: f32 = env.mem.read(ConstPtr::<f32>::from_bits(regs[13]));
    CGPoint { x: f32::from_bits(regs[3]), y }
}

/// [MoleWorld 宽屏适配·热路径] 虚拟世界换算用到的全部选择子的 SEL 指针(只解析一次,零分配)。
#[derive(Clone, Copy)]
struct Ui43FastSels {
    pos: u32,
    set_pos: u32,
    dealloc: u32,
    frame: u32,
    set_frame: u32,
    loc_in_view: u32,
    prev_loc_in_view: u32,
    to_world: u32,
    to_world_ar: u32,
    to_node: u32,
    to_node_ar: u32,
    to_ui: u32,
}
fn ui43_fast_sels(env: &mut Environment) -> Ui43FastSels {
    thread_local! {
        static SELS: std::cell::OnceCell<Ui43FastSels> = const { std::cell::OnceCell::new() };
    }
    SELS.with(|c| {
        *c.get_or_init(|| {
            let mut r = |n: &str| {
                env.objc
                    .register_host_selector(n.to_string(), &mut env.mem)
                    .to_bits()
            };
            Ui43FastSels {
                pos: r("position"),
                set_pos: r("setPosition:"),
                dealloc: r("dealloc"),
                frame: r("frame"),
                set_frame: r("setFrame:"),
                loc_in_view: r("locationInView:"),
                prev_loc_in_view: r("previousLocationInView:"),
                to_world: r("convertToWorldSpace:"),
                to_world_ar: r("convertToWorldSpaceAR:"),
                to_node: r("convertToNodeSpace:"),
                to_node_ar: r("convertToNodeSpaceAR:"),
                to_ui: r("convertToUI:"),
            }
        })
    })
}

/// [MoleWorld 宽屏适配·热路径] 虚拟世界换算的 SEL 指针快判定,在 `is_intercept_sel` 字符串化之前调用。
/// 没有任何登记对象时只付一次原子读;命中选择子之后才去读寄存器。`from_host` 见 [UI43_ROOTS] 注释。
/// 返回 true = 消息已在宿主侧完成(不再派发)。
pub fn intercept_fast(env: &mut Environment, sel: SEL, from_host: bool) -> bool {
    if UI43_ROOTS_LEN.load(O) == 0 && UI43_VIEWS_LEN.load(O) == 0 {
        return false;
    }
    let s = ui43_fast_sels(env);
    let sb = sel.to_bits();
    // ① dealloc:按对象指针清登记表。guest 的 release 和宿主的 release 都会走到这里,
    //    所以不看 from_host;对象一旦释放就必须除名,否则地址复用会张冠李戴。
    if sb == s.dealloc {
        let p = env.cpu.regs()[0];
        if ui43_root_contains(p) {
            ui43_root_remove(p);
            if ui43_debug() {
                log!("[UI43] root dealloc @{:#x}", p);
            }
        }
        if ui43_view_contains(p) {
            ui43_view_remove(p);
        }
        return false;
    }
    // 宿主发起 / 本模块正在转发:一律看真实坐标。
    if from_host || ui43_inner_active(env) {
        return false;
    }
    let kind = if sb == s.pos {
        1
    } else if sb == s.set_pos {
        2
    } else if sb == s.frame {
        3
    } else if sb == s.set_frame {
        4
    } else if sb == s.loc_in_view || sb == s.prev_loc_in_view {
        5
    } else if sb == s.to_world || sb == s.to_world_ar {
        6
    } else if sb == s.to_node || sb == s.to_node_ar || sb == s.to_ui {
        7
    } else {
        0
    };
    if kind == 0 {
        return false;
    }
    let off = ui43_offset_x(env);
    if off < 1.0 {
        return false;
    }
    let regs = *env.cpu.regs();
    match kind {
        // 已右移根层的 position(stret:r0=缓冲区, r1=self)
        1 => {
            if !ui43_root_contains(regs[1]) {
                return false;
            }
            let recv: id = Ptr::from_bits(regs[1]);
            let mut p: CGPoint = ui43_inner(env, |env| msg_send(env, (recv, sel)));
            p.x -= off;
            env.mem.write(MutPtr::<CGPoint>::from_bits(regs[0]), p);
            true
        }
        // 已右移根层的 setPosition:(r0=self, r2=x, r3=y)
        2 => {
            if !ui43_root_contains(regs[0]) {
                return false;
            }
            let recv: id = Ptr::from_bits(regs[0]);
            let p = CGPoint {
                x: f32::from_bits(regs[2]) + off,
                y: f32::from_bits(regs[3]),
            };
            ui43_inner(env, |env| {
                let _: () = msg_send(env, (recv, sel, p));
            });
            true
        }
        // 已右移 UIKit 子视图的 frame(stret:r0=缓冲区, r1=self)
        3 => {
            if !ui43_view_contains(regs[1]) {
                return false;
            }
            let recv: id = Ptr::from_bits(regs[1]);
            let mut f: CGRect = ui43_inner(env, |env| msg_send(env, (recv, sel)));
            f.origin.x -= off;
            env.mem.write(MutPtr::<CGRect>::from_bits(regs[0]), f);
            true
        }
        // 已右移 UIKit 子视图的 setFrame:(r0=self, r2=x, r3=y, [sp]=w, [sp+4]=h)
        4 => {
            if !ui43_view_contains(regs[0]) {
                return false;
            }
            let recv: id = Ptr::from_bits(regs[0]);
            let w: f32 = env.mem.read(ConstPtr::<f32>::from_bits(regs[13]));
            let h: f32 = env.mem.read(ConstPtr::<f32>::from_bits(regs[13] + 4));
            let f = CGRect {
                origin: CGPoint {
                    x: f32::from_bits(regs[2]) + off,
                    y: f32::from_bits(regs[3]),
                },
                size: CGSize { width: w, height: h },
            };
            ui43_inner(env, |env| {
                let _: () = msg_send(env, (recv, sel, f));
            });
            true
        }
        // 坐标换算:只在【白名单代码在问】且确实有根层被右移过时才动
        _ => {
            if UI43_ROOTS_LEN.load(O) == 0 || !ui43_lr_in_wl(env.cpu.regs()[14] & !1u32) {
                return false;
            }
            let recv: id = Ptr::from_bits(regs[1]);
            let out: CGPoint = match kind {
                5 => {
                    let view: id = Ptr::from_bits(regs[3]);
                    let mut p: CGPoint = ui43_inner(env, |env| msg_send(env, (recv, sel, view)));
                    // view==nil 返回窗口(竖屏)坐标,横轴不在 x 上,不动。
                    if view != nil {
                        p.x -= off;
                    }
                    p
                }
                6 => {
                    let p = ui43_point_arg(env, &regs);
                    let mut q: CGPoint = ui43_inner(env, |env| msg_send(env, (recv, sel, p)));
                    q.x -= off;
                    q
                }
                _ => {
                    let mut p = ui43_point_arg(env, &regs);
                    p.x += off;
                    ui43_inner(env, |env| msg_send(env, (recv, sel, p)))
                }
            };
            env.mem.write(MutPtr::<CGPoint>::from_bits(regs[0]), out);
            if ui43_debug() {
                static N: AtomicU32 = AtomicU32::new(0);
                let n = N.fetch_add(1, O);
                if n < 40 || n % 200 == 0 {
                    let (ox, oy) = (out.x, out.y);
                    let lr = env.cpu.regs()[14] & !1u32;
                    log!("[UI43] conv#{} kind={} lr={:#x} → ({:.0},{:.0})", n, kind, lr, ox, oy);
                }
            }
            true
        }
    }
}
/// [MoleWorld 宽屏适配·UI 4:3 虚拟化] MOLE_UI43=1 是否开启(winSize 返回 1024x768)。仅解析一次。
fn ui43_mode() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // [同步 iOS 2026-09-16] 移植自 iOS 分支 c9ad2b6:桌面靠启动器 export MOLE_UI43=1;iOS 没有环境变量,
    // 宽屏(--fill-screen 算出的逻辑屏比 4:3 宽)时自动开。MOLE_UI43=0/1 仍可覆盖;非 iOS 平台默认值不变。
    *S.get_or_init(|| {
        std::env::var("MOLE_UI43")
            .map(|v| v != "0")
            .unwrap_or_else(|_| cfg!(target_os = "ios") && crate::window::is_widescreen())
    })
}

/// [扫描修 2026-09-15] F9-4/F9-8 取游戏本地化文案:[[NSBundle mainBundle] localizedStringForKey:key value:@"" table:nil]
/// (与原版 onSharedToWeChat / DailySignLayer checkNetWork 的取法一致)。返回 autoreleased/静态串,调用方不释放。
/// 宿主方法签名是 (id,id,id)->id,参数类型已逐个对齐。
fn game_localized_string(env: &mut Environment, key: &'static str) -> id {
    let bundle_cls = env.objc.get_known_class("NSBundle", &mut env.mem);
    if bundle_cls == nil {
        return nil;
    }
    let main_s = island_sel(env, "mainBundle");
    let bundle: id = msg_send(env, (bundle_cls, main_s));
    if bundle == nil {
        return nil;
    }
    let k = crate::frameworks::foundation::ns_string::get_static_str(env, key);
    let empty = crate::frameworks::foundation::ns_string::get_static_str(env, "");
    let loc_s = island_sel(env, "localizedStringForKey:value:table:");
    msg_send(env, (bundle, loc_s, k, empty, nil))
}

/// [扫描修 2026-09-15] F9-4/F9-8/F12-10 用游戏自带 MessageBox 弹提示,逐参数照原版调用序列:
///   [[MessageBox sharedInstance] showWithTarget:target selector:callback title:nil message:msg type:type vipgold:0]
///   取证:-[SharedInterfaceLayer onSharedToWeChat]@0x1a5e8e 与 -[GameManager showNetworkErrorMessage]@0x26f3c 用 type 6
///   (buttonok → onButtonOK: 只关框,target/selector 传 0);-[DailySignLayer checkNetWork]@0x39a4ac 用 type 8 带回调
///   (buttonok1 → onButtonOK1:@0xcb650 关框后 [targetCallback_ performSelector:selector_])。
/// ABI:该方法 6 个参数,touchHLE 宿主 msg_send 只实现到"接收者+选择子+5 个参数"(objc/methods.rs impl_HostIMP 到 P5)。
///   AAPCS 下 r2=target、r3=selector,栈 sp+0=title、sp+4=message、sp+8=type、sp+0xc=vipgold;write_next_arg 按槽顺序连续写,
///   u64 低 32 位先写(abi.rs u64::to_regs)→ 把 type(低)与 vipgold(高,恒 0)合成一个 u64 放在第 5 个参数,
///   落到 sp+8/sp+0xc,与分开传逐字节相同。MessageBox 是游戏自己实现的方法,msg_send 不做类型校验。
/// 只能在非 drawScene/mainLoop 帧栈的钩子里调用(菜单/按钮回调);会打乱 r0-r3,由调用方处理。返回是否真的发出了弹框消息。
fn show_game_message_box(
    env: &mut Environment,
    message: id,
    box_type: u32,
    target: id,
    callback: SEL,
) -> bool {
    if message == nil {
        return false;
    }
    let mb_cls = env.objc.get_known_class("MessageBox", &mut env.mem);
    if mb_cls == nil {
        return false;
    }
    let sh = island_sel(env, "sharedInstance");
    let mb: id = msg_send(env, (mb_cls, sh));
    if mb == nil {
        return false;
    }
    let show_s = island_sel(env, "showWithTarget:selector:title:message:type:vipgold:");
    let type_and_vipgold: u64 = box_type as u64; // 低 32 位 = type,高 32 位 = vipgold(0)
    let _: () = msg_send(env, (mb, show_s, target, callback, nil, message, type_and_vipgold));
    true
}

/// [扫描修 2026-09-15] F11-1 照搬 -[MainMenuScene onButtonChangeIDSelected:]@0xb523c 开头的守卫:
///   0xb5282 isEnable(+235,槽 0xb03fa0)==0 → 返回;0xb5298 isClickingMenu(+268,槽 0xb03fac)!=0 → 返回。
///   偏移从 guest 的 _OBJC_IVAR 槽现读(兼容 touchHLE 非脆弱 ivar 修正写回),不写死。只读内存、不发消息、不碰寄存器。
///   返回 Some(isEnable 字节指针)= 守卫通过(原版会继续往下走);None = 守卫不通过或槽值异常,调用方一律放行原方法。
fn mainmenu_change_id_guard(env: &Environment, scene: u32) -> Option<MutPtr<u8>> {
    if scene == 0 {
        return None;
    }
    let off_enable: u32 = env.mem.read(ConstPtr::<u32>::from_bits(0xb03fa0));
    let off_clicking: u32 = env.mem.read(ConstPtr::<u32>::from_bits(0xb03fac));
    // MainMenuScene 的 ivar 都在 0x120 以内;越界说明槽没按预期初始化,放弃(宁可保持原样也不乱读写内存)。
    if off_enable == 0 || off_enable >= 0x1000 || off_clicking == 0 || off_clicking >= 0x1000 {
        return None;
    }
    let enable_ptr: MutPtr<u8> = Ptr::from_bits(scene + off_enable);
    let enable: u8 = env.mem.read(enable_ptr);
    let clicking: u8 = env
        .mem
        .read(ConstPtr::<u8>::from_bits(scene + off_clicking));
    if enable == 0 || clicking != 0 {
        return None;
    }
    Some(enable_ptr)
}

/// [扫描修 2026-09-15] F11-3 compare 前按原版算法补算远端 upgradePercent(详见 intercept 里的调用点注释)。
/// 与 +[GameDataCompareLayer compareRemoteGameDataWithLocalOne] 自己的前置条件一致:远端 mapdata 为空时原版直接
/// updateInfoToServer 返回 YES、根本不比进度,这里也不动;remoteUserInfoData 为 nil 同样不动。
fn sync_remote_upgrade_percent(env: &mut Environment) {
    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
    if gd_cls == nil {
        return;
    }
    let sh = island_sel(env, "sharedInstance");
    let gd: id = msg_send(env, (gd_cls, sh));
    if gd == nil {
        return;
    }
    let rmd_s = island_sel(env, "remoteMapData");
    let rmd: id = msg_send(env, (gd, rmd_s));
    if rmd == nil {
        return;
    }
    let md_s = island_sel(env, "mapdata");
    let md: id = msg_send(env, (rmd, md_s));
    if md == nil {
        return;
    }
    let cnt_s = island_sel(env, "count");
    let cnt: crate::mem::GuestUSize = msg_send(env, (md, cnt_s));
    if cnt == 0 {
        return;
    }
    let rui_s = island_sel(env, "remoteUserInfoData");
    let rui: id = msg_send(env, (gd, rui_s));
    if rui == nil {
        return;
    }
    // -[GameData calculatePercent:]@0x7a730 与 -[UserInfoData upgradePercent] 都是游戏方法,返回值按寄存器原样透传(u32)。
    let calc_s = island_sel(env, "calculatePercent:");
    let pct: u32 = msg_send(env, (gd, calc_s, rui));
    let get_s = island_sel(env, "upgradePercent");
    let old: u32 = msg_send(env, (rui, get_s));
    let set_s = island_sel(env, "setUpgradePercent:");
    let _: () = msg_send(env, (rui, set_s, pct));
    log!(
        "[MOLECHEAT] 在线:云存档 compare 前按原版 calculatePercent: 补算远端 upgradePercent {} → {}",
        old,
        pct
    );
}

pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> bool {
    // ★[2026-06-22 飞机进岛卡死修复] 离线黄金岛总开关 ENABLE_NEWSCENE_ISLAND 默认 ON(飞机/作弊菜单
    // 两条进岛路径等价)。仅【在线模式】(--allow-network-access)强制 OFF——在线下岛 hook(网络门强制
    // 在线/吞包/解活锁)会干扰私服真连接,且在线岛非功能点;离线(默认)保持 ON,飞机点击即进岛。
    // 注:主村期间岛 hook 本就空过(网络门 gated ISLAND_ENTER_WINDOW||ON_ISLAND),此处只为在线模式
    // 额外保险关掉总闸,确保你的服务器/在线工作零干扰。
    if env.options.network_access {
        // [扫描修 2026-09-15] F11-10 记下在线模式供菜单如实显示;先读再写,常态(已是 false)不做写操作。
        if !ONLINE_MODE.load(O) {
            ONLINE_MODE.store(true, O);
        }
        if ENABLE_NEWSCENE_ISLAND.load(O) {
            ENABLE_NEWSCENE_ISLAND.store(false, O);
        }
    }
    // 启动时 / 任一破解开关变更后,按当前开关状态把破解补丁写入或还原到模拟内存(香草基底)。
    // 写在最前面、只在 dirty 时跑一次:invalidate_cache_range 让 dynarmic 重新编译被改的指令。
    // [扫描修 2026-09-15] F10-2 先 load 再 swap:常态 dirty=false 时只做一次原子读,不再每条命中消息都做一次读-改-写。
    if CRACK_PATCHES_DIRTY.load(O) && CRACK_PATCHES_DIRTY.swap(false, O) {
        apply_crack_patches(env);
    }

    // [MoleWorld 宽屏适配·UI 4:3 虚拟化] MOLE_UI43=1:拦截 `[[CCDirector sharedDirector] winSize]`
    // 返回原生 4:3(1024x768),让【按 winSize 定位的 UI】(商店 NewStyleStoreMainLayer init 实证
    // 0x3ae612 走 msgSend_stret 调 winSize 后 setContentSize:)仍按原设计布局,不被宽 winSize 拉散。
    // ★ABI:CGSize(两个 CGFloat=f32)>4 字节 → objc_msgSend_stret,r0=返回缓冲区指针(r1=self)。
    // touchHLE 的 intercept 挂在 objc_msgSend_inner(messages.rs:260),stret 与普通 msgSend 同源,
    // 故这里直接把 8 字节写进 r0 缓冲区即可完成"返回"。
    // 现版:按调用者 LR 白名单区分——UI 类的 240 处调用点拿 4:3,世界场景相机/边界、贴边 HUD、全屏画面、
    // cocos2d 内部拿真实宽度(世界 Hor+ 不受影响)。开关见 [ui43_mode]:桌面 MOLE_UI43=1,iOS 宽屏时自动开。
    // [同步 iOS 2026-09-16] 启动第一屏(淘米游戏 logo)「右侧黑边 / 一半白一半黑」根治,移植自 iOS 分支 8bc7046,
    // 桌面 4:3 默认模式同样适用(无头实测:4:3 下前两帧右侧 22% 全黑,之后正常)。
    // cocos2d 的 winSize 是缓存 ivar(winSizeInPoints_,写于 setOpenGLView:/reshapeProjection:)。touchHLE 上 guest
    // 建 EAGLView 时窗口还是【竖屏】bounds(4:3 为 768×1024,--fill-screen 为 768×长边),横屏 bounds 要等旋转后
    // 才更新;而 iMoleVillageAppDelegate 的启动序列是 setOpenGLView:(0xf5a8)→ setDeviceOrientation:(0xf63e)
    // → runWithScene:(0xf8ba),首个场景 TaomeeLogoLayer::init(0x3c0c32)在旋转之前就按竖屏宽度布局了
    // 白底和 logo → 横屏画布右侧露黑。本游戏 Info.plist 只支持横屏,竖屏 winSize 任何时候都是错的:
    // 缓存值高>宽时直接返回对调后的横屏尺寸(UI43 开且调用点在白名单时返回 4:3 设计尺寸);ivar 一旦变成横屏
    // 就闩住,之后 winSize 只付一次原子读。纯 ivar 读,不发消息、不碰 r0-r3 以外的状态。
    if sel == "winSize" && WINSIZE_STALE.load(O) {
        let recv: id = Ptr::from_bits(env.cpu.regs()[1]);
        let cached = env
            .objc
            .object_lookup_ivar(&env.mem, recv, &"winSizeInPoints_".to_string())
            .map(|p| {
                let f: MutPtr<f32> = p.cast();
                (env.mem.read(f), env.mem.read(f + 1))
            });
        match cached {
            Some((cw, ch)) if ch > cw + 1.0 => {
                let lr = env.cpu.regs()[14] & !1u32;
                let (rw, rh) = if ui43_mode() && UI43_CALLSITES.binary_search(&lr).is_ok() {
                    (UI43_W, UI43_H)
                } else {
                    (ch, cw)
                };
                let buf = env.cpu.regs()[0];
                let w: MutPtr<f32> = Ptr::from_bits(buf);
                let h: MutPtr<f32> = Ptr::from_bits(buf + 4);
                env.mem.write(w, rw);
                env.mem.write(h, rh);
                static N: AtomicU32 = AtomicU32::new(0);
                let n = N.fetch_add(1, O);
                // [2026-09-16] 每次启动首屏布局期间都会命中约 5 次,以前每次启动往 touchHLE_log.txt 刷 5 行。
                // 首条保留 log! 作为修正生效的证据,其余降为 log_dbg!(调试时仍能打开);修正逻辑本身不变。
                if n == 0 {
                    log!("[启动第一屏] winSize 竖屏缓存修正 #{n} lr={lr:#x} ({cw},{ch}) → ({rw},{rh})");
                } else if n < 12 {
                    log_dbg!("[启动第一屏] winSize 竖屏缓存修正 #{n} lr={lr:#x} ({cw},{ch}) → ({rw},{rh})");
                }
                return true;
            }
            // 已经是横屏,或拿不到这个 ivar(不是 CCDirector):闩住,以后不再查。
            _ => WINSIZE_STALE.store(false, O),
        }
    }
    if sel == "winSize" && ui43_mode() {
        // 调用者返回地址(Thumb blx: LR = 调用点+4+1;查表前清 Thumb 位)。
        let lr = env.cpu.regs()[14] & !1u32;
        // 数组按地址升序生成 → 二分查找(winSize 每帧被调多次,避免 240 项线性扫描)。
        if UI43_CALLSITES.binary_search(&lr).is_ok() {
            let buf = env.cpu.regs()[0];
            let w: MutPtr<f32> = Ptr::from_bits(buf);
            let h: MutPtr<f32> = Ptr::from_bits(buf + 4);
            env.mem.write(w, UI43_W);
            env.mem.write(h, UI43_H);
            return true;
        }
        // 非白名单调用点(世界场景相机/边界、贴边 HUD、cocos2d 内部)→ 放行真方法拿真实宽度,
        // 世界 Hor+ 与贴边 UI 完全不受影响。
        return false;
    }

    // [MoleWorld 宽屏适配·居中偏移] 白名单 UI 根层进场 → 整体右移居中(见 ui43_center_on_enter)。
    // 永远 return false 让真 onEnter 继续跑(只是顺手改了 position)。
    if sel == "onEnter" && ui43_mode() {
        ui43_center_on_enter(env);
        return false;
    }
    // [2026-09-16] 宽屏宽版底图按设计锚点对齐(见 wide_bg_align_on_add_child);不 return,下面的 UI43 臂照常处理。
    if sel == "addChild:z:tag:" && crate::window::is_widescreen() {
        wide_bg_align_on_add_child(env);
    }
    // [MoleWorld 宽屏适配·居中偏移 v2] 已右移根层收到迟到的全宽背景子节点 → 当场拉伸铺满(见 ui43_on_add_child)。
    if ui43_mode() && (sel == "addChild:" || sel == "addChild:z:" || sel == "addChild:z:tag:") {
        ui43_on_add_child(env);
        return false;
    }
    // [MoleWorld 宽屏适配·居中偏移 v2 · UIKit 子视图] 挂到 EAGLView 上的输入框/网页/好友表随根层右移。
    if ui43_mode() && sel == "addSubview:" {
        ui43_on_add_subview(env);
        return false;
    }

    // [扫描修 2026-09-15] 集成新模块(依次调度 mole_dev → mole_items → mole_activity)。
    //   位置铁律:必须在破解补丁与 UI43 两段【之后】,且【早于】下面的去广告、岛上 isReachable/isConnected/state 通配臂
    //   与 sendPacket:commandId: 吞包臂——活动模块要按精确调用方 LR 先拿到这些调用,晚了就被通配臂吞掉。
    //   startup 只调一次(早于游戏读档:GameData 在 CLASSES 里,loadUserInfoData 首次进来时它已先跑)。
    //   startup 可能发宿主消息,而本条消息稍后可能被放行 → 快照并恢复 r0-r3。
    if !DEV_STARTUP_DONE.load(O) && !DEV_STARTUP_DONE.swap(true, O) {
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        crate::mole_dev::startup(env);
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
    }
    // [复核修 2026-09-15] R7-1:模块返回 None(不归它管)之前可能已经发过宿主 msg_send(如 mole_activity 的节日判定
    //   读 [NSTimeZone systemTimeZone]、旁路档读盘),r0-r3 已被改写;落到下面的 mole_cheats 逻辑后若最终放行,真方法就拿
    //   错的 self/参数执行(sendPacket:commandId: 会先写 self+204 再给野指针发 setSendFlag:)。在第一个模块前快照一次,
    //   每个模块返回 None 后都恢复,一次兜住所有模块的 None 路径;Some(r) 照旧直接返回(Some(false) 可能是模块有意改了参数)。
    let module_regs = [
        env.cpu.regs()[0],
        env.cpu.regs()[1],
        env.cpu.regs()[2],
        env.cpu.regs()[3],
    ];
    if let Some(r) = crate::mole_dev::intercept(env, class, sel) {
        return r;
    }
    env.cpu.regs_mut()[0..4].copy_from_slice(&module_regs);
    if let Some(r) = crate::mole_items::intercept(env, class, sel) {
        return r;
    }
    env.cpu.regs_mut()[0..4].copy_from_slice(&module_regs);
    if let Some(r) = crate::mole_activity::intercept(env, class, sel) {
        return r;
    }
    env.cpu.regs_mut()[0..4].copy_from_slice(&module_regs);

    // [MoleWorld 去广告] 淘米跨游戏广告弹窗 AdViewForMoleCart(如"赛尔号:王者归来 / 立即参战")。
    // 实测:它【不】走 showWithTarget(那条没命中过),而是 -[GameManager checkPromptForLoadingNewApp]
    // 触发 → getMoleCartAdImageFromServer → onImageRecieved → 直接 addChild 上屏(有 defaultAdImage 兜底,
    // 本端 HTTP 已 drop 也照弹)。所以正确的拦点是【触发器本身】:掐掉 checkPromptForLoadingNewApp,
    // 整条广告流程不启动。按 selector 收窄,不影响别的类。
    // [2026-09-16] B-05 拉图入口 getMoleCartAdImageFromServer 全二进制只有 1 处调用,在 -[GameManager checkPromptForLoadingNewApp]@0x25a24
    //   内(+0xce,0x25af2),下面已在触发器处无条件吞掉,所以原来这里的诊断臂永远走不到,已删。
    // [扫描修 2026-09-15] F10-6 去广告各日志点:每个点本进程首次用 log!(证明钩子生效、保留「去广告」关键字),之后降为 log_dbg!。
    // ★将来接私服「自定义公告推送」:这里改成——不 return,而是放行/改喂我们后台的 PNG;现在=纯 ban。
    if sel == "checkPromptForLoadingNewApp"
        || (class == "AdViewForMoleCart" && (sel == "showWithTarget:selector:" || sel == "showWithTarget:"))
    {
        log_first_then_dbg!(
            LOG1_AD_PROMPT,
            "[MOLECHEAT] 去广告:吞掉 {class} {sel}(淘米跨游戏广告/赛尔号弹窗触发器)"
        );
        return true; // handled —— 跳过真方法,广告不展示
    }
    // [去广告·真凶] 淘米「更多游戏」跨游戏推荐弹窗(赛尔号/摩尔卡丁车整屏弹窗):直接吞掉它的展示方法
    // showMoreGame*(OnRootView/WithScale/WithUrl)。无论推荐数据从哪来,整屏弹窗都不再展示。
    // 比 fake SDK 数据类干净(fake 数据反而可能弹"无游戏可推→试试其他"兜底)。showMoreGameButton(村里
    // 的小入口按钮)没列入白名单,保留不动,只掐自动整屏弹窗。
    if sel.starts_with("showMoreGame") {
        log_first_then_dbg!(
            LOG1_AD_MOREGAME,
            "[MOLECHEAT] 去广告:吞掉 {class} {sel}(淘米「更多游戏」跨游戏推荐弹窗)"
        );
        return true;
    }
    // [去广告·真凶确认] 淘米广告墙板 ShowAdwallBoardLayer("快来参战/现在去参战",赛尔号/卡丁车跨游戏推荐
    // 整屏弹窗)。它是 cocos2d 单例层,展示入口是 open(配 shareInstance)。直接吞掉 open → 板子永不展示。
    // 它不是 SDK 类(fake AdWalls* 拦不到),所以前面全没用;这才是真凶。
    // [去广告·真凶] 进村自动弹出的"中心"促销弹窗(赛尔号/卡丁车跨游戏推荐,Activity_zhongxin):
    // AutoPopZhongXinLayer(自动弹出中心层),展示入口 open/showLayer;连同广告墙板 ShowAdwallBoardLayer
    // 一起吞掉其展示方法。这俩是 cocos2d 单例层、进村被加进场景(onEnter 实证),fake SDK 类拦不到——
    // 这才是真凶。OnTouchPopZhongXinLayer 是玩家手动点开的中心,不碰它。
    if (class == "AutoPopZhongXinLayer" || class == "ShowAdwallBoardLayer")
        && (sel == "open" || sel == "showLayer")
    {
        log_first_then_dbg!(
            LOG1_AD_ZHONGXIN,
            "[MOLECHEAT] 去广告:吞掉 {class} {sel}(进村自动弹的跨游戏推荐弹窗 真凶)"
        );
        return true;
    }

    // ★[深扫修 2026-09-11] #3/#2 常驻钩子:主村读档前的偏好/截断档兜底(详见 guard_userinfo_before_load)。
    //   不受任何作弊开关控制(any_enabled 已恒真)。前置钩子里发了十几条消息,必须快照并恢复 r0-r3 再放行真方法。
    if class == "GameData" && sel == "loadUserInfoData" {
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        let gd: id = Ptr::from_bits(saved[0]);
        guard_userinfo_before_load(env, gd);
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
        return false;
    }
    // [深扫修 2026-09-11] #3 醒目日志:-[GameData alertView:clickedButtonAtIndex:]@0x754b4 就是 `exit(0)`
    //   (HACK_USERINFO_DATA_ERROR 弹框的回调,touchHLE 自动关框会立刻点到它)。只打日志、不吞 exit、不碰寄存器。
    if class == "GameData" && sel == "alertView:clickedButtonAtIndex:" {
        log!("[MOLECHEAT] ⚠️ GameData alertView:clickedButtonAtIndex: → 原版即将 exit(0)(本地存档校验失败/偏好缺失弹框被自动关闭,排查 userinfo.dat 与偏好 plist)");
    }

    // [扫描修 2026-09-15] F11-3 云存档 compare 前置(仅在线):+[GameDataCompareLayer compareRemoteGameDataWithLocalOne]@0x1bcbb8
    //   是无参类方法,逐项比 remoteUserInfoData 与本地 userInfoData 的 curLevel / vipGoldWithNewType / gold / upgradePercent。
    //   根因:1001 解析器 -[NetworkManager parseUserInfoData:pos:header:] 从不给远端 setUpgradePercent:(全二进制 5 处调用无它),
    //   远端恒 0 → 本地有进度就恒不等 → 服务端一改发 sendFlag≠1234,每次重登都弹选存档框。
    //   做法照原版自己的算法补上:ChooseVillageLayer onButtonPreviewRemoteSelected:@0x1828ac-0x1828d0 与
    //   -[GameData saveMapDataAndUserInfoToLocal]@0x7b4c4 都是 `pct = [x calculatePercent:userInfo]; [userInfo setUpgradePercent:pct]`,
    //   这里对远端用 [[GameData sharedInstance] calculatePercent:remoteUserInfoData](与 ChooseVillageLayer 版逐指令同构,不依赖 self)。
    //   只补算、不拿本地值抹平,三项真不等时照样弹框(保留原版"让玩家选"的语义)。发了宿主消息 → 恢复 r0-r3 后放行真方法。
    if class == "GameDataCompareLayer"
        && sel == "compareRemoteGameDataWithLocalOne"
        && env.options.network_access
    {
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        sync_remote_upgrade_percent(env);
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
        return false;
    }

    // [扫描修 2026-09-15] F9-4 离线点好友(排行/推荐/访客/串门都从这里进):原版 -[VillageMenuLayer onButtonFriendSelected:]@0x615dc
    //   全程无网络门,先 saveToLocal + 卸载主村地图再进空好友图,之后 getFriendsInfo 因 isReachable=0 静默 return,
    //   玩家只看到一张只有自己的空图、没有任何提示。改为在卸图之前弹游戏自带文案 ACTION_CENTER_NETWARNING
    //   (「该功能需要联网才能使用哦!」,与活动中心离线体验一致),吞掉按钮回调。菜单回调不在 drawScene 帧栈上,可以发消息。
    // [2026-09-16 黄金岛审查修] 黄金岛底部菜单条是另一个类 NewSceneVillageMenuLayer,它的
    //   -[NewSceneVillageMenuLayer onButtonFriendSelected:]@0x25a4bc 与主村同构:一路过 gameMode/hasTopView 等
    //   本地门后,0x25a70a 调 [NewSceneData saveUserinfoToLocal]、0x25a728 调 startNewSceneFrom:toScene: 离岛去好友村,
    //   同样没有任何网络门 → 离线点了就被甩进只有自己的空好友图,且岛也退了。原来这道拦截只认主村的类名,
    //   岛上这个入口是漏的。两个类的这个选择子行为一致,合并进同一道拦截即可(弹框失败仍旧恢复寄存器走原版)。
    if (class == "VillageMenuLayer" || class == "NewSceneVillageMenuLayer")
        && sel == "onButtonFriendSelected:"
        && !env.options.network_access
    {
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        let msg = game_localized_string(env, "ACTION_CENTER_NETWARNING");
        if show_game_message_box(env, msg, 6, nil, SEL::null()) {
            log!("[MOLECHEAT] 离线:好友/排行/串门入口需要联网 → 弹「该功能需要联网」提示,不卸载主村");
            env.cpu.regs_mut()[0] = 0;
            return true;
        }
        // 弹框没发出去(类/文案缺失):不静默吞按钮,恢复寄存器走原版(最坏只是进空好友图,能正常回村)。
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
    }

    // [2026-09-16 黄金岛审查修] 离线点「免费贝壳」(HUD 菜单条上的 buttonFreeShells.png,ivar buttonAdwall):
    //   -[NewSceneVillageMenuLayer onButtonAdwallSelected:]@0x25cb28 与主村 -[VillageMenuLayer …] 同构,
    //   先 setGameMode:1(0x25cb56,没人还原)再打开淘米广告墙 ShowFreeShellsLayer —— 离线时广告墙拉不到
    //   任何数据,玩家看到一张空板,关掉之后 gameMode 已被改过,岛上交互跟着不对。
    //   照 F9-4 的写法在【按钮回调】这一层拦:既掐掉 setGameMode,也掐掉关闭时跨场景的那次 onButtonBuildSelected:。
    //   ★不拦 ShowFreeShellsLayer.open 本身 —— 作弊菜单的「免费贝壳墙」正是主动调它,拦了就废。
    if (class == "NewSceneVillageMenuLayer" || class == "VillageMenuLayer")
        && sel == "onButtonAdwallSelected:"
        && !env.options.network_access
    {
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        let msg = game_localized_string(env, "ACTION_CENTER_NETWARNING");
        if show_game_message_box(env, msg, 6, nil, SEL::null()) {
            log!("[MOLECHEAT] 离线:免费贝壳(广告墙)需要联网 → 弹「该功能需要联网」提示,不进空广告墙、不改 gameMode");
            env.cpu.regs_mut()[0] = 0;
            return true;
        }
        // 弹框没发出去:恢复寄存器走原版(不静默吞)。
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
    }

    // [扫描修 2026-09-15] F9-8 离线微博分享:-[SharedInterfaceLayer onSharedToSinaWeibo]@0x1a58b8 与
    //   onSharedToSinaWeiboGetShareReward@0x1a5a30 都没有网络门,直接进 ShareKit(钥匙串恒空 → 未授权 → 弹 OAuth WebView,
    //   网页必失败、关闭按钮依赖缺失的 UIBarButtonItem)。原版微信分支 onSharedToWeChat@0x1a5d8a 离线时弹
    //   SINAWEIBO_NO_CONNECT 并留在分享层;这里对微博两个入口照搬这个分支(同一文案、type 6、不 detech),不进 OAuth。
    //   分享奖励原由服务器发放,来源未核实,不在本地凭空发奖。
    if class == "SharedInterfaceLayer"
        && (sel == "onSharedToSinaWeibo" || sel == "onSharedToSinaWeiboGetShareReward")
        && !env.options.network_access
    {
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        let msg = game_localized_string(env, "SINAWEIBO_NO_CONNECT");
        if show_game_message_box(env, msg, 6, nil, SEL::null()) {
            log!("[MOLECHEAT] 离线:微博分享需要联网 → 照原版微信分支弹「连接不上互联网」提示,不进 ShareKit 授权页({sel})");
            env.cpu.regs_mut()[0] = 0;
            return true;
        }
        // 弹框没发出去:恢复寄存器走原版(不静默吞)。
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
    }

    // [扫描修 2026-09-15] F12-10 「左左右右」(沙滩WC,game_id 8,-[MiniGameManager enterMiniGame:stage:] tbb 第 8 路)靠
    //   IFAccelerometer 重力感应左右移动指挥官,ccTouchBegan: 只处理暂停/退出,触摸移动不了;touchHLE 桌面端只能
    //   "按住鼠标右键拖动"或手柄左摇杆模拟倾斜,原来只在日志里提示,玩家以为不能操作。
    //   做法:选关层 -[WashRoomLevelChoose startGame]@0x35dbfc(菜单按钮回调,不在帧栈上)本进程第一次被点时,
    //   弹游戏自带 MessageBox(type 8 = 带回调的确认按钮 buttonok1 → onButtonOK1: → [target performSelector:selector],
    //   同 -[DailySignLayer checkNetWork]@0x39a4ac 的用法),回调就是 startGame 本身 → 玩家点「确定」后照常开局;
    //   万一回调没触发,再点一次开始也会放行(标志已置)。只在桌面端弹:Android/iOS 有真传感器,提示反而误导。
    if class == "WashRoomLevelChoose"
        && sel == "startGame"
        && cfg!(not(any(target_os = "android", target_os = "ios")))
        && !WASHROOM_HINT_SHOWN.swap(true, O)
    {
        let this: id = Ptr::from_bits(env.cpu.regs()[0]);
        let saved = [
            env.cpu.regs()[0],
            env.cpu.regs()[1],
            env.cpu.regs()[2],
            env.cpu.regs()[3],
        ];
        let start_sel = island_sel(env, "startGame");
        let msg = crate::frameworks::foundation::ns_string::get_static_str(
            env,
            "「左左右右」靠重力感应左右移动:电脑上请按住鼠标右键拖动,或用手柄左摇杆倾斜。点「确定」开始游戏。",
        );
        log!("[MOLECHEAT] 左左右右(沙滩WC)操作提示:用右键拖拽或手柄左摇杆倾斜(首次开始时弹一次)");
        if show_game_message_box(env, msg, 8, this, start_sel) {
            env.cpu.regs_mut()[0] = 0;
            return true; // 等玩家点「确定」由 MessageBox 回调 startGame 开局
        }
        // 弹框失败(MessageBox 类缺失等):恢复寄存器,照常开局。
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
    }

    // ===== ONLINE MODE:登录通行证绕过 + 米米号注入(全 gate 在 online_login_mimi) =====
    // 离线(默认)每条分支都是空过,单机路径逐字节不变。仅 --allow-network-access + MOLE_MIMI 时生效。
    if let Some(mimi) = online_login_mimi(env) {
        // 捕获真正的 MainMenuScene 实例(runningScene 只是 CCScene 壳,菜单层在其子节点)。
        if class == "MainMenuScene" {
            let s = env.cpu.regs()[0];
            if s != 0 {
                MAINMENU_SCENE.store(s, O);
            }
        }
        // [扫描修 2026-09-15] F11-1 主菜单「切换账号」-[MainMenuScene onButtonChangeIDSelected:]@0xb523c。
        //   原版分支:isConnected 且 GameData.userInfoData.userId!=0(0xb5342)→ byte_B409B0=0 + setDelegateLoginMainMenu:
        //   + MBProgressHUD,state==4 时 loginWith...InSendType:3,否则 getLocalUserAndMapInfo(重拉 1001→compare);
        //   只有 userId==0 且 nextStorySectionId<=1 才走 showLoginView@0xb6140(→ reconnectUsingNewHD + setGameId:
        //   + [TMALoginViewController showAccountManagerViewWithDelegate:andUserID:])。在线合成登录后 userId 恒为米米号,
        //   所以这个按钮永远到不了账号菜单。
        //   · 账号菜单模式(MOLE_ACCOUNT_MENU)且登录包已发出:守卫(isEnable/isClickingMenu)照原版判;通过则吞掉原方法,
        //     照原版 0xb52ba 先把 isEnable 清 0 挡连点,锁存场景指针;下一帧在 drawScene 寄存器恢复安全区发原版
        //     showLoginView(它自带重连 + 弹账号菜单,之后 MENU_ACTIVE → passport 代理 → P3 抓 user_id 的现成链路照常接上)。
        //     不在这里内联派发:showLoginView 会断开重连并弹 UIKit 视图,嵌在菜单触摸派发栈里有重入卡死风险。
        //   · 默认模式(账号来自启动器 MOLE_MIMI):本进程第一次点时弹游戏自带 MessageBox 说明「账号由启动器决定」,
        //     type 8 的回调就是 onButtonChangeIDSelected: 本身(原方法不读 sender,r2 任意),玩家点「确定」后照原版继续
        //     (重拉存档语义不变),不改账号;之后再点直接走原版。按钮回调不在 drawScene 帧栈上,同 F12-10 的用法。
        //   · 登录包发出前 / 守卫不通过 / 槽值异常:一律放行原方法(= 修前行为)。
        if class == "MainMenuScene" && sel == "onButtonChangeIDSelected:" && LOGIN_PKT_SENT.load(O)
        {
            let scene = env.cpu.regs()[0];
            if let Some(enable_ptr) = mainmenu_change_id_guard(env, scene) {
                if account_menu_mode() {
                    env.mem.write(enable_ptr, 0u8);
                    PENDING_SHOW_LOGIN.store(scene, O);
                    log!("[MOLECHEAT] 账号菜单模式:主菜单点「切换账号」→ 吞掉原方法,下一帧改派原版 showLoginView(弹账号管理菜单)");
                    env.cpu.regs_mut()[0] = 0;
                    return true;
                } else if !CHANGEID_HINT_SHOWN.swap(true, O) {
                    let saved = [
                        env.cpu.regs()[0],
                        env.cpu.regs()[1],
                        env.cpu.regs()[2],
                        env.cpu.regs()[3],
                    ];
                    let this: id = Ptr::from_bits(scene);
                    let cb = island_sel(env, "onButtonChangeIDSelected:");
                    let msg = crate::frameworks::foundation::ns_string::get_static_str(
                        env,
                        "账号由启动器决定:游戏内不能切换账号。换号请在启动器里修改米米号(MOLE_MIMI)和密码(MOLE_PASSWORD)后重启游戏。点「确定」按原版重新同步存档。",
                    );
                    log!("[MOLECHEAT] 在线:主菜单点「切换账号」→ 提示账号由启动器环境变量决定(本进程只提示一次;要游戏内换号请设 MOLE_ACCOUNT_MENU=1)");
                    if show_game_message_box(env, msg, 8, this, cb) {
                        env.cpu.regs_mut()[0] = 0;
                        return true; // 等玩家点「确定」由 MessageBox 回调 onButtonChangeIDSelected: 走原版
                    }
                    // 弹框没发出去:恢复寄存器,照原版执行。
                    env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
                }
            }
        }
        // [扫描修 2026-09-15] F11-4 登录回包账号校验失败的可见提示(仅默认在线模式)。
        //   -[MainMenuScene onLoginMainMenuCommandReceived:]@0xb6958:r0=self,r2=回包头(0xb696c mov r5,r2;0xb6974 [r5 errorID]
        //   → -[MVPacketHeader errorID]@0x12444c 纯 ivar 取值;commandID@0x1243ec 同理)。私服密码不符回 cmd 1234 + errorID 112
        //   + sendFlag 1234(私服登录处理的密码校验分支)→ 原版 0xb6a14 落到 0xb7024:resetTaomeeUserInfoData,
        //   0xb7480 cmp #0x70 → UIAlertView「LOGIN_ID_PASSWORD_INCORRECT」。touchHLE 的 UIAlertView 立即按索引 0 自动关闭(F11-6),
        //   alertView:didDismissWithButtonIndex:@0xb7db0 断线时还会 setState:2+establishConnection 重连重登,玩家只看到卡在标题。
        //   这里只读 errorID/commandID(游戏自己的取值器,不改任何参数与返回),命中就锁存标志,恢复 r0-r3 后放行原方法;
        //   提示由 drawScene 安全区用游戏自带 MessageBox 弹,每进程一次。日志与提示都不含任何账号凭据内容。
        //   账号菜单模式不弹:那里账号来自 passport 菜单,原版错误链(showLoginView / loginForUser:withDelegate:)才是忠实入口。
        if class == "MainMenuScene"
            && sel == "onLoginMainMenuCommandReceived:"
            && !account_menu_mode()
            && !AUTH_FAIL_HINT_SHOWN.load(O)
        {
            let saved = [
                env.cpu.regs()[0],
                env.cpu.regs()[1],
                env.cpu.regs()[2],
                env.cpu.regs()[3],
            ];
            let hdr: id = Ptr::from_bits(saved[2]);
            if hdr != nil {
                // 方法类型:errorID / commandID 都是 L(unsigned long)getter,返回值按寄存器原样取 u32。
                let err_s = island_sel(env, "errorID");
                let err: u32 = msg_send(env, (hdr, err_s));
                if err == 112 {
                    let cmd_s = island_sel(env, "commandID");
                    let cmd: u32 = msg_send(env, (hdr, cmd_s));
                    if cmd == 1234 && !AUTH_FAIL_HINT_PENDING.swap(true, O) {
                        log!("[MOLECHEAT] 在线:登录回包 errorID=112(账号校验失败)→ 下一帧弹提示,请检查启动器里的米米号/密码配置");
                    }
                }
            }
            env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
            // 落到下面:返回 false,原方法照常处理(本钩子只读)。
        }
        // (0) Serverlist 注入:游戏向 mlogin.61.com/ipsvr.fcgi 发 ASIHTTPRequest 取 JSON(CFHTTP
        // touchHLE 没实现=死路)。直接注入私服、复用游戏 parseData:,跳过死 HTTP,放行后不跑真方法。
        if class == "TaomeeGetServerIpListManager"
            && sel == "getServerListWithServiceName:andDelegate:"
        {
            let manager: id = Ptr::from_bits(env.cpu.regs()[0]);
            let delegate: id = Ptr::from_bits(env.cpu.regs()[3]);
            inject_serverlist(env, manager, delegate);
            return true; // handled; skip the dead real HTTP fetch
        }
        // AsyncSocket.setSocketFromStreamsAndReturnError: pulls the native socket fd via
        // CFReadStreamCopyProperty(kCFStreamPropertySocketNativeHandle), which touchHLE doesn't
        // implement → it returns null and AsyncSocket would closeWithError (or crash) so the
        // connection never reaches didConnect. We don't need the native socket — read/write go
        // through the CFStreams — so force success (BOOL YES) and skip the real method; then
        // doStreamOpen proceeds to onSocket:didConnectToHost: (state=4). connectedHost/connectedPort
        // are nil-safe (return nil/0) when theSocket4/6 stay unset.
        if class == "AsyncSocket" && sel == "setSocketFromStreamsAndReturnError:" {
            env.cpu.regs_mut()[0] = 1; // BOOL YES
            return true;
        }
        // (1) 强制 wire 米米号:MVPacketHeader setUserID: 的入参在 R2,改写后放行真 setter
        //     (覆盖所有 sendType,含 onStateChangedTo:4 走 sendType3 读本地 userId 的路径)。
        if LOGIN_ARMED.load(O) && class == "MVPacketHeader" && sel == "setUserID:" {
            // 账号菜单模式用真正登录的米米号(passport 回的 user_id),默认模式仍用 MOLE_MIMI。
            env.cpu.regs_mut()[2] = if account_menu_mode() {
                LOGIN_MIMI.load(O)
            } else {
                mimi
            };
            // 落到下面:返回 false,真 setUserID: 用我们的值
        }
        // (2) 登录密码 MD5 块的明文来源:taomeePassword getter 返回 MOLE_PASSWORD。
        //     未设则不拦(空哈希,宽松服务器接受)。
        if LOGIN_ARMED.load(O) && class == "TaomeeUserInfo" && sel == "taomeePassword" {
            if let Ok(p) = std::env::var("MOLE_PASSWORD") {
                let ns = crate::frameworks::foundation::ns_string::from_rust_string(env, p);
                // [扫描修 2026-09-15] F10-7 getter 返回值按 Cocoa 约定是 autoreleased(游戏不会 release 它),
                //   以前直接返回 from_rust_string 的 +1 → 每次读密码泄漏一个串。
                let ns = autorelease(env, ns);
                env.cpu.regs_mut()[0] = ns.to_bits();
                return true;
            }
        }
        // (G1) Gate A(onButtonChangeIDSelected:)+ Gate C(onTaomeeLoginViewDidUnload:)。
        if LOGIN_ARMED.load(O) && class == "NetworkManager" && sel == "isReachable" {
            env.cpu.regs_mut()[0] = 1;
            return true;
        }
        // (G2) Gate B(showAccountManagerViewWithDelegate:)。
        if LOGIN_ARMED.load(O) && class == "TMA_ASIHTTPRequest" && sel == "isNetworkReachable" {
            env.cpu.regs_mut()[0] = 1;
            return true;
        }
        // (G3) 吞掉死掉的淘米通行证 HTTP(sendRequest:1012),改为 arm 延迟合成。
        // ★账号菜单模式:不吞,放原版发真 passport 1012,让账号管理菜单 UI 走真流程渲染出来。
        if class == "TMADataManager" && sel == "autoLoginWithUserID:" {
            if account_menu_mode() {
                return false;
            }
            if LOGIN_ARMED.load(O) {
                LOGIN_MIMI.store(mimi, O);
                LOGIN_PWD.with(|c| *c.borrow_mut() = std::env::var("MOLE_PASSWORD").ok());
                if !LOGIN_ARMED.swap(true, O) {
                    log!(
                        "[MOLECHEAT] 在线:拦截 autoLoginWithUserID:,改为合成登录成功 米米号={}",
                        mimi
                    );
                }
                return true;
            }
        }
        // ===== 账号菜单模式 passport 代理(让 touchHLE 也弹原版账号菜单)=====
        if account_menu_mode() {
            // 玩家点"切换账号"= showAccountManagerViewWithDelegate:andUserID:,激活 passport 代理。
            // 只代理这之后的 passport;之前进村自动发的 autoLogin 不碰(它走会崩的静默登录分支)。
            if sel == "showAccountManagerViewWithDelegate:andUserID:" {
                if !MENU_ACTIVE.swap(true, O) {
                    log!("[MOLECHEAT] ★切换账号入口,激活 passport 代理");
                }
            }
            // (P0) 抓 TMAHttpManager sendRequest: 的命令字(reqID),紧接着的 addOperation: 代理时据此构造 body。
            if class == "TMAHttpManager" && sel == "sendRequest:" {
                PENDING_REQID.store(env.cpu.regs()[2], O);
            }
            // (K) keychain 桩:TMA_SSKeychain 被 touchHLE fake 成 nil,登录成功路径拿 allAccounts(nil)
            //     当指针解引用 → null-page 崩。至少让 allAccounts 回【空数组】(非 nil)。
            //     MOLE_REAL_KEYCHAIN=1 时类是真的(classes.rs 不 fake),交给原版实现。
            if class == "TMA_SSKeychain"
                && sel == "allAccounts"
                && std::env::var("MOLE_REAL_KEYCHAIN").as_deref() != Ok("1")
            {
                let arr = crate::frameworks::foundation::ns_array::from_vec(env, vec![]);
                let arr = autorelease(env, arr);
                env.cpu.regs_mut()[0] = arr.to_bits();
                return true;
            }
            // (J) ★绕开 touchHLE 没实现的 JSONKit(JKDictionary/JKArray 是 unimplemented class → 解析 nil → 崩):
            //     拦 TMAHttpManager getDictionaryWithJsonData:,自己在 Rust 解析 passport 响应 JSON
            //     构造【标准 NSDictionary】喂回,客户端 requestFinish: 照常 objectForKey: 取 status_code/extra_data 分发。
            if class == "TMAHttpManager" && sel == "getDictionaryWithJsonData:" {
                // [2026-09-16] F1-04 nsdata_to_bytes 发了 length/bytes 两次宿主 msg_send(返回后 r0-r3 是被调方留下的值);
                //   空键路径要放行真方法,先快照、落空前恢复。真方法@0x4aeb8c 眼下只用 r2,不恢复也侥幸无害,但不能靠侥幸。
                let saved = [
                    env.cpu.regs()[0],
                    env.cpu.regs()[1],
                    env.cpu.regs()[2],
                    env.cpu.regs()[3],
                ];
                let data: id = Ptr::from_bits(saved[2]);
                let bytes = nsdata_to_bytes(env, data);
                let pairs = parse_flat_json(&bytes);
                if !pairs.is_empty() {
                    let dict = build_nsdict(env, &pairs);
                    log!(
                        "[MOLECHEAT] getDictionaryWithJsonData: 绕 JSONKit → Rust 构造 NSDictionary({} 键)",
                        pairs.len()
                    );
                    env.cpu.regs_mut()[0] = dict.to_bits();
                    return true;
                }
                env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
            }
            // (P1) 拦 TMA_ASINetworkQueue addOperation:(passport 真正的发送动作),代理到私服 shim。
            //      只在切换账号激活后代理(避免碰进村自动 autoLogin 的静默登录崩溃分支)。
            if MENU_ACTIVE.load(O) && class == "TMA_ASINetworkQueue" && sel == "addOperation:" {
                // [2026-09-16] F1-04 passport_proxy_enqueue 先经 asi_request_url 发 url/absoluteString 两次宿主 msg_send;
                //   非 passport URL 或没抓到 reqID 时返回 false、要放行真 addOperation:(@0x4d6ac4 开头 mov r5,r0 取 self),
                //   不恢复就会拿 NSString 当 self 跑。它内部的每个 return false 都由这里统一恢复 r0-r3。
                let saved = [
                    env.cpu.regs()[0],
                    env.cpu.regs()[1],
                    env.cpu.regs()[2],
                    env.cpu.regs()[3],
                ];
                let req: id = Ptr::from_bits(saved[2]);
                if passport_proxy_enqueue(env, req) {
                    return true;
                }
                env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
            }
            // (P2) 回灌:原版 requestFinish: 读 [request responseData] 时,把代理拿到的 JSON 喂回去。
            if sel == "responseData"
                && (class == "TMA_ASIFormDataRequest" || class == "TMA_ASIHTTPRequest")
            {
                let req_bits = env.cpu.regs()[0] as u32;
                let bytes = {
                    let resp = PASSPORT_RESP.lock().unwrap();
                    resp.iter()
                        .find(|(b, _)| *b == req_bits)
                        .map(|(_, v)| v.clone())
                };
                if let Some(bytes) = bytes {
                    let data = crate::frameworks::foundation::ns_url_connection::nsdata_from_bytes(
                        env, &bytes,
                    );
                    env.cpu.regs_mut()[0] = data.to_bits();
                    return true;
                }
            }
            // (P3) passport 登录成功后原版回调 onTaomeeLoginViewDidUnload...,捕获 user_id 武装 TCP 登录链
            //      (setUserID/taomeePassword/isReachable 等门 gate 在 LOGIN_ARMED),让换号后能真连 TCP 1234。
            if class == "MainMenuScene"
                && sel == "onTaomeeLoginViewDidUnloadWithUserID:password:returnCode:"
            {
                let uid = env.cpu.regs()[2];
                if uid != 0 {
                    LOGIN_MIMI.store(uid, O);
                    LOGIN_PWD.with(|c| *c.borrow_mut() = std::env::var("MOLE_PASSWORD").ok());
                    if !LOGIN_ARMED.swap(true, O) {
                        log!(
                            "[MOLECHEAT] 账号菜单模式:passport 登录成功 user_id={},武装 TCP 登录链",
                            uid
                        );
                    }
                }
                // 放行真回调(establishConnection -> serverlist -> TCP)。
            }
        }
        // establishConnection 开头 `if(self->isReachable_)` 读的是 IVAR(G1 只改了方法),
        // 进入前先 [self setIsReachable:YES] 置 ivar,否则直接 bail 不连。放行真方法。
        if LOGIN_ARMED.load(O) && class == "NetworkManager" && sel == "establishConnection" {
            // [2026-09-16] F1-04 setIsReachable: 是宿主 msg_send,返回后 r0-r3 是被调方留下的值。现在只因
            //   -[NetworkManager setIsReachable:]@0xed30c 恰好是 `strb r2,[r0,r1]; bx lr` 才保住 r0(r1 已变成 180),
            //   真方法@0xe104c 开头 mov r8,r0 取 self。放行前恢复快照,不靠被调方的实现细节。
            let saved = [
                env.cpu.regs()[0],
                env.cpu.regs()[1],
                env.cpu.regs()[2],
                env.cpu.regs()[3],
            ];
            let nm: id = Ptr::from_bits(saved[0]);
            let set = env
                .objc
                .register_host_selector("setIsReachable:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (nm, set, true));
            env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
            // 落到下面 -> 返回 false,真 establishConnection 用 isReachable_=1 运行
        }
        // HUD 统计:state 6 = 发了一个包,state 7 = 解析了一个包。一律 pass-through ——
        // 尤其 state 8(伪 "Error connecting" 断开):实测它是 connect-retry 流程一环,抑制会让
        // 连接建不起来;真正的进村卡点在下游(LoadingLayer update:/loadTarget 不复触发)。
        if class == "NetworkManager" && sel == "changeStateTo:withMessage:" {
            let state = env.cpu.regs()[2] as i32;
            if state == 6 {
                PKTS_SENT.fetch_add(1, O);
                LAST_SEND_AT.with(|c| c.set(Some(std::time::Instant::now())));
            } else if state == 7 {
                PKTS_RECV.fetch_add(1, O);
                STATE_IS_7.store(true, O); // connection is up → safe to start the HUD tick
                LAST_SEND_AT.with(|c| {
                    if let Some(t) = c.get() {
                        LAST_RTT_MS.store(t.elapsed().as_millis() as u32, O);
                    }
                });
            }
            return false;
        }
        // ★ 15s 断连根治(走原版 play-login 语义)。passport 回调以 sendType 3 发登录(1234)→
        // loginWith...InSendType: 末尾 switch 把 sendType 3 映射成 sendFlag=1000;但客户端把发出的命令
        // 按 sendFlag 当 key 存进 UnreadPacketsDic_(sendPacket:commandId:),回包按 sendFlag 移除。
        // 服务端登录回包用 sendFlag=1234(原版语义:onLoginMainMenuCommandReceived 据此置 byte_B409B0
        // 进村)→ 对不上 key "1000" → 清不掉 → checkTimeOut@15s 超时 → disconnect → 重连 churn →
        // socket 回调狂刷饿死 run-loop → 画面冻结。原版 play-login 本就是 sendType 1(switch:1→
        // sendFlag 1234),与 3 的唯一实际差别就是 sendFlag(userID/密码都回落到 taomeeUserID+
        // taomeePassword,mole_cheats 已设)。把 3 改成 1 → 请求 sendFlag=1234 → 回包自然匹配清超时
        // + 置 byte_B409B0 → 进村。服务端一行不改,纯把客户端登录摆回原版姿势。
        if class == "NetworkManager" && sel == "loginWithDeviceInfoAndUserIDInfoInSendType:" {
            if env.cpu.regs()[2] == 3 {
                env.cpu.regs_mut()[2] = 1;
                log!("[MOLECHEAT] 在线:登录 sendType 3→1(原版 play-login,请求 sendFlag=1234,根治 15s 超时断连)");
            }
            return false; // 用改过的 sendType 跑真 loginWith...
        }
        // ★ Spurious-disconnect root cause (empirically pinned via the changeStateTo:8 caller-LR =
        // 0xebc60 = -[NetworkManager onServerListResult:], message "Error connecting to server"):
        // the game's ORIGINAL flow fetches the server list over HTTP, but our private host serves only
        // the raw TCP game protocol (no HTTP list endpoint), so onServerListResult: is invoked with
        // success=NO → it falls straight through to changeStateTo:8 "Error connecting to server" →
        // MainMenuScene goes back to the title (entermainmenu), derailing village loading. Our
        // synthetic passport flow already establishes the TCP link directly (establishConnection
        // cold-connect; 1234→1052→1001 all succeed regardless of this HTTP result), so this HTTP
        // server-list callback is redundant — skip it to kill the bogus disconnect. (Verified: with
        // the island hook OFF the state-8 still fired from here, and no -[NetworkManager disconnect]
        // was ever called, ruling out the OnLoginOk userId-guard / onSocketDidDisconnect: path.)
        // onServerListResult: is called BOTH with success=YES (a3!=0 → it connects to the
        // serverLinkInfoList; THIS is the live connection path — must NOT be skipped) and with
        // success=NO (a3==0 → the HTTP list fetch failed → falls through to changeStateTo:8 "Error
        // connecting to server" → entermainmenu → derails the village). So skip ONLY the a3==0 call
        // (suppress the bogus disconnect) and let the a3!=0 call run normally (keep the connection).
        // onServerListResult:(success) is -[HttpManager callDelegateServerList]'s callback with
        // success = HttpManager.result_ (the HTTP server-list fetch result). Our private host serves
        // only the raw TCP game protocol (no HTTP list endpoint), so result_ == NO → onServerListResult:
        // falls through to changeStateTo:8 "Error connecting to server" → entermainmenu → derails the
        // village. FAITHFUL fix: force success = YES so it takes the connect path instead — if already
        // connected (our establishConnection cold-connect) it just returns; otherwise it connects to
        // the injected serverLinkInfoList. Either way: no bogus disconnect, and the real flow proceeds.
        if sel == "onServerListResult:" {
            return false;
        }
        // Diagnose the village render: -[LoadingLayer update:] (scheduled by showWithTarget:) is what
        // schedules loadTarget on the main thread. If it never fires after showWithTarget:4, the village
        // scene (case 4 → loadFromLocal + startGame) is never built.
        if class == "LoadingLayer" && sel == "update:" {
            // Natural update: fired → loadTarget will run via the perform queue; cancel our fallback.
            PENDING_LOADTARGET.store(0, O);
            return false;
        }
        if sel == "showWithTarget:" {
            let tgt = env.cpu.regs()[2] as i32;
            // Latch the village transition (target 4) so the drawScene tick can drive loadTarget if the
            // LoadingLayer's natural update: never re-fires (see PENDING_LOADTARGET).
            if tgt == 4 {
                PENDING_LOADTARGET.store(env.cpu.regs()[0], O);
                PENDING_LOADTARGET_FRAMES.store(0, O);
            }
            return false;
        }
        // The 1s HUD tick (fired by performSelector:afterDelay: in the run-loop perform phase, NOT
        // the drawScene frame stack). Refresh the overlay, then reschedule the next tick. GameManager
        // doesn't implement moleHudTick — we intercept it before the real (no-op) dispatch.
        if sel == "moleHudTick" {
            update_debug_hud(env, LOGIN_MIMI.load(O));
            schedule_hud_tick(env);
            return true;
        }
        // 在线自动登录:启动若干帧后自动 arm(无需点 Play;离线/未设 MOLE_MIMI 永不到这)。
        // 然后在同一安全帧边界(drawScene/mainLoop)一次性 fire 合成登录,绝不内联派发。
        if sel == "drawScene" || sel == "mainLoop" {
            // ★ Save self/sel. Everything below (fire_online_login, the loadTarget drive, the 8×
            // drive_streams drain) does host msg_sends that clobber r0-r3. We return false so the REAL
            // -[CCDirectorIOS drawScene] runs next, and touchHLE dispatches it with the POST-hook
            // registers — a clobbered r0 = wrong director self → it reads nextScene_ off the wrong
            // object (nil) and never calls setNextScene → scene transitions silently stop after our flow
            // engages (exactly the symptom: nextScene_=InGameScene set in memory but never applied). So
            // restore r0/r1 before falling through. (drawScene/mainLoop take no further args.)
            let saved_r0 = env.cpu.regs()[0];
            let saved_r1 = env.cpu.regs()[1];
            // 账号菜单模式也保留自动合成登录(进村),玩家在游戏里点"切换账号"时 G3 放行真 passport 弹菜单
            //(主菜单的摩尔标志是 placeholder 没登录入口,停标题反而点不动;走熟悉的进村→切换账号流程)。
            if !LOGIN_ARMED.load(O) && !LOGIN_FIRED.load(O) {
                let n = LOGIN_BOOT_FRAMES.fetch_add(1, O);
                if n >= 180 && !LOGIN_ARMED.swap(true, O) {
                    LOGIN_MIMI.store(mimi, O);
                    LOGIN_PWD.with(|c| *c.borrow_mut() = std::env::var("MOLE_PASSWORD").ok());
                    // 在线模式开启庄园持久化补丁(NOP saveMapData 第4道闸),让活图能整包上传。
                    MAP_SYNC_PATCH.store(true, O);
                    CRACK_PATCHES_DIRTY.store(true, O);
                    log!("[MOLECHEAT] 在线:启动后自动登录 米米号={}(开启 MapSync 持久化补丁)", mimi);
                }
            }
            // Once armed, drive the native passport login: phase 1 (cold connect) then phase 2
            // (send login at state 4). fire_online_login latches both via LOGIN_FIRED/LOGIN_PKT_SENT.
            if LOGIN_ARMED.load(O) && !LOGIN_PKT_SENT.load(O) {
                fire_online_login(env);
            }
            // Village-render fallback (see PENDING_LOADTARGET): showWithTarget:4 latched a LoadingLayer,
            // but in touchHLE its update: doesn't re-fire so loadTarget(case 4) never builds the village.
            // After a short grace (so a natural update: can cancel us), drive loadTarget ourselves.
            {
                let pend = PENDING_LOADTARGET.load(O);
                if pend != 0 && PENDING_LOADTARGET_FRAMES.fetch_add(1, O) >= 6 {
                    PENDING_LOADTARGET.store(0, O);
                    let ll: id = Ptr::from_bits(pend);
                    let lt = env
                        .objc
                        .register_host_selector("loadTarget".to_string(), &mut env.mem);
                    // Queue loadTarget on the main run loop EXACTLY as -[LoadingLayer update:] would
                    // (performSelectorOnMainThread:), so the replaceScene: it triggers is applied by the
                    // director in its normal scene-switch phase rather than inline in this drawScene.
                    let psomt = env.objc.register_host_selector(
                        "performSelectorOnMainThread:withObject:waitUntilDone:".to_string(),
                        &mut env.mem,
                    );
                    let _: () = msg_send(env, (ll, psomt, lt, nil, false));
                    log!("[MOLECHEAT] 在线:★原生 update: 未复活→手动 performSelectorOnMainThread:loadTarget(渲染村庄 case4)");
                }
            }
            // FLAKY FIX (aggressive stream drain) — RE-confirmed root cause: -[AsyncSocket
            // doBytesAvailable] completes only ONE queued read per HasBytes, and a packet is read in
            // stages (a 24B header read, THEN a body read; each reply is 2+ reads). The game's
            // CADisplayLink frame loop doesn't pump the run-loop's CFStream callbacks reliably, so a
            // single pump/frame routinely leaves the login reply header-read-but-body-pending → state
            // stuck at 4 → sendPacket re-login spam → watchdog drop (the intermittent never-reaches-7).
            // Fix: while online, drain the socket SEVERAL times every frame. drive_streams peeks+reads
            // and runs the same stream callbacks the run loop would (cheap no-op when nothing buffered),
            // so header+body+the whole 1234/1052/1001 sequence + ongoing traffic all drain promptly.
            // Continuous (not state-gated, no msg_send) — drive_streams is host-side, never re-enters a
            // scene swap (the village transition is deferred to the next frame via showWithTarget:).
            if LOGIN_FIRED.load(O) || account_menu_mode() {
                for _ in 0..8 {
                    crate::frameworks::core_foundation::cf_stream::drive_streams(env);
                }
            }
            // 账号菜单模式:每帧把后台 HTTP 拿到的 passport 响应回灌原版(在 saved_r0/r1 恢复区内,msg_send 安全)。
            if account_menu_mode() {
                drive_passport(env);
            }
            // Debug HUD: do NOT refresh it from this drawScene frame stack (that starved the
            // run-loop during the connect window and killed the Open event). Instead, ONCE the
            // connection reached state 7, kick off a 1s self-rescheduling tick (performSelector:
            // afterDelay:) that refreshes the HUD entirely in the run-loop perform phase. Gated on
            // STATE_IS_7 so nothing fires during state 4/6 (the疯狂发包 connect window).
            // [扫描修 2026-09-15] F10-8 HUD 出厂关:以前这里 unwrap_or(true) 默认开;改读 hud_enabled()(MOLE_HUD 非 "0" 才开,只解析一次)。
            if LOGIN_FIRED.load(O)
                && STATE_IS_7.load(O)
                && !HUD_TIMER_SET.load(O)
                && hud_enabled()
            {
                HUD_TIMER_SET.store(true, O);
                schedule_hud_tick(env);
            }
            // 曾在此直接调 getLocalUserAndMapInfo 并强写 byte_B409B0,因是绕过原版 1234 回包处理的捷径而删除,勿复活。
            // ★ 庄园地图持久化(修法甲):进村稳定后(STATE_IS_7)host 主动把活图整包发上来。主庄园持久化
            // 唯一上行=updateInfoToServer 追加的 gzip map blob(非 1059 增量=黄金岛机制)。原版自发上传被
            // saveMapData: 的 5 道闸卡死(touchHLE 活图状态不满足)→ map 恒 0B。host 先调已验证可用的无参
            // saveMapData 把活图写进 mapdata_,再 updateInfoToServer(内部 encodeLocalMapData 见 mapdata_
            // 非空→编 blob→发)。服务端 Stage A 已就位存 map_blob、1001 回吐。频率 once/~30s 不每帧探测。
            if LOGIN_PKT_SENT.load(O) && STATE_IS_7.load(O) {
                let n = MAP_UPLOAD_FRAMES.fetch_add(1, O);
                if n == 600 || (n > 600 && (n - 600) % 1800 == 0) {
                    let shared = env
                        .objc
                        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
                    let gd: id = msg_send(env, (gd_cls, shared));
                    if gd != nil {
                        // 把活图写进 mapdata_:无参 saveMapData→saveMapData:0。MapSync 补丁已 NOP 掉第4道闸
                        // (m_isLoadMap!=0→bail),前3道(currentGameMode/curSceneId)+第5道(objects≥14)本就过,
                        // 故 saveMapData 把 ObjectManager 活图序列化进 mapdata_(满村 count=42)。
                        let save = env
                            .objc
                            .register_host_selector("saveMapData".to_string(), &mut env.mem);
                        let _: () = msg_send(env, (gd, save));
                        // 发整图上传 1019:updateInfoToServer→encodeLocalMapData→gzipDeflate(已补 deflate 压缩族)
                        // →gzip blob→sendPacket。服务端 Stage A 存 user_info.map_blob,下次登录 1001 回吐→持久化闭环。
                        let nm_cls = env.objc.get_known_class("NetworkManager", &mut env.mem);
                        let nm: id = msg_send(env, (nm_cls, shared));
                        if nm != nil {
                            let upd = env.objc.register_host_selector(
                                "updateInfoToServer".to_string(),
                                &mut env.mem,
                            );
                            let _: () = msg_send(env, (nm, upd));
                        }
                        // [扫描修 2026-09-15] F10-6 每 ~30s 一次的周期上传:首次 log!(证明链路在跑),之后 log_dbg!。
                        log_first_then_dbg!(
                            LOG1_MAP_UPLOAD,
                            "[MOLECHEAT] 在线:庄园地图持久化上传(saveMapData+updateInfoToServer,帧{})",
                            n
                        );
                    }
                }
            }
            // [扫描修 2026-09-15] F11-1 消费「切换账号」锁存(见 onButtonChangeIDSelected: 钩子):在本安全区对主菜单场景发原版
            //   showLoginView。先照原版按钮回调开头补点击音效 [[GameSoundManager sharedManager] playSound:37](0xb5260-0xb5272,
            //   方法类型串 i12@0:4i8)。锁存的指针与最近一次捕获的 MainMenuScene 不一致(期间换了场景)就丢弃,不对旧指针发消息。
            {
                let pend = PENDING_SHOW_LOGIN.swap(0, O);
                if pend != 0 {
                    let scene: id = Ptr::from_bits(pend);
                    if MAINMENU_SCENE.load(O) == pend
                        && env
                            .objc
                            .object_has_method_named(&env.mem, scene, "showLoginView")
                    {
                        let gsm_cls = env.objc.get_known_class("GameSoundManager", &mut env.mem);
                        if gsm_cls != nil {
                            let sm_s = island_sel(env, "sharedManager");
                            let gsm: id = msg_send(env, (gsm_cls, sm_s));
                            if gsm != nil {
                                let ps_s = island_sel(env, "playSound:");
                                let _: i32 = msg_send(env, (gsm, ps_s, 37i32));
                            }
                        }
                        let slv_s = island_sel(env, "showLoginView");
                        let _: () = msg_send(env, (scene, slv_s));
                        log!("[MOLECHEAT] 账号菜单模式:已对主菜单发原版 showLoginView(重连 + 弹账号管理菜单)");
                    } else {
                        log!("[MOLECHEAT] 账号菜单模式:「切换账号」锁存的主菜单场景已失效,放弃派发 showLoginView");
                    }
                }
            }
            // [扫描修 2026-09-15] F11-4 消费 112 提示锁存(见 onLoginMainMenuCommandReceived: 钩子)。MessageBox 挂在
            //   [[CCDirector sharedDirector] runningScene] 上(0xca9e6-0xcaa12),且已有父节点时新的 show 直接返回(0xca672);
            //   所以 runningScene 为空(切场景中)或已有 MessageBox 在屏上时留到之后的帧再弹,保证提示真的看得见。
            if AUTH_FAIL_HINT_PENDING.load(O) && !AUTH_FAIL_HINT_SHOWN.load(O) {
                let dir_cls = env.objc.get_known_class("CCDirector", &mut env.mem);
                let running: id = if dir_cls != nil {
                    let sd_s = island_sel(env, "sharedDirector");
                    let dir: id = msg_send(env, (dir_cls, sd_s));
                    if dir != nil {
                        let rs_s = island_sel(env, "runningScene");
                        msg_send(env, (dir, rs_s))
                    } else {
                        nil
                    }
                } else {
                    nil
                };
                let mb_cls = env.objc.get_known_class("MessageBox", &mut env.mem);
                let mb_busy = if mb_cls != nil {
                    let sh_s = island_sel(env, "sharedInstance");
                    let mb: id = msg_send(env, (mb_cls, sh_s));
                    if mb != nil {
                        let parent_s = island_sel(env, "parent");
                        let parent: id = msg_send(env, (mb, parent_s));
                        parent != nil
                    } else {
                        false
                    }
                } else {
                    false
                };
                if running != nil && !mb_busy {
                    AUTH_FAIL_HINT_PENDING.store(false, O);
                    AUTH_FAIL_HINT_SHOWN.store(true, O);
                    let msg = crate::frameworks::foundation::ns_string::get_static_str(
                        env,
                        "登录失败:服务器提示米米号或密码不正确(错误码 112)。请检查启动器里配置的米米号(MOLE_MIMI)和密码(MOLE_PASSWORD),改好后重新启动游戏。",
                    );
                    if show_game_message_box(env, msg, 6, nil, SEL::null()) {
                        log!("[MOLECHEAT] 在线:已弹「账号校验失败(112),请检查启动器账号配置」提示(本进程只弹一次)");
                    }
                }
            }
            // ★ Restore self/sel so the real drawScene/mainLoop runs on the correct director and its
            // `if(nextScene_) setNextScene` applies pending scene transitions (the village switch).
            env.cpu.regs_mut()[0] = saved_r0;
            env.cpu.regs_mut()[1] = saved_r1;
        }
    }

    // ===== 离线黄金岛(NewScene 可建筑岛,scene id 10)进岛打通 =====
    // 全部 hook 仅在 ENABLE_NEWSCENE_ISLAND 开时生效;网络门强制仅在进岛窗口内,
    // 不污染主村离线行为(铁律:别动已修好的东西)。从 host 嵌套调 guest 的操作只在
    // 运行时就绪后发生(drawScene / 进岛序列),避开启动早期 yielder=None 的坑。
    // ★[审查修 2026-09-11] 节拍兜底:处理臂在 ENABLE_NEWSCENE_ISLAND 块内,开关关着时排队的那一拍会被跳过、当 no-op 丢掉,
    //   闩锁卡在 true。这里吞掉并清闩锁(开关关着时不碰岛档,所以不落盘)。GameManager 不实现该选择子,必须 return true。
    if sel == "moleIslandTick" && !ENABLE_NEWSCENE_ISLAND.load(O) {
        ISLAND_TICK_RUNNING.store(false, O);
        return true;
    }

    // ★[审计修 2026-09-11·取证纠错] 离线时钟:拦 -[NewSceneTimer getCurrentServerTime](0x22f60c)直接返回宿主真实时间,
    //   且**必须是 CFAbsoluteTime(2001 纪元)而非 unix 秒**:原版 -[NetworkManager parseServerTime:pos:len:] 收到 1065 的
    //   u32 unix 秒后先减 kCFAbsoluteTimeIntervalSince1970(978307200)再存(0x226fea vsub.f64)。离线从没人调
    //   resetTimerWithLatestServerTime: → 基准恒 0 → 岛上计时(餐厅升级/公寓训练/出海/商店/打工任务)跨会话全错,
    //   主村 DailySignLayer 算出 2001-01-01、WaterTower isServerTimeCorrect(>394264064)恒假水塔不产水、RewardBox 冷却错乱。
    //   拦 getter 而不调原版 reset:reset 会取消再重新调度 timeCounterAdded(touchHLE 有"取消后重调度不复活"的前科),
    //   且后台/掉帧时计数器不走;三个 ivar 除 NewSceneTimer 自身外无人直读(xref 实证)。返回类型 L,88 个调用点均按无符号用。
    //   原 getter 在 isConnected(岛上被强制为真)时每次调用都发 1065,拦下后顺带消除。仅离线;在线走私服 1065 原版路径。
    if class == "NewSceneTimer" && sel == "getCurrentServerTime" && !env.options.network_access {
        env.cpu.regs_mut()[0] = now_cf_secs().max(0.0) as u32;
        return true;
    }

    if ENABLE_NEWSCENE_ISLAND.load(O) {
        // 每帧:递减进岛网络门窗口。(SUCC 回调不再在这里同步 fire——那会在 CADisplayLink
        // 帧定时器栈内同步 startNewSceneFrom→replaceScene→改 CCScheduler,触发 cocos2d
        // 重入 UB=整屏卡死。改由 gate#1 用 performSelector:afterDelay:0 异步排到 run loop
        // 的 perform 相位,在 director 退出 draw 的安全帧边界换场。)
        if sel == "drawScene" || sel == "mainLoop" {
            watchdog_frame(); // 推进看门狗帧计数(出帧=游戏还活着,没卡死)
            let w = ISLAND_ENTER_WINDOW.load(O);
            if w > 0 {
                ISLAND_ENTER_WINDOW.store(w - 1, O);
            }
            // ★绝不在此(CADisplayLink 帧定时器栈)做任何 msg_send / 同步 guest 调用——那正是
            // 进岛卡死(cocos2d scheduler 重入活锁)的病根。会话标志全部事件驱动(enterLoading/loadNewScene/
            // startNewSceneFrom 臂),这里只做【只读内存】的过渡完成判定:SceneMannager+12 = curSceneId_。
            if sel == "drawScene" {
                let mgr = ISLAND_SCENE_MGR.load(O);
                if ISLAND_EXITING.load(O) {
                    let left = ISLAND_EXIT_FRAMES.fetch_sub(1, O);
                    let cur: i32 = if mgr != 0 {
                        let slot: ConstPtr<i32> = Ptr::from_bits(mgr + 12);
                        env.mem.read(slot)
                    } else {
                        -1
                    };
                    if cur == 1 {
                        ISLAND_EXITING.store(false, O);
                        log!("[MOLECHEAT] island: 离岛完成(curSceneId=1)");
                    } else if cur == 10 {
                        ISLAND_EXITING.store(false, O);
                        log!("[MOLECHEAT] island: 离岛过渡异常:curSceneId 仍为 10 → 清离岛标志");
                    } else if left <= 0 {
                        ISLAND_EXITING.store(false, O);
                        log!("[MOLECHEAT] island: 离岛过渡超时(curSceneId={})→ 清离岛标志", cur);
                    }
                }
                if ISLAND_LOADING.load(O) && mgr != 0 {
                    let slot: ConstPtr<i32> = Ptr::from_bits(mgr + 12);
                    let cur: i32 = env.mem.read(slot);
                    if cur == 1 {
                        ISLAND_LOADING.store(false, O);
                        log!("[MOLECHEAT] island: 进岛加载未完成就回到主村(curSceneId=1)→ 清加载标志");
                    }
                }
            }
        }

        // 问题2-B:岛上断网弹框(HolidayVillageLayer)会被 touchHLE 自动按 index0=「返回庄园」
        // → didDismissWithButtonIndex:→returnToMainVillage 踢回村。直接吞掉这三个弹框方法,
        // 彻底消灭"踢"这个动作(不弹框→不自动dismiss→不回村)。配合 2-A 的网络门续期双保险。
        if class == "HolidayVillageLayer"
            && matches!(
                sel,
                "showNoNetConnectErrorMessage"
                    | "showNetConnectErrorMessageWithRetryButton"
                    | "showMultiLoginErrorMessageInNewScene"
            )
        {
            return true; // 吞掉弹框
        }

        // ★岛上点击建筑崩溃(null-page @0x1)根因 + 修复:
        // RestaurantView showWithTarget:(id)target selector:(SEL) 的真方法开头会
        // `[target isKindOfClass:某类]`。它前面虽有 `if(target==nil)return`,但岛上下文里
        // target 实测 = 0x1(不是 nil,绕过空检查),于是 [0x1 isKindOfClass:] 读 isa@0x1 → 崩。
        // (符号化实证:LR=0x2497eb=RestaurantView showWithTarget:selector: imp 0x249769,
        //  R1=0x88aca7="isKindOfClass:",R5=R0=0x1=target。)
        // 而最初的 issue-4 修复(在此顶 gameMode=1)经 workflow 实证=本崩的根因:顶 gameMode 会
        // 提前打开 HolidayVillageLayer.processTouch 触摸派发循环、命中未初始化哨兵槽 0x1。故 gameMode
        // 待机化已移到 HolidayVillageLayer.onEnter 延后顶(见下 onEnter hook);这里只保留硬兜底:
        // target 非零却不像指针(<0x1000)就吞掉整条 showWithTarget:(任意类,防别的建筑面板同样的崩),
        // 作为 0x1 的最后一道防线。寄存器:self=r0, _cmd=r1, target=r2, selector=r3。
        // [2026-09-16] F1-01 nil 不再算无效。岛 HUD 是 NewSceneUserInfoLayer(继承 UserInfoLayer 的按钮回调),点成就/兑换中心/
        //   VIP 功能发的是 [XxxLayer showWithTarget:nil selector:nil](-[UserInfoLayer onButtonAchieveSelected:]@0x59c90 在
        //   0x59e40 movs r2,#0;兑换中心 0x59f3e、VIP 功能 0x5a28a 同样传 nil)。原版各 show 方法都容忍 nil:AchieveSystemLayer@0x310044、
        //   VIPFunctionsLayer@0x37b83c、VIPLayer@0x37ef18、ExchangeCenterLayer@0x376d60;RestaurantView@0x249768 自己在
        //   0x2497ae 起判 nil 就 return。旧判据 `target < 0x1000` 连 0 一起吞 → 这些面板在岛上点了没反应。
        //   只改判据,仍对任意类生效、不按类名收窄(别的岛建筑面板是否会收到 0x1 没核实,收窄会让它们失去这道防线)。
        if ON_ISLAND.load(O) && sel == "showWithTarget:selector:" {
            let target = env.cpu.regs()[2];
            if target != 0 && target < 0x1000 {
                log_first_then_dbg!(
                    LOG1_ISLAND_BAD_TARGET,
                    "[MOLECHEAT] island: {} showWithTarget: 无效 target={:#x},吞掉防崩",
                    class,
                    target
                );
                return true; // 吞掉:不跑真方法 → 不会 [0x1 isKindOfClass:] → 不崩
            }
            // target 为 nil 或有效指针:放行真方法(nil 由原版自己处理;gameMode 门已由 LR 收窄 hook 放行,布兰的家正常弹面板)。
        }

        // ★Bug B 续(公寓雇用按了没真出摩尔):点雇用 NewSceneApartment 走 setCurrentProduceMoleNums:(old+1)
        // 设"在产数";真摩尔靠 createInterupdate 每秒计时器等满 build_time(~3600s)才 addWorker:→
        // initMoleActors: 出来,而计时器由 onInfoViewClosed 才 schedule(布兰的家面板 LR 硬开,关闭可能
        // 不走该回调)→ 永不出。改:hook 此 setter,雇用(new>old)时【立即】对 userInfoDataInNewScene
        // addWorker:(new-old)(实测 types v12@0:4i8=收 int,内含 initMoleActors: 出可见摩尔,无发包),
        // 再把在产数压回 old(改 r2 放行真 setter)避免每秒计时器到点二次 addWorker。
        if ON_ISLAND.load(O) && class == "NewSceneApartment" && sel == "setCurrentProduceMoleNums:" {
            // ★寄存器护栏(2026-09-06 审计发现):本臂 return false 放行真 setter,而 guest IMP 是直接
            // 沿用当前 r0(self)/r1(_cmd)/r2(参数) 取值的(abi.rs call_without_pushing_stack_frame 不重写
            // 它们)。下面每次 host 侧 msg_send 都会 clobber r0-r3(被调方留下的返回值),若只改回 r2 就
            // 放行,真 setter 会拿着**上一次 msg_send 的返回值**当 self 写 ivar = 写野指针。这里先整体
            // 快照 r0-r3,分派完再原样恢复,最后才按需改 r2。
            let saved_regs = [
                env.cpu.regs()[0],
                env.cpu.regs()[1],
                env.cpu.regs()[2],
                env.cpu.regs()[3],
            ];
            let self_id: id = Ptr::from_bits(env.cpu.regs()[0]);
            let new_v = env.cpu.regs()[2] as i32;
            let get_s = env
                .objc
                .register_host_selector("currentProduceMoleNums".to_string(), &mut env.mem);
            let old_v: i32 = msg_send(env, (self_id, get_s));
            if new_v > old_v {
                let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
                let shared = env
                    .objc
                    .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                let nsd: id = msg_send(env, (nsd_cls, shared));
                if nsd != nil {
                    let uid_s = env.objc.register_host_selector(
                        "userInfoDataInNewScene".to_string(),
                        &mut env.mem,
                    );
                    let uid: id = msg_send(env, (nsd, uid_s));
                    if uid != nil {
                        let add_s = env
                            .objc
                            .register_host_selector("addWorker:".to_string(), &mut env.mem);
                        let _: () = msg_send(env, (uid, add_s, new_v - old_v));
                        log!(
                            "[MOLECHEAT] island: 公寓雇用 +{} 摩尔(即时本地出)",
                            new_v - old_v
                        );
                    }
                }
                // 先恢复被上面若干次 msg_send 打乱的 r0-r3,再压回在产数,放行真 setter 写 old。
                env.cpu.regs_mut()[0..4].copy_from_slice(&saved_regs);
                env.cpu.regs_mut()[2] = old_v as u32;
                return false;
            }
            // 未命中"雇用(new>old)"分支(tick/道具走的减法路径)同样要恢复:上面已做过两次 msg_send。
            env.cpu.regs_mut()[0..4].copy_from_slice(&saved_regs);
        }

        // ★解 state1 等服务器回包的活锁(进岛加载卡死的根因):LoadingHoliday.updateLoading
        // 的唯一停点 state1(curStep_=2)置 updatePause_=1 后发 getAllObjects 等服务器回包;
        // 离线无回包→updatePause_ 永为1→每帧入口直接 return→curStep_ 永卡 2 = 活锁。每帧在
        // 真方法执行前,若 curStep_(self+0x10,int)>=2 就强清 updatePause_(self+0xC,char)=0,
        // 让状态机靠 curStep_ 自增走完(state2 的 mapData 已注入,其余态本地无门)。放行真方法。
        // ★[审计修 2026-09-11] 去掉 ISLAND_ENTER_WINDOW>0 前置:窗口按【帧】倒计时(1200 帧),进岛加载一慢(首次解图集/
        //   慢机器/掉帧)就先耗尽 → updatePause_ 不再被强清 → 永久卡在加载画面,且看门狗同谓词一起哑掉、不留痕迹。
        //   LoadingHoliday 只在 nextSceneId==10(进黄金岛)时才会被创建,仅按类名门控零回归。
        if class == "LoadingHoliday" && sel == "updateLoading:" {
            let self_bits = env.cpu.regs()[0];
            let cur_ptr: ConstPtr<i32> = Ptr::from_bits(self_bits + 0x10);
            let cur: i32 = env.mem.read(cur_ptr);
            if cur >= 2 {
                let pause_ptr: MutPtr<u8> = Ptr::from_bits(self_bits + 0xc);
                env.mem.write(pause_ptr, 0u8);
            }
        }

        // ★[审计修 2026-09-11] 进岛/在岛标志改为【事件驱动】(纯原子操作 + 读寄存器,无 msg_send):
        //   · ISLAND_LOADING:[LoadingManager enterLoadingWithDelegate:nextSceneId:] 且 r3==10。唯一调用点在 startNewSceneFrom
        //     已过网络门之后(0x24155e),LoadingHoliday 也只在此、仅 nextSceneId==10 时分配;r2=SceneMannager 自身。
        //   · ON_ISLAND:[SceneMannager loadNewScene:] 且 r2==10(唯一调用方 endLoadingScene;真方法入口第一件事写 curSceneId_=r2)。
        //     以前还要求 ISLAND_ENTER_WINDOW>0:加载超过 1200 帧就永远置不上 → 岛上全部 hook 静默失效。
        //   · LoadingHoliday alertView:didDismissWithButtonIndex: 且 buttonIndex(r3)==0:原版的加载中止路径(不切场景)。
        if class == "LoadingManager" && sel == "enterLoadingWithDelegate:nextSceneId:" {
            if env.cpu.regs()[3] == 10 {
                ISLAND_LOADING.store(true, O);
                ISLAND_SCENE_MGR.store(env.cpu.regs()[2], O);
                log!("[MOLECHEAT] island: >> enterLoading (加载场景开始,ISLAND_LOADING=true)");
            }
        } else if class == "SceneMannager" && sel == "loadNewScene:" && env.cpu.regs()[2] == 10 {
            ON_ISLAND.store(true, O);
            ISLAND_LOADING.store(false, O);
            ISLAND_EXITING.store(false, O);
            ISLAND_QUEST4_SENT.store(false, O); // [I5-01] 每次进岛重新判一次任务 4
            ISLAND_SCENE_MGR.store(env.cpu.regs()[0], O);
            // ★【已回滚】曾在此 load_island_shop_atlases 补加载 4 个建筑商店图集——实测它把黄金岛渲染搞坏成全绿场地。
            log!("[MOLECHEAT] island: >> loadNewScene (建 GameNewScene),ON_ISLAND=true");
        } else if class == "LoadingHoliday"
            && sel == "alertView:didDismissWithButtonIndex:"
            && env.cpu.regs()[3] == 0
            && ISLAND_LOADING.load(O)
        {
            ISLAND_LOADING.store(false, O);
            log!("[MOLECHEAT] island: 进岛加载被中止(LoadingHoliday 弹框 index0)→ ISLAND_LOADING=false");
        }
        // 曾有 gobackMainVillage 前置钩子清离岛标志,因真方法 0x23d19c 读到 isChangeSceneButtonSelected 会早退、抢跑会误判离岛而删除,勿复活
        // (离岛统一走网络门块里 startNewSceneFrom:toScene: 10→1 全局出口)。
        // ★[审计修 2026-09-11] 岛存档节拍(见 start_island_tick)。GameManager 不实现该选择子,必须 return true 吞掉。
        //   在岛上且有脏标记、距上次落盘 ≥1.5s → 落盘;岛会话仍活跃就续排下一拍,否则停。
        if sel == "moleIslandTick" {
            // [审查修] 合并重复节拍链:距上一次【被受理】的节拍 <600ms 的视为重复链,吞掉且不续排(单链间隔≈1s;
            //   两条链任意相位差下总有一个间隔 ≤0.5s,重复链必被收敛)。被吞的这拍不更新时间戳,免得误杀主链。
            let dup = ISLAND_LAST_TICK
                .with(|c| c.get())
                .map_or(false, |t| t.elapsed().as_millis() < 600);
            if dup {
                return true;
            }
            ISLAND_LAST_TICK.with(|c| c.set(Some(Instant::now())));
            // [2026-09-16 黄金岛审查修 I5-01] 岛农场任务链第 4 条的完成信号补发,见 island_resend_quest4_action。
            //   放在这里而不是放在 setCurrentProduceMoleNums: 钩子里:本臂是宿主自排的选择子、return true 吞掉,
            //   栈上没有任何游戏方法体(island_flush 本来就在这里发大量 msg_send),是全文件发消息最安全的点;
            //   而 finish 会对正打开的 ApartmentView 发 detech(卸载面板 + 回调),在 setter 钩子里触发它等于
            //   在 onButtonCallSelected: 方法体中段把 self 拆了,后面还要读 self->itemmoney,窗口危险且可能重入。
            island_resend_quest4_action(env);
            if ON_ISLAND.load(O) && ISLAND_DIRTY.load(O) {
                let due = ISLAND_LAST_FLUSH
                    .with(|c| c.get())
                    .map_or(true, |t| t.elapsed().as_millis() >= 1500);
                if due {
                    // [扫描修 2026-09-15] F10-6 节拍落盘只打一行:原因文本并入 island_flush 的汇总行(含「节拍落盘」与各「存盘 island_xxx.dat」)。
                    island_flush(env, "节拍落盘(岛上有未保存的变化)");
                }
            }
            if island_session_active() {
                schedule_island_tick(env);
            } else {
                ISLAND_TICK_RUNNING.store(false, O);
            }
            return true;
        }

        // ★[审计修 2026-09-11] 置脏:岛上经营/任务/剧情/成就/扩地/工人数都落在 NewSceneUserInfoData 的 set*/add*,
        //   经验/贝壳/金币/建设值/碎片走 NewSceneData 的 add*InNewScene:/addAdventureMapFragment:/setMapFragments:,
        //   新放置走 NetworkManager addObjectToServer:(这里只置脏不拦截,seqId 在它内部分配)。纯原子操作。
        if ON_ISLAND.load(O)
            && ((class == "NewSceneUserInfoData" && (sel.starts_with("set") || sel.starts_with("add")))
                || (class == "NewSceneData"
                    && (sel.ends_with("InNewScene:")
                        || sel == "addAdventureMapFragment:"
                        || sel == "setMapFragments:"
                        || sel == "saveUserinfoToLocal"))
                || (class == "NetworkManager" && sel == "addObjectToServer:"))
        {
            island_mark_dirty();
        }

        // ★[审计修 2026-09-11] 在岛上直接关窗口/Cmd+Q:touchHLE 的干净退出链(uikit.rs → ui_application::exit)
        //   依次给 AppDelegate 发 applicationWillResignActive: 与 applicationWillTerminate:,然后 process::exit。
        //   以前岛存档只在离岛时写 → 关窗 = 本局岛上进度全丢,而经济(金币/贝壳)早已即时写进 userinfo.dat
        //   (买建筑扣的钱在、建筑没了;交任务的奖励在、任务指针回滚=可无限刷)。两个回调各落一次盘,幂等。
        if class == "iMoleVillageAppDelegate"
            && (sel == "applicationWillResignActive:" || sel == "applicationWillTerminate:")
            && ON_ISLAND.load(O)
        {
            let saved = [
                env.cpu.regs()[0],
                env.cpu.regs()[1],
                env.cpu.regs()[2],
                env.cpu.regs()[3],
            ];
            log!("[MOLECHEAT] island: 应用即将退出({})→ 岛存档落盘", sel);
            island_flush(env, "应用退出落盘");
            env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
            return false;
        }

        // 曾在 HolidayVillageLayer onEnter 顶 gameMode=1,因会暂停 cocos2d director 冻结全岛(NPC/动画全停)而删除,勿复活
        // (点建筑 0x1 崩已由 messages.rs 根治)。

        // 网络门 #2/#3:进岛窗口内【或在岛上全程】把 NetworkManager 在线判定强制为真
        // (state==6=已登录)。在岛上续期是问题2 的核心:否则窗口20s过期后岛上周期/触摸
        // 网络检查恢复离线值→弹断网框→被自动「返回」踢人;且触摸需 state∈{5,6,7} 才走
        // 正常 processTouch(state=6 满足),否则触摸被网络检查分支吞掉。
        // [审计修 2026-09-11] 加上 ISLAND_LOADING:LoadingHoliday 在加载各步读 [NetworkManager isConnected]/state==6,
        //   读到离线就走 showNetConnectErrorMessage;以前只靠帧窗口,加载一慢就踩到。
        if ISLAND_ENTER_WINDOW.load(O) > 0 || ON_ISLAND.load(O) || ISLAND_LOADING.load(O) {
            match (class, sel) {
                ("NetworkManager", "isConnected") => {
                    env.cpu.regs_mut()[0] = 1;
                    return true;
                }
                ("NetworkManager", "state") => {
                    env.cpu.regs_mut()[0] = 6;
                    return true;
                }
                // ★[P3 商店空白真因·治本] -[SceneMannager curSceneId]:startNewSceneFrom:toScene:
                //   (0x241420)进岛时把 curSceneId_ 设成【2=过场/loading 态】,只有 loading 真正完成
                //   才推到 nextSceneId(=10)。离线流靠 host hook 驱动加载把岛渲染出来了,但 loading
                //   完成"把 curSceneId_→10"那一步常没触发 → 它卡在 2。而 -[NewStyleStoreItemsView
                //   loadObjectsDataByType:](0x3b9534)按 curSceneId 选数据源:==1→GameData、==10→
                //   NewSceneData,【既非1非10→数据源=nil→menuItemBuy=nil→numberOfCells=0→建设庄园/
                //   食材店全空格、买不了】。catalog(store 数组/食材桶)主村 boot loadPropertyWithType:
                //   + 我们 force-call loadFileWithType: 早填满了,空白纯是 curSceneId 读偏。
                //   修:在岛上(ON_ISLAND)把 curSceneId 强制为 10——loadObjectsDataByType: 读到已填满的
                //   NewSceneData store 数组→出货;并连带修好所有 curSceneId==10 门控的岛功能。
                //   安全:ON_ISLAND 只在 loadNewScene(GameNewScene 已建)后置 true、gobackMainVillage
                //   置 false=正好框在岛会话期;real==1(主村)不覆盖(防 ON_ISLAND 残留误伤);LoadingHoliday
                //   状态机用 curStep_(self+0x10)推进、不读 curSceneId,故不破坏加载。ivar 偏移=12
                //   (实读 _OBJC_IVAR_$_SceneMannager.curSceneId_=12)。
                // [2026-09-06 审计修] 离岛全局出口:岛上 HUD「返回」/「串门」按钮直调
                //   startNewSceneFrom:10→1,完全绕过 gobackMainVillage → 那条路离岛不存档、
                //   ON_ISLAND 还永久残留(回主村后建筑操作被一直吞掉)。这里按 fromScene==10 兜底。
                //   pre-hook,此刻 unloadMap 还没清空活表,merge 拿得到新放置的建筑。放行原方法。
                ("SceneMannager", "startNewSceneFrom:toScene:") if ON_ISLAND.load(O) => {
                    let from = env.cpu.regs()[2] as i32;
                    let to = env.cpu.regs()[3] as i32;
                    if from == 10 && to == 1 {
                        let saved = [
                            env.cpu.regs()[0],
                            env.cpu.regs()[1],
                            env.cpu.regs()[2],
                            env.cpu.regs()[3],
                        ];
                        log!("[MOLECHEAT] island: 离岛(startNewSceneFrom {}→{})→ 统一落盘", from, env.cpu.regs()[3] as i32);
                        island_flush(env, "离岛统一落盘");
                        // 4 条离岛路径(gobackMainVillage/菜单返回/串门/exitNewIsland:)都经过这里,且 to==1 跳过网络门,
                        // 放行后必定成功(0x24142e beq)。在岛标志在此清,curSceneId→10 强制随之停止,不会误路由主村加载。
                        ON_ISLAND.store(false, O);
                        ISLAND_ENTER_WINDOW.store(0, O);
                        ISLAND_SCENE_MGR.store(saved[0], O);
                        ISLAND_EXIT_FRAMES.store(3600, O);
                        ISLAND_EXITING.store(true, O);
                        env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
                    }
                    return false;
                }
                ("SceneMannager", "curSceneId") if ON_ISLAND.load(O) => {
                    let recv = env.cpu.regs()[0];
                    let slot: ConstPtr<i32> = Ptr::from_bits(recv + 12);
                    let real: i32 = env.mem.read(slot);
                    if real != 10 && real != 1 {
                        if !CURSCENE_DIAG_DONE.swap(true, O) {
                            log!(
                                "[MOLECHEAT] island: curSceneId 真实={} → 强制 10(修商店/岛功能空白)",
                                real
                            );
                        }
                        env.cpu.regs_mut()[0] = 10;
                        return true;
                    }
                    // 已是 10(loading 正常完成)或在主村(1):放行真 getter,不覆盖。
                }
                // ★[Barbara's House 雇佣摩尔修复·2026-06-23] -[NewSceneData moleUpperLimit] 是公寓雇佣门
                //   -[ApartmentView onButtonCallSelected:](0x325e80)的容量上限:门
                //   `curTotalWorkersCount + currentProduceMoleNums >= moleUpperLimit` 为真就弹
                //   "EXCEED_RESTAURANT_LIMIT"、雇不了。IDA 实证 moleUpperLimit 唯一非餐厅设值点是
                //   -[NewSceneData init] 设 0;餐厅 initWithMapData:type:(0x31b4f0)本应 setMoleUpperLimit:
                //   [getMoleUpperLimit](=levelupHV[30002][level].upgradeFinishMoleUpperCount),但离线这条没
                //   把它设成非0(实测=0:连第一只都雇不了=门 0>=0 恒真)。在岛上把 moleUpperLimit 顶到 ≥16
                //   (餐厅 level1 原版上限,内存实证),real≥16(餐厅真升过级)则保留真值不降。ivar 偏移=180
                //   (实读 _OBJC_IVAR_$_NewSceneData.moleUpperLimit)。配合已有 setCurrentProduceMoleNums:→
                //   addWorker hook,雇佣即时增加 curTotalWorkersCount(addWorker@0x3233e0 实证 +总数+空闲+出摩尔)。
                ("NewSceneData", "moleUpperLimit") if ON_ISLAND.load(O) => {
                    let recv = env.cpu.regs()[0];
                    let slot: ConstPtr<u32> = Ptr::from_bits(recv + 180);
                    let real: u32 = env.mem.read(slot);
                    if real < 16 {
                        env.cpu.regs_mut()[0] = 16;
                        return true;
                    }
                    // real≥16(餐厅已升级到更高上限):放行真 getter,不降级。
                }
                // ★isReachable 必须匹配【几乎任意类】= 进岛刚需(workflow 实证):进岛链上多处
                // `[self isReachable]` 的接收者是 NetworkManager 之外的类(GameManager/VillageLayer/
                // SceneMannager/HolidayVillageLayer/NewSceneQuestLayer/LoadingHoliday 等),
                // 收窄到 NetworkManager 会让这些门判离线走偏。任意类→1 的门已收在窗口/在岛,主村空过;
                // 触摸 0x1 崩另有 showWithTarget 兜底独立挡住,不靠收窄它。
                // ★[深扫修 2026-09-11] #10 排除 NewScenePorter 与 Porter(以前注释把 NewScenePorter 误列为"网络门接收者")。
                //   取证:-[NewScenePorter isReachable]@0x26b114 是【扩地边界判定】,与网络无关——读
                //   [[NewSceneData sharedInstance] userInfoDataInNewScene].extendMap,目标格 line/column 超出已购扩地带就返回 0;
                //   全二进制唯一对 NewScenePorter 发 isReachable 的是 -[NewScenePorter checkCanPut:]@0x271270(接收者=self),
                //   返回 NO 就写可放置标志=0。主村 -[Porter isReachable]@0x29228 / checkCanPut:@0x3017a 同构。以前通配臂吞掉它们 →
                //   未购买的扩地区域也能盖建筑、还被写回 island_map.dat。排除后这两个类落到下面 `_ => {}`,intercept 返回 false、
                //   放行真方法:本臂之前没有任何 msg_send,r0(self)/r1(_cmd)原样未动,真方法读 self 正确。
                //   不改成"类自己实现了 isReachable 就放行":NetworkManager 自己也实现了(imp 0xed2fc),那样会把进岛最关键的门放掉。
                // [2026-09-16] E-01 再按调用点排除商店主菜单「免费贝壳」按钮:-[NewStyleStoreMainLayer onItemsMenuSelected:]@0x3b2378
                //   对 0x11 号菜单项在 0x3b23c0 `blx [NetworkManager isReachable]`(LR=0x3b23c5,带 Thumb 位),为真才在 0x3b23ce 以
                //   itemid 8 调 onBuyVIPGold:(广告墙「免费贝壳」),为假弹原版 IAP_NETWORK_ERROR「咦，你的设备没有连接网络哦」。以前岛上
                //   这里通配成 1,岛上点它会进 SHELLHOOK 白送贝壳并误触发充值副作用,主村却弹离线提示。排除后落到下面 `_ => {}`,放行真
                //   isReachable(本臂之前没有 msg_send,寄存器未动),岛上与主村一样弹原版离线提示。只精确排除这一个 LR,进岛链上其它门不受影响。
                (c, "isReachable")
                    if c != "NewScenePorter" && c != "Porter" && env.cpu.regs()[14] != 0x3b23c5 =>
                {
                    env.cpu.regs_mut()[0] = 1;
                    return true;
                }
                // ★进岛卡死真凶硬掐断(workflow 实证):离线下游戏会走 NSKeyedArchiver 归档一个
                // "边走边膨胀"的对象图——缓冲回放(sendAllBufferDatas imp 0x226d84,按包循环逐包
                // encodeWithCoder:,由 LoadingHoliday case0 经 checkBuffDataFileForCurrentUserIdExistOrNot
                // 在【磁盘有残留缓冲文件】时触发,故时有时无)或 save 路径(archivedDataWithRootObject:
                // 37 处)。touchHLE 归档器忠实深度遍历,每步新建 NSMutableData 命不中去重表→不收敛→
                // 看似死锁(看门狗抓到的 CCNode visit 0x2d30cc 是同源的果)。离线岛布局本就每进岛重注入、
                // 无需持久化,故直接掐断安全且治本。【不吞 encodeWithCoder:】——17 个类拿它当自有方法名,
                // 吞它副作用面过大;掐"驱动遍历的入口"比掐"遍历的每一步"精准。
                // [P2b 经营进度回写] 升级餐厅/雇用公寓/出海改的活建筑,游戏 saveTMMapDataFromObject:
                //   现造快照(a3,其 objectSequenceId 已对齐活对象 objSequenceId)喂 setModObjectToServer:
                //   发 1060;离线发包被吞、从不写回 mapData → 经营进度退岛丢。先把快照按 seqId 写回
                //   mapData,再 return true 跳过原方法(原方法只发被吞的包+push buffer,跳过顺带免积压)。
                //   注:新建筑 add 的 seqId 在 addObjectToServer: 内才分配,pre-hook 拿不到 → P3 放置链
                //   另解;此处只保【经营态】(mod,seqId 已就绪,覆盖默认岛 90001+ 的种子建筑)。
                ("NetworkManager", "setModObjectToServer:") => {
                    let snap: id = Ptr::from_bits(env.cpu.regs()[2]);
                    writeback_island_object(env, snap);
                    island_mark_dirty();
                    return true;
                }
                // [2026-09-06 审计修] 删除同理:不接管则 mapData 只增不删,拆掉/收纳掉的建筑下次
                //   进岛原地复活(一键收纳 storeOnekey: 会批量走这里,复活整批)。
                ("NetworkManager", "deleteObjectFromServer:") => {
                    let snap: id = Ptr::from_bits(env.cpu.regs()[2]);
                    delete_island_object(env, snap);
                    island_mark_dirty();
                    return true;
                }
                // (a) ★storm 真驱动:sendPacket:commandId:(imp 0xe231d)——离线下每个包都被
                //     encodeWithCoder: 序列化,残留缓冲里几千个包逐个发=刷屏卡死(看门狗实锤:LR
                //     落在 sendPacket:commandId: imp+0x4a,日志爆刷 encodeWithCoder no-op 7000+ 行)。
                //     离线本就发不出去,直接吞掉整条=根治 storm。(上一版砍 sendAllBufferDatas 砍错
                //     了选择子:storm 是直接循环 sendPacket,不走那个包装方法。)
                (_, "sendPacket:commandId:") => {
                    return true; // 离线无服务器,发包=空过且每包序列化必卡 → 吞掉
                }
                // (a2) 缓冲回放包装也一并吞(belt-and-suspenders;其三调用方全空过)。
                (_, "sendAllBufferDatas") | (_, "sendAllBuffDataInNewSceneLoading") => {
                    return true; // 离线无服务器,缓冲回放无意义且必卡 → 吞掉
                }
                // ★Bug A(布兰的家面板不弹)修复——LR 收窄,绝不冻岛:
                // RestaurantView showWithTarget:selector:(imp 0x249769)开头有门
                // `[[NewGameManager sharedManager] gameMode]==1`(实证 0x2497a4 读 gameMode,该 blx
                // 返回址 LR=0x2497a9;cmp#1/bne.w 0x24996a)。一键进岛后 gameMode≠1 → 门 bail → 面板
                // 不弹。绝不能全局顶 gameMode=1(=暂停 cocos2d director=整岛 freeze,本会话血坑)。
                // 改 LR 收窄:仅当"正是这道门在读 gameMode"(LR==0x2497a9,该 blx 独有返回址;实证
                // showWithTarget 体内 gameMode 只读这一次)时返 1,其余 200+ 处 gameMode 读 LR 不符 →
                // 落下面 `_ => {}` 走真值 → scheduler/NPC/触摸不受影响 = 不冻岛。
                // ★回退建设庄园门1(0x25aab9):实测加它后建设庄园渲染崩(numberOfCellsInTableView
                //   self=脏指针@0x12b),且 gmdiag 证明建设庄园 gameMode 天然=1、门没挡、数据照样加载
                //   (count=35)——门改动多余且有害。只保留布兰的家(0x2497a9)。
                // ★P2 公寓面板门:ApartmentView showWithTarget:selector:(imp 0x3263fc)与餐厅同构,
                //   开头也 `[[NewGameManager sharedManager] gameMode]==1` 才弹面板(blx@0x326436 →
                //   返回址 LR=0x32643b)。与餐厅 0x2497a9 一样 LR 收窄放行(各自 showWithTarget 体内
                //   唯一一次 gameMode 读),否则离线进岛点公寓不弹经营面板。绝不全局顶 gameMode(冻岛)。
                ("NewGameManager", "gameMode")
                    if env.cpu.regs()[14] == 0x2497a9 || env.cpu.regs()[14] == 0x32643b =>
                {
                    env.cpu.regs_mut()[0] = 1;
                    return true;
                }
                // [P3-b 食材商店门] ShopItemsLayer showWithTarget:(0x24be80)开头 [WrapperManager
                //   currentGameMode]==1 才显示商店(blx 返回址 LR=0x24bec3;cmp@0x24bec2/bne@0x24bec4)。
                //   岛待机 gameMode≠1 → 食材商店空格。LR 收窄放行(仅这一处 currentGameMode 读;0x1329c7
                //   是 ArrowSprite 的无关门,不碰)。currentGameMode@0x261518:岛(curSceneId10)用 gameMode。
                ("WrapperManager", "currentGameMode") if env.cpu.regs()[14] == 0x24bec3 => {
                    env.cpu.regs_mut()[0] = 1;
                    return true;
                }
                // ★Bug C(岛商店商品锁)修复:getLockType4ShopItem:shop:(imp 0x21eec1)返
                // 0=解锁 / 1,2,3,5=等级/前置/雇工锁。离线无服务器等级权威 + 玩家可能未达门 → 全顶 0
                // 解锁。纯本地等级门,只放宽不破坏;onChooseUse 不经此条,不误伤。(注:这解决"能否买";
                // 空格子是目录未填、另行诊断——锁只灰格不删格。)
                ("NewSceneData", "getLockType4ShopItem:shop:") => {
                    env.cpu.regs_mut()[0] = 0;
                    return true;
                }
                // ★Bug C 真修(岛商店点分类格子全空)——workflow 二进制实证:格子空【不是桶空】(桶在
                // 主村启动期 loadPropertyWithType:1 andSceneId:10 已填满 20 食材),而是 ShopItemsLayer
                // showWithTarget:(imp 0x24be81)开头一道 `[[WrapperManager sharedManager] currentGameMode]
                // ==1` 门(currentGameMode blx@0x24bebe 返回址 LR=0x24bec2,cmp#1/bne.w 0x24c114)——
                // gameMode≠1 就 bail、shopItemsIds_ 永不赋值 → numberOfCellsInTableView 读 nil count=0 =
                // 零格。这是布兰的家(上面 gameMode 臂)的【兄弟门】。同样 LR 收窄:仅这一处返1,放行后
                // getShopItemsIds: 返 4 件桶 → 出 4 格(价格/可买齐;图标/中文名缺=propertyHV 限制,可接受)。
                // ★LR 必须带 thumb 位(=cmp地址+1):食材商店 cmp@0x24bec2 → LR=0x24bec3(上版误写
                //   0x24bec2 漏 thumb 位 = 根本没生效)。★建设庄园门2 NewStyleStoreMainLayer.
                //   showWithTarget:selector: 也读 [WrapperManager currentGameMode]==1(blx@0x3aeec0,
                //   cmp@0x3aeec4 → LR=0x3aeec5;≠1 面板入口 bail、6 分类网格全跳过)——这才是用户点的
                //   "建设庄园(卖建筑)",不是 ShopItemsLayer 食材商店。一并放行,放行后网格自然渲染。
                // ★【已整条回退 currentGameMode hook】:gmdiag 实测建设庄园 currentGameMode 真实 LR
                //   =0x1329c7(我之前的 0x24bec3/0x3aeec5 全错、根本没触发);且建设庄园 gameMode 天然
                //   =1、门没挡、数据照样加载(count=35),空格子是【渲染/明细】问题不是门。门改动多余
                //   且疑似把建设庄园推进到会崩的渲染路径,整条移除。(上面那段 currentGameMode 注释为
                //   历史记录;食材商店若日后真需放行,用 gmdiag 抓到的真 LR 再加。)
                // ★岛屿可建面积扩大(workflow 实证,方案①低风险):网格其实 47×117 很大,可建区由陆地
                // tile 表(环岛形≈833格)+ checkCanPut:(0x271051)的水域/海岸禁建门决定。掐这两道门
                // (NewScenePorter 独有,岛专属)→ 可建区从环岛窄带扩到环带内侧/浅水。仍受 per-tile
                // property 门约束(不放开),故只在原岛轮廓内放宽、不让纯海可建=零美术穿帮。
                ("NewScenePorter", "inRectOfAquaticAreaOrNot:") => {
                    env.cpu.regs_mut()[0] = 0; // 不在水域禁建矩形
                    return true;
                }
                ("NewScenePorter", "checkBeyoundLeftCircleBeach:") => {
                    env.cpu.regs_mut()[0] = 0; // 未越过左侧海岸圈
                    return true;
                }
                // 曾有 (_,"archivedDataWithRootObject:") 归 nil 兜底,因把岛会话内自动存档写成 36 字节空壳坏档(下次启动崩)而删除,勿复活。
                _ => {}
            }
        }

        // 进岛起点:一看到 enterNewIslands 就开窗 + reset 注入标志,放行原方法。开窗是为
        // 下游 startNewSceneFrom 的三道 NetworkManager 门(isReachable/isConnected/state)在
        // SUCC 帧边界执行时铺路。(注:enterNewIslands 自身真实前置门是 GameManager.gameMode
        // ∈{0,1,6} 与 SceneMannager.isChangeSceneButtonSelected==NO;它的 isReachable 已被
        // 破解版 nop 掉、不是门。)
        if sel == "enterNewIslands" {
            // ★[审计修 2026-09-11] 按真方法(0x375b0)的两道前置门预判:gameMode∈{0,1,6} 且
            //   isChangeSceneButtonSelected==NO。门不过真方法会静默 return,以前窗口照开 1200 帧 → 主村被强制
            //   判成"在线"20 秒(setModObjectToServer: 等被吞)。门不过就不开窗。msg_send 前后护住 r0-r3。
            let saved = [
                env.cpu.regs()[0],
                env.cpu.regs()[1],
                env.cpu.regs()[2],
                env.cpu.regs()[3],
            ];
            let sm_s = env
                .objc
                .register_host_selector("sharedManager".to_string(), &mut env.mem);
            let gm_cls = env.objc.get_known_class("GameManager", &mut env.mem);
            let gm: id = msg_send(env, (gm_cls, sm_s));
            let gmode: i32 = if gm != nil {
                let g = env
                    .objc
                    .register_host_selector("gameMode".to_string(), &mut env.mem);
                msg_send(env, (gm, g))
            } else {
                -1
            };
            let sc_cls = env.objc.get_known_class("SceneMannager", &mut env.mem);
            let sc: id = msg_send(env, (sc_cls, sm_s));
            let busy: u8 = if sc != nil {
                let g = env.objc.register_host_selector(
                    "isChangeSceneButtonSelected".to_string(),
                    &mut env.mem,
                );
                msg_send(env, (sc, g))
            } else {
                0
            };
            env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
            if !matches!(gmode, 0 | 1 | 6) || busy != 0 {
                log!(
                    "[MOLECHEAT] island: enterNewIslands 真方法将早退(gameMode={} isChangeSceneButtonSelected={})→ 不开网络窗口",
                    gmode,
                    busy
                );
                return false;
            }
            ISLAND_INJECTED.with(|c| c.set(false));
            ISLAND_GATE1_HIT.store(false, O);
            if ISLAND_ENTER_WINDOW.load(O) <= 0 {
                ISLAND_ENTER_WINDOW.store(1200, O);
            }
            log!("[MOLECHEAT] island: enterNewIslands — opened network window");
            return false; // 放行原方法
        }

        // 网络门 #1:进岛数据同步。原版发包等服务器回 SUCC 回调;离线无回包 → 开窗 +
        // 把成功回调 onGameDataInMainVillageUpdateSUCC【异步】排到 run loop 的 perform 相位
        // (performSelector:withObject:afterDelay:0)再触发——绝不在当前/draw 栈内同步换场,
        // 避免 cocos2d scheduler 重入活锁(热点路整屏卡死的根因)。吞掉发包。
        if class == "GameManager"
            && sel == "updateGameDateForEnterNewSceneWithTarget:andCallback:"
        {
            let target: id = Ptr::from_bits(env.cpu.regs()[2]); // r2 = target(VillageLayer)
            ISLAND_INJECTED.with(|c| c.set(false));
            ISLAND_ENTER_WINDOW.store(1200, O); // ~20s @60fps,覆盖飞机过场 + 全部加载态
            if target != nil {
                let suc = env.objc.register_host_selector(
                    "onGameDataInMainVillageUpdateSUCC".to_string(),
                    &mut env.mem,
                );
                let pf = env.objc.register_host_selector(
                    "performSelector:withObject:afterDelay:".to_string(),
                    &mut env.mem,
                );
                // [target performSelector:onGameDataInMainVillageUpdateSUCC withObject:nil afterDelay:0]
                let _: () = msg_send(env, (target, pf, suc, nil, 0.0f64));
            }
            ISLAND_GATE1_HIT.store(true, O);
            log!("[MOLECHEAT] island: gate#1 — scheduled SUCC via perform afterDelay:0, swallowed packet");
            return true; // 吞掉发包
        }

        // state-1 向服务器拉岛物件:离线没有回包,改成本地注入默认岛 mapData,使
        // state-2(mapData.count>0)放行;吞掉发包。每次进岛只注入一次。
        if sel == "getAllObjectsListFromServerWithStartId:" && (ISLAND_ENTER_WINDOW.load(O) > 0 || ISLAND_LOADING.load(O)) {
            if !ISLAND_INJECTED.with(|c| c.get()) {
                ISLAND_INJECTED.with(|c| c.set(true));
                build_default_island_mapdata(env);
            }
            return true;
        }
    }

    if KILL_ANTICHEAT.load(O) {
        match (class, sel) {
            ("GameData", "isHackData") | ("NewSceneUserInfoData", "isHackData") => {
                env.cpu.regs_mut()[0] = 0; // NO — never flagged as hacked
                return true;
            }
            ("WrapperManager", "showCheatWarningMessage")
            | ("iMoleVillageAppDelegate", "showCheatWarningMessage") => {
                env.cpu.regs_mut()[0..2].fill(0); // swallow the warning UI
                return true;
            }
            ("NewSceneData", "checkUserinfoMd5:") => {
                env.cpu.regs_mut()[0] = 1; // YES — checksum passes
                return true;
            }
            ("NewSceneData", "CheckUserInfoData:") => {
                env.cpu.regs_mut()[0] = 0; // 0 == OK
                return true;
            }
            // Clock-tamper watchdog (would otherwise pop FOUND_TIME_CHEAT_MESSAGE
            // once time-magic features are used). Neuter both its start and check.
            ("SystemTimeCheck", "check") | ("SystemTimeCheck", "start") => {
                env.cpu.regs_mut()[0..2].fill(0);
                return true;
            }
            _ => {}
        }
    }

    // VIP: force "is VIP user" + a high VIP level/value. Only the methods that
    // actually exist on this build are hooked (verified against the method table):
    //   - WrapperManager checkIsVipUser     (the real "is this a VIP" check)
    //   - UserInfoLayer isShowVIPFunctionsButton:  (show the VIP UI)
    //   - UserVIPInfoData vipLevelWithNewType  (the real VIP-level getter; there
    //     is NO plain `vipLevel` getter, and UserInfoData/GoldSprite have no
    //     isVip/vipLevel at all — those earlier hooks were dead no-ops).
    //   - UserVIPInfoData vipValue           (raw VIP growth points)
    if FORCE_VIP.load(O) {
        match (class, sel) {
            ("WrapperManager", "checkIsVipUser") => {
                env.cpu.regs_mut()[0] = 1; // YES — treat as a VIP user
                return true;
            }
            // 修1:isShowVIPFunctionsButton: 是【带 BOOL 参(r2)的 void setter】,不是
            // getter。原来和 checkIsVipUser 并臂 r0=1+return true,等于把这个 setter 整个
            // 跳过、VIP 按钮的显示逻辑根本没跑。正确做法:把参数 r2 强制成 1(YES)再
            // 放行原方法(return false),让它把 VIP UI 按钮真正接上。
            ("UserInfoLayer", "isShowVIPFunctionsButton:") => {
                env.cpu.regs_mut()[2] = 1; // BOOL arg = YES
                return false; // run the real setter with the forced argument
            }
            // ★ 闪退真凶修复:vipLevelWithNewType 返回的是【NSString*】(类型编码 @8@0:4,
            // 真身 `[NSString stringWithFormat:@"%d", decryptInt(vipLevel_)]`),不是 int。
            // 所有调用方拿到后立刻 `[结果 intValue]`(VIP 总闸 checkIsVipUser 就是
            // `[[...vipLevelWithNewType] intValue] > 0`)。原来这里把 r0 写成裸整数 1..4 当
            // 指针返回 → `[0x00000004 intValue]` 向非法地址发消息 → EXC_BAD_ACCESS 闪退
            // (一开强制VIP、一进 VIP 相关 UI/商店就崩的根因)。改成返回一个永驻 NSString
            // (VIP_LEVEL 的字符串):[intValue] 得到正确等级、VIP 判定通过、且绝不崩。
            ("UserVIPInfoData", "vipLevelWithNewType") => {
                let s = match VIP_LEVEL.load(O).clamp(1, VIP_LEVEL_MAX) {
                    1 => "1",
                    2 => "2",
                    3 => "3",
                    _ => "4",
                };
                let ns = crate::frameworks::foundation::ns_string::get_static_str(env, s);
                env.cpu.regs_mut()[0] = ns.to_bits();
                return true;
            }
            // 曾拦 GameData getVipInfoDataOfCurrentUser(原「修2」),因多余已删,勿复活。
            // [扫描修 2026-09-15] F5-10 纠错:旧注释说"vipDataDic_ 只有服务器下发才填、离线恒空"是错的——
            //   -[GameData load:type:]@0x7b9b6 无条件调 loadVipUserInfoData 从本地 250_1.dat 读 4 级,原版方法离线也返回
            //   对应 VipInfoData,强制 VIP 下 VIP 加成/折扣真实生效。
            ("UserVIPInfoData", "vipValue") => {
                env.cpu.regs_mut()[0] = 999_999; // plenty of VIP growth value
                return true;
            }
            _ => {}
        }
    }

    // Player level: override the curLevel getter (and its scene variant)
    // exactly the way force_vip overrides vipLevel.
    // ★[深扫修 2026-09-11] #5 删掉 ("UserInfoData","encryptCurLevel") 臂。
    //   根因:-[UserInfoData encryptCurLevel]@0xbb030 直接返回【密文槽】原值(ldr r0,[r0,r1]; bx lr),全二进制唯一
    //   调用点 -[UserInfoData intiWithUserInfo:]@0xb960a 把它原样 str 回自己的密文槽(0xb9624)。钩子在这里返回
    //   明文 FORCE_LEVEL → 明文被当密文存进活对象;此后 curLevel 解密(eors #0x01011011)得到约 1684 万,关掉作弊后
    //   任意一次 saveUserInfoData 就把坏等级永久写进 userinfo.dat。encryptCurLevel 只用于对象间复制密文、与显示
    //   无关,删掉无任何功能损失;不采用"返回 FORCE^0x01011011"备选(那会把作弊从显示覆盖变成真实改档,关掉后回不去)。
    if FORCE_LEVEL.load(O) > 0 {
        match (class, sel) {
            ("UserInfoData", "curLevel") | ("NewSceneData", "getLevel") => {
                env.cpu.regs_mut()[0] = FORCE_LEVEL.load(O) as u32;
                return true;
            }
            _ => {}
        }
    }

    // [MoleWorld] mapExtend 修复(见 fix_mapextend_on() 注释):在线进村存档 mapExtend=6 与满图
    // 内容不一致 → curVisibleArea/curWalkableArea/curBornArea/setBkg 算出错误可视区 → 拖动闪。
    // ★[2026-09-16] F1-02 只在这 4 个取景调用点生效,返回 真值|0x1F(保留 0x1F 以上的位,如存档 287=0x11F 的 0x100)。
    //   根因:以前对 -[UserInfoData mapExtend]@0xbd6cc(`ldrh r0,[r0,r1]` 读 mapExtend_ +72,纯 u16 getter)的全部 23 个调用点
    //   都返回 0x1F:encodeWithCoder: 在 0xba246 取值编码 → 每次存档把 0x1F 永久写进 userinfo.dat;encodeUserInfoData
    //   0xbc47c 上传私服;Bridge/Ladder onFinishHandler、ObjectManager moveBridge:/checkMapExtendError、VillageMenuLayer
    //   addNewObject2Map:gift: 读后 orr 再 setMapExtend: 把假值写回 ivar;Porter isReachable 摆放可达、GameData getLockType4Object:、
    //   Quest 任务与 AchievementControl checkAchieve_ReqMap 成就判定全被直接满足。与深扫 #5 encryptCurLevel 显示覆盖漏进存档同类。
    //   现在按调用者 LR 精确匹配 MAPEXTEND_VIEW_LRS(四处取值后都只用低 5 位:0x350c0/0x351f4 and #0x1f、0x35364 ands #0x1f、
    //   setBkg 存到 [sp,#0xa0] 后全函数只在 0x3358a tst #0x10 用一次;所以 真值|0x1F 与旧的恒 0x1F 对这 4 处效果完全相同),
    //   其余调用点一律放行真 getter。不加在线门控(离线也有早先在线同步来的 mapExtend=6 本地坏档要兜底);不额外调
    //   checkMapExtendError(原版 -[GameManager endLoadCallBack]+0x30 已调,且它只补 0x2/0x4 两位)。
    if fix_mapextend_on() {
        if let ("UserInfoData", "mapExtend") = (class, sel) {
            let lr = env.cpu.regs()[14];
            if MAPEXTEND_VIEW_LRS.contains(&lr) {
                let recv: id = Ptr::from_bits(env.cpu.regs()[0]);
                let real: Option<u16> = if recv == nil {
                    None
                } else {
                    env.objc
                        .object_lookup_ivar(&env.mem, recv, &"mapExtend_".to_string())
                        .map(|p| {
                            let p: MutPtr<u16> = p.cast();
                            env.mem.read(p)
                        })
                };
                // 查不到 ivar(理论上不会)时退回旧值 0x1F:只影响这 4 个取景调用点,不会进存档。
                let ret: u32 = real.map_or(0x1F, |r| (r as u32) | 0x1F);
                log_first_then_dbg!(
                    LOG1_MAPEXTEND_VIEW,
                    "[MOLECHEAT] mapExtend 取景覆盖:LR={:#x} 真值={:?} → 返回 {:#x}(只改 4 个取景调用点,存档/扩地/成就/任务读真值)",
                    lr,
                    real,
                    ret
                );
                env.cpu.regs_mut()[0] = ret;
                return true;
            }
            // 其余 19 个调用点:放行真 getter。
        }
    }

    // All shop / collection items reported as unlocked.
    if ALL_UNLOCK.load(O) {
        match (class, sel) {
            // 收藏册/音乐"已解锁"显示判定 + 头像所需 VIP 等级 → 满足(返回 YES=1)
            ("WrapperManager", "isUnlockedItem:")
            | ("MusicHallLayer", "checkIsUnlockMusic:")
            | ("AvatarLayer", "checkRequiredVipLevel:") => {
                env.cpu.regs_mut()[0] = 1;
                return true;
            }
            // 实际下种/摆放/购买/装扮走的锁链路:getLockType4* 全族 → 0(=完全解锁)。
            // 这是 all_unlock 之前的空白(它只管"已解锁显示"),与既有
            // getLockType4ShopItem:shop:→0 同构。作物/物品/家具/宠物/头像/礼物/房间/音乐厅
            // 装扮/海洋岛物品在使用层面全部解锁。
            ("GameData", "getLockType4Crop:")
            | ("GameData", "getLockType4CropWithId:")
            | ("GameData", "getLockType4Object:")
            | ("GameData", "getLockType4Gift:")
            | ("NewSceneData", "getLockType4Object:")
            | ("NewSceneData", "getLockType4Crop:")
            | ("DecorateRoomLayer", "getLockType4Decorate:")
            | ("MusicHallLayer", "getLockType4Decorate:") => {
                env.cpu.regs_mut()[0] = 0; // 0 == unlocked
                return true;
            }
            _ => {}
        }
    }

    // 工人/房间补满:三个 ivar getter 恒返回 99 → 收菜/建造永不卡人力、房间不卡容量。
    // [2026-09-16] G-07 只管主村,菜单标签注明「仅主村」。岛上工人走 -[NewSceneUserInfoData curTotalWorkersCount]@0x3239c4,不在这里全局拦:
    //   save_island_userinfo 用宿主 msg_send 读这个 getter 写进 island_userinfo.dat,读档时再 setCurTotalWorkersCount: 写回,
    //   恒返回 99 会把 99 永久存进岛档。另外已核实主村有同类问题(本包不改,另记):-[UserInfoData encodeWithCoder:]@0xb9f98
    //   在 0xba0e2/0xba108/0xba17a 就是经 totalWorkers/availableWorkers/totalRooms 这三个 getter 取值编码的,开着开关时存档,
    //   99 会写进 userinfo.dat,关掉开关后不会回退。
    if MAX_FACILITY.load(O) {
        match (class, sel) {
            ("UserInfoData", "totalWorkers")
            | ("UserInfoData", "availableWorkers")
            | ("UserInfoData", "totalRooms") => {
                env.cpu.regs_mut()[0] = 99;
                return true;
            }
            _ => {}
        }
    }

    // 产出 ×10:收菜结算的建筑加成倍率 getter(百分比,100=1 倍;公式 reward*multiple/100)
    // 恒返回 1000=10 倍。走游戏原生收菜管线,无溢出风险(比直接加币稳)。
    if HARVEST_MULT.load(O) {
        match (class, sel) {
            ("ObjectManager", "getXPSpeedUpObjectMultiple")
            | ("ObjectManager", "getGoldSpeedUpObjectMultiple") => {
                env.cpu.regs_mut()[0] = 1000;
                return true;
            }
            _ => {}
        }
    }

    // 任务秒完成免费:用贝壳立即完成任务/催熟所需的贝壳数 → 0。
    // [2026-09-16] G-07 补上黄金岛任务 NewSceneQuest、日常任务 DailyQuest、VIP 任务 VipQuest。intercept 拿到的是接收者 isa 的
    //   精确类名、不沿父类链,而这三个类都不继承 Quest(NewSceneQuest : CCNode,DailyQuest/VipQuest : NSObject),各有自己的
    //   shellsNeeded(0x32ab40/0x341d48/0x389268,返回 int)。以前只列 Quest/TimeQuest,岛上、日常、VIP 任务面板的「立即完成」
    //   照样收贝壳。调用者只有各任务层的 updateTimeInfo:/onShellButtonPressed/quickFinish,只读不落盘。
    //   粗筛走 intercept_wants 末尾的 FREE_QUEST 门控,没把类名加进 CLASSES。
    if FREE_QUEST.load(O) {
        match (class, sel) {
            ("Quest", "shellsNeeded")
            | ("TimeQuest", "shellsNeeded")
            | ("NewSceneQuest", "shellsNeeded")
            | ("DailyQuest", "shellsNeeded")
            | ("VipQuest", "shellsNeeded") => {
                env.cpu.regs_mut()[0] = 0;
                return true;
            }
            _ => {}
        }
    }

    // 海底寻宝必中稀有:generateRandomRewardId 掷骰(1-100)按 7 档查 id 表;最稀档(roll6-10)
    // = id 31169(脱壳实证 dump 的 id 表)。恒返回它 = 必中最稀奖励。
    if SEABED_BEST.load(O)
        && class == "SeabedSeekingTreasureMainLayer"
        && sel == "generateRandomRewardId"
    {
        env.cpu.regs_mut()[0] = 31169;
        return true;
    }

    // 小游戏奖励满:在所有小游戏共用的结算点把本局摩尔豆/经验放大。
    // [2026-09-16] A2-03+G-04 原来钩的是 +[FishingGame getRewardCoin:](0x15e3b4,全二进制零调用)和
    //   +[MinerGame getRewardCoin:/getRewardXp:](挖矿每块矿石初始化、MinerAchivement 显示也读):结果只有挖矿石变,矿石初始数值还被
    //   改成 99999;切水果、钓鱼、拍虫子、敲木桩、左左右右完全不变。三个臂已删。
    //   所有小游戏结算都汇入 -[MiniGameManager enterAchivement:]@0xf4544:0xf458c `[m_curMiniGame gainXP]`、0xf45a2
    //   `[m_curMiniGame gainCoin]`(继承自 -[MiniBase gainXP]@0xf3234 / gainCoin@0xf3260,返回 int ivar m_gainXP/m_gainCoin),
    //   写进 m_achivementData,之后 -[Building onMiniGameFinished] 据此 addGold:/addXp: 入账。
    //   只在 LR 精确等于这两处 blx 的返回地址(0xf4591/0xf45a7,带 Thumb 位)时放大:selref gainCoin 的另一处在
    //   -[MinerGame caculateReward],DivineGame 走 enterDivineGameAchivement,都不受影响。接收者类名是子类(CutFruit/BugGame/
    //   Plow/FishingGame/MinerGame/WashRoomGame),ivar 用 object_lookup_ivar 沿父类链按名字查,不写死 +304/+308(兼容非脆弱 ivar
    //   修正写回)。放大规则:原值 >0 时 ×10、封顶 99999、且不小于原值;用倍数不用定值,是怕一次给太多触发 isHackData 反作弊弹框。
    //   会与「金币 x10」「经验 x10」叠乘。前置拦截,没发宿主消息,吞掉后自写 r0;查不到 ivar 就放行真 getter。
    //   粗筛走 intercept_wants 末尾的 MINIGAME_REWARD 门控。
    if MINIGAME_REWARD.load(O) && (sel == "gainCoin" || sel == "gainXP") {
        const LR_ENTER_ACHIVEMENT_GAIN_XP: u32 = 0xf4591;
        const LR_ENTER_ACHIVEMENT_GAIN_COIN: u32 = 0xf45a7;
        let lr = env.cpu.regs()[14];
        if lr == LR_ENTER_ACHIVEMENT_GAIN_XP || lr == LR_ENTER_ACHIVEMENT_GAIN_COIN {
            let recv: id = Ptr::from_bits(env.cpu.regs()[0]);
            let ivar_name = if sel == "gainCoin" {
                "m_gainCoin"
            } else {
                "m_gainXP"
            };
            let slot = env
                .objc
                .object_lookup_ivar(&env.mem, recv, &ivar_name.to_string());
            if let Some(slot) = slot {
                let raw: u32 = env.mem.read(slot);
                let orig = raw as i32;
                let boosted: i32 = if orig > 0 {
                    orig.saturating_mul(10).min(99999).max(orig)
                } else {
                    orig
                };
                log!(
                    "[MOLECHEAT] 小游戏结算放大:{} {} {} → {}",
                    class,
                    sel,
                    orig,
                    boosted
                );
                env.cpu.regs_mut()[0] = boosted as u32;
                return true;
            }
        }
    }

    // Achievements shown as already unlocked. ONLY the BOOL "is in the unlocked
    // list" getters — NEVER the void checkAchieve_* methods (wrong signature ->
    // EXC_BAD_ACCESS; the original tweak hit this and backed off).
    // [2026-09-16] G-05 只保留纯显示的 -[AchievementItems unlocked:](唯一调用点 table:cellAtIndex:+0x2a6@0x319bd6)。
    //   删掉 AchievementControl / NewSceneAchievement 的 checkInAlreadyUnlockList: 两臂:这个选择子的 14 处调用全是判定入口
    //   (13 个 -[AchievementControl checkAchieve_*],加 -[NewSceneAchievement checkConditions:itemId:]@0x334a74)。以 checkAchieve_ReqLevel
    //   为例,0x1f551e 调用后返回非 0 就 cbnz 跳过,只有返回 0 才走到 0x1f5538 saveAchieveUnlockData:(记录解锁)和 0x1f5540
    //   updateInfoToServer。恒返回 1 等于开着开关期间一个新成就都不记录、不发奖,和「全成就」的字面意思正好相反。
    //   菜单标签同步改成「成就面板全亮(仅显示,不发奖)」。下面的坏档止血臂用同一个选择子,只在 SAVE_HAS_DICT_AS_ARRAY 时生效,保留不动。
    if ALL_ACHIEVE.load(O) && class == "AchievementItems" && sel == "unlocked:" {
        env.cpu.regs_mut()[0] = 1;
        return true;
    }

    // 坏档止血(P0:玩家报"批量收菜/快速连收必崩")。某些旧存档因 NSKeyedArchiver 去重
    // bug(已在 ns_keyed_archiver.rs 治本)把 UserInfoData.achieveUnlock 写成了
    // NSMutableArray;真方法 -[AchievementControl checkInAlreadyUnlockList:] 内部
    // `[achieveAlreadyUnlock allKeys]` 在数组上恒空 → 每收一颗作物都把成就重判为"未解锁"
    // → 反复达成、反复发奖(金币暴涨"多了十几万")+ 反复建奖励 UI/AVAudioPlayer → 堆耗尽
    // OOM,进程被直接杀(日志无 Rust panic)。仅在侦测到坏档时报告"已在解锁列表"以打断
    // 重复触发链。只改返回寄存器、不放行真方法、不写任何存档(零毁档风险);健康存档永不
    // 置标志,真成就逻辑照常。不碰 AchievementItems.unlocked:(纯显示,与崩溃无关)。
    if SAVE_HAS_DICT_AS_ARRAY.load(O) {
        match (class, sel) {
            ("AchievementControl", "checkInAlreadyUnlockList:")
            | ("NewSceneAchievement", "checkInAlreadyUnlockList:") => {
                env.cpu.regs_mut()[0] = 1;
                return true;
            }
            _ => {}
        }
    }

    // Currency adds: r2 holds the (signed) delta. free_shop swallows spends
    // (delta < 0); the multipliers scale gains (delta > 0).
    if class == "UserInfoData" {
        match sel {
            "addGold:" => {
                let delta = env.cpu.regs()[2] as i32;
                if FREE_SHOP.load(O) && delta < 0 {
                    env.cpu.regs_mut()[0..3].fill(0);
                    return true;
                }
                let m = GOLD_MULT.load(O);
                if m > 1 && delta > 0 {
                    env.cpu.regs_mut()[2] = delta.saturating_mul(m) as u32;
                }
            }
            "addVipGold:" => {
                let delta = env.cpu.regs()[2] as i32;
                if FREE_SHOP.load(O) && delta < 0 {
                    env.cpu.regs_mut()[0..3].fill(0);
                    return true;
                }
            }
            "addXp:" => {
                let delta = env.cpu.regs()[2] as i32;
                let m = XP_MULT.load(O);
                if m > 1 && delta > 0 {
                    env.cpu.regs_mut()[2] = delta.saturating_mul(m) as u32;
                }
            }
            _ => {}
        }
    }

    // Time-based toggles. The time getters return a double (soft-float r0:r1).
    // ★[深扫修 2026-09-11] #11 类名匹配补上 Farm 的两个子类 FlowerFarm/FruitFarm。
    //   根因:intercept 收到的 class 是接收者 isa 的运行时类名、不沿父类链(objc/messages.rs 取 read_isa)。objc_meta 实证
    //   FlowerFarm(0xaf2698)/FruitFarm(0xaf2fd0)的 superclass 都是 Farm(0xaf00a0),且都没重写 innerupdate:/getMatureTime/
    //   getWitherTime/cropWitherHandler:(全靠继承);再无其它 Farm 子类。以前只认 "Farm" → 花圃/果树永远不命中。
    //   只列这两个具体子类名(不做通用"沿父类链匹配"):后者会把 Building 等父类钩子扩散到所有子类,作用面不可控。
    if matches!(class, "Farm" | "FlowerFarm" | "FruitFarm") {
        // ★[深扫修 2026-09-11] #11「作物瞬熟」换钩子点。取证:getMatureTime 全二进制只被 -[GameData saveMapData:]
        //   (0x779f4)拿去汇总本地推送通知时间,不在玩法路径上 → 以前这个开关对【所有】地块都无效。真正的成熟判定在
        //   -[Farm innerupdate:]@0x48590 内联:elapsed = CFAbsoluteTimeGetCurrent − beginTime(Object ivar),先比
        //   elapsed ≥ matureTime+witherTime → 枯萎,再比 elapsed ≥ matureTime → cropMatureHandler。
        //   这里在真方法前把 beginTime 往前拨到"刚好过了成熟点"(见 farm_instant_mature),然后放行真方法,
        //   由原版自己走 cropStage_=4 + cropMatureHandler。纯内存读写、不发消息,寄存器零改动。
        if INSTANT_CROP.load(O) && sel == "innerupdate:" {
            farm_instant_mature(env);
            return false;
        }
        // 保留:getMatureTime 返回 0 只让 saveMapData: 跳过成熟推送时间的更新(对玩法无作用,无害)。
        if INSTANT_CROP.load(O) && sel == "getMatureTime" {
            ret_double(env, 0.0); // matured at t=0 → already ripe
            return true;
        }
        if NO_WITHER.load(O) {
            match sel {
                "getWitherTime" => {
                    ret_double(env, 1.0e15); // withers far in the future → never
                    return true;
                }
                // [深扫修 2026-09-11] #11 注意:吞之前 innerupdate: 已在 0x486b4 把自己 unschedule,吞掉后地块不写状态 5 也不再更新
                //   (已成熟则停在可收获,基本无害)。createCropForMapData: 读档路径(r2=1)被吞后地块停在哪个状态未实测,
                //   现在也覆盖花圃/果树,主控请实测一次"读档本应枯萎的花圃/果树"。
                "cropWitherHandler:" => {
                    env.cpu.regs_mut()[0..2].fill(0); // swallow the wither event
                    return true;
                }
                _ => {}
            }
        }
    }
    // [2026-09-16] G-07 建筑瞬完成补上 NewSceneShop(黄金岛商铺等)、Bridge、Ladder:三者都直接继承 Object、不是 Building 子类,
    //   各有自己的 getBuildTime:(0x31ecc8/0xd94a0/0xdfd28,与 Building 0xb07c0 同构:build_time × objectCount:type: 转浮点,返回 double)。
    //   调用点只在各自的 initWithTile:sprite:size:data:(0x31cf74/0xd8668/0xdf0a0)里,所以已经放下的建筑要重进场景才生效。
    //   CropInfoView getBuildTime: 是信息面板自己的方法,不在此列。粗筛走 intercept_wants 末尾的 INSTANT_BUILD 门控。
    if INSTANT_BUILD.load(O)
        && matches!(class, "Building" | "NewSceneShop" | "Bridge" | "Ladder")
        && sel == "getBuildTime:"
    {
        ret_double(env, 0.0);
        return true;
    }
    if NO_COOLDOWN.load(O) {
        match (class, sel) {
            ("Building", "getCurLevelCoolTime")
            | ("Building", "getLastCooldownTime")
            // [2026-09-16] G-07 特殊装饰 SpacialObject(0x14b154)与小黄鸭 YellowDuck(0x3ac42c)都直接继承 Object,各有自己的
            //   getLastCooldownTime(取 outputHanlder 的 lastCoolDownTime 时间戳,返回 double),与上面 Building 臂同一语义:
            //   上次冷却开始时刻 → 0,即早就冷却完。和 Building 臂一样,-[GameData saveMapData:](0x76d58 取这个选择子)会把 0 写进
            //   map.dat,关掉开关后这批装饰保持已冷却。不加 NewSceneRestaurant getLastCooldownTime:岛餐厅冷却已由下面的
            //   getOutCoolTime 臂覆盖,再加会经 +[NewGameManager saveTMMapDataFromObject:](0x244382 等)把 0 写进岛档。
            //   粗筛走 intercept_wants 末尾的 NO_COOLDOWN 门控。
            | ("SpacialObject", "getLastCooldownTime")
            | ("YellowDuck", "getLastCooldownTime")
            | ("Building", "getLastGameCoolTime")
            | ("NewSceneRestaurant", "getOutCoolTime")
            | ("MCNpcActor", "getCurLevelCooltime:") => {
                ret_double(env, 0.0);
                return true;
            }
            ("YaliNpcActor", "checkCooltimeOver") => {
                env.cpu.regs_mut()[0] = 1; // YES — cooldown over
                return true;
            }
            _ => {}
        }
    }

    false
}
