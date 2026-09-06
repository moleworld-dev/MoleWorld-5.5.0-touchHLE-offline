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
use crate::objc::{id, msg_send, nil, retain, SEL};
use crate::Environment;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

const O: Ordering = Ordering::Relaxed;

/// 强制 VIP 的等级上限。游戏真实上限是 VIP10,但本移植按用户要求封顶 **VIP4**
/// (调试菜单「VIP等级」在 1..=VIP_LEVEL_MAX 循环,getVipInfoDataWithLevel: 也 clamp 到此)。
const VIP_LEVEL_MAX: i32 = 4;

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
/// 工人/空闲工人/房间数 getter 恒返回 99(收菜建造不卡人力/容量)。
static MAX_FACILITY: AtomicBool = AtomicBool::new(false);
/// 收菜结算建筑加成倍率 getter 恒返回 1000(=10倍经验/金币,走原生管线无溢出)。
static HARVEST_MULT: AtomicBool = AtomicBool::new(false);
/// 任务/催熟所需贝壳数 → 0(秒完成免费)。
static FREE_QUEST: AtomicBool = AtomicBool::new(false);
/// 海底寻宝必中稀有:generateRandomRewardId 恒返回最稀档 id(roll6-10 档 = 31169)。
static SEABED_BEST: AtomicBool = AtomicBool::new(false);
/// 小游戏奖励满:钓鱼/挖矿 getRewardCoin:/getRewardXp: 恒返回大值(类方法 hook)。
static MINIGAME_REWARD: AtomicBool = AtomicBool::new(false);
/// VIP level reported while force_vip is on (cycled 1..=VIP_LEVEL_MAX by the menu).
static VIP_LEVEL: AtomicI32 = AtomicI32::new(VIP_LEVEL_MAX);
/// Forced player level (0 = off; cycled 0/10/.../100 by the menu). Overrides the
/// curLevel getter, mirroring how FORCE_VIP overrides vipLevel.
static FORCE_LEVEL: AtomicI32 = AtomicI32::new(0);
/// All shop / collection items reported as unlocked.
static ALL_UNLOCK: AtomicBool = AtomicBool::new(false);
/// All achievements reported as already in the unlocked list.
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
static ENABLE_NEWSCENE_ISLAND: AtomicBool = AtomicBool::new(false);

/// 进岛网络门强制窗口(剩余帧数;>0 时把 NetworkManager isConnected/state/isReachable
/// 强制成"在线",**只覆盖进岛加载序列**,不污染主村离线行为)。每帧 drawScene 递减。
/// gate#1 触发时设为约 20 秒(1200 帧),足够走完飞机过场 + LoadingHoliday 全部状态。
static ISLAND_ENTER_WINDOW: AtomicI32 = AtomicI32::new(0);

/// 问题2-A:玩家当前是否在黄金岛上。★事件驱动(loadNewScene 置 true / gobackMainVillage
/// 置 false),绝不在 drawScene 每帧 msg_send 探测——那会在帧定时器栈同步跑 guest=进岛卡死。
/// 网络门在"进岛窗口内 或 在岛上"都强制在线 → 岛上周期/触摸网络检查不再弹断网框踢人,
/// 且触摸时 state==6 走正常 processTouch(否则触摸被网络检查分支吞掉)。
static ON_ISLAND: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The locally-built CaribbeanDiscoveringData (retained guest object) or nil.
    static CARIBBEAN_DATA: Cell<id> = const { Cell::new(nil) };
    /// 本次进岛是否已注入默认 mapData(每次进岛在 gate#1 reset,避免重复注入)。
    static ISLAND_INJECTED: Cell<bool> = const { Cell::new(false) };
    /// 诊断:上次记录的 LoadingHoliday curStep_,用于只在状态变化时打日志(看加载进度/卡点)。
    static ISLAND_LAST_STEP: Cell<i32> = const { Cell::new(-1) };
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
/// Diagnostic call counter for fire_online_login (logs the first few frames).
static LOGIN_DIAG: AtomicU32 = AtomicU32::new(0);
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

/// [MoleWorld iOS · P0 返回主村空村] 首次进村时 -[GameManager loadMapFromData:] 拿到的那个
/// **地图数据字典**的 guest 指针(实测 0x30017440,count=7)。返回主村时同一个指针的 count 变成 0
/// (被原地清空)→ -[GameManager loadMapFromData:selector:mapData:forNPC:] 在 0x20b16 处
/// `count==0` 早退 → 一个地图对象都不加载 → 只剩背景。记住它以便(a)追踪谁清空的、(b)拦住清空。
pub static MAPDATA_PTR: AtomicU32 = AtomicU32::new(0);
/// The HUD must NOT msg_send during the connect window (state 4/6) — doing so starved the run-loop
/// and dropped the cf_stream Open event. STATE_IS_7 (set by the changeStateTo: hook) gates HUD
/// startup to AFTER the connection is up; HUD_TIMER_SET latches a 1s self-rescheduling tick that
/// refreshes the HUD via performSelector:afterDelay: in the run-loop perform phase — never inside
/// the drawScene frame stack — so it can't interfere with packets or the village scene transition.
static STATE_IS_7: AtomicBool = AtomicBool::new(false);
static HUD_TIMER_SET: AtomicBool = AtomicBool::new(false);
/// Once the login round-trip reached state 7, drive the map request (cmd 1001) ourselves. The
/// native 1234-reply handler only sends it when MainMenuScene.isOptionLayerShow_==0 AND it reaches
/// the delegate, which our boot-synthesized flow doesn't reliably satisfy (server saw only
/// 1234→1052, never 1001). Driving getLocalUserAndMapInfo + byte_B409B0 directly is robust.
static SENT_1001: AtomicBool = AtomicBool::new(false);
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
    {
        let n = LOGIN_DIAG.fetch_add(1, O);
        if n < 4 {
            let resp_unload = env.objc.object_has_method_named(
                &env.mem,
                scene,
                "onTaomeeLoginViewDidUnloadWithUserID:password:returnCode:",
            );
            log!(
                "[MOLECHEAT] 在线诊断#{}: scene={:?} respButton={} respUnload={}",
                n,
                scene,
                resp_btn,
                resp_unload
            );
        }
    }
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

/// Phase 2 of online login: once the socket is connected (NetworkManager state==4), send the
/// login packet. didConnect (onSocket:didConnectToHost:) only sets connected=1 + state=4 — it
/// does NOT auto-send login; the game drives -[NetworkManager
/// loginWithDeviceInfoAndUserIDInfoInSendType:] separately. We set GameData.taomeeUserInfo =
/// TaomeeUserInfo{米米号, MOLE_PASSWORD} and send sendType=1 (wire commandId 0x4D2=1234; the
/// builder reads taomeeUserInfo userID+password). taomeePassword MUST be non-nil (the builder
/// does UTF8String/strlen on it) — we always set a string (empty = the 16-zero password path).
fn send_login_packet_if_connected(env: &mut Environment) {
    if LOGIN_PKT_SENT.load(O) {
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
    let state_sel = env
        .objc
        .register_host_selector("state".to_string(), &mut env.mem);
    let state: i32 = msg_send(env, (nm, state_sel));
    if state != 4 {
        return; // socket not connected yet; retry next frame
    }
    LOGIN_PKT_SENT.store(true, O);
    // GameData.taomeeUserInfo = TaomeeUserInfo{米米号, MOLE_PASSWORD}.
    let mimi = LOGIN_MIMI.load(O);
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
    let set_uid = env
        .objc
        .register_host_selector("setTaomeeUserID:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (tui, set_uid, mimi));
    let pwd_ns = crate::frameworks::foundation::ns_string::from_rust_string(env, pwd);
    let set_pwd = env
        .objc
        .register_host_selector("setTaomeePasswordOfUserID:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (tui, set_pwd, pwd_ns));
    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
    let gd: id = msg_send(env, (gd_cls, shared));
    let set_tui = env
        .objc
        .register_host_selector("setTaomeeUserInfo:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (gd, set_tui, tui));
    let rel = env
        .objc
        .register_host_selector("release".to_string(), &mut env.mem);
    let _: () = msg_send(env, (tui, rel));
    // Send the login packet (sendType 1 → taomeeUserInfo creds; commandId 1234).
    let login_sel = env.objc.register_host_selector(
        "loginWithDeviceInfoAndUserIDInfoInSendType:".to_string(),
        &mut env.mem,
    );
    let _: () = msg_send(env, (nm, login_sel, 1i32));
    log!(
        "[MOLECHEAT] 在线:已发送登录包(loginWithDeviceInfo sendType=1, 米米号={})",
        mimi
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

/// Whether "sail to finish" is on.
pub fn golden_win_on() -> bool {
    GOLDEN_WIN.load(O)
}

/// Force the Golden Island fix on (the menu's one-tap open button calls this so
/// the data getter / network short-circuits are active before showing the UI).
pub fn enable_golden_island() {
    FIX_GOLDEN_ISLAND.store(true, O);
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
        island_set_point(env, apt, "setBaseTile:", 15.0, 26.0);
        obj_set_int(env, apt, "setIsFlip:", 0);
        obj_set_int(env, apt, "setMoleNumInWaitingQueue:", 0);
        obj_set_int(env, apt, "setLastMoleFinishTrainingTime:", 0);
        island_put(env, dict, "32", apt);
    }

    let set_s = env
        .objc
        .register_host_selector("setMapData:".to_string(), &mut env.mem);
    let _: () = msg_send(env, (nsd, set_s, dict));

    // ★Bug D(火山地图碎片买了不工作)补偿:mapFragments 本应进岛时由 parseMapDataWithPackageData
    // 从 getAllObjects 回包重填,离线无回包→数组恒空→探险船永远凑不齐 4 块。直接往 NewSceneData
    // 的 mapFragments(NSMutableArray,ivar offset156)注入 4 块碎片 31005-31008(activatedAdventureMap
    // 只判这 4 槽)→ 火山探险解锁可点;出航/扣费/领奖本就全本地零发包。(一期每进岛重灌,同默认岛。)
    let frags_s = env
        .objc
        .register_host_selector("mapFragments".to_string(), &mut env.mem);
    let frags: id = msg_send(env, (nsd, frags_s));
    if frags != nil {
        let num_cls = env.objc.get_known_class("NSNumber", &mut env.mem);
        let nwi = env
            .objc
            .register_host_selector("numberWithInt:".to_string(), &mut env.mem);
        let add_s = env
            .objc
            .register_host_selector("addObject:".to_string(), &mut env.mem);
        let has_s = env
            .objc
            .register_host_selector("containsObject:".to_string(), &mut env.mem);
        for fid in [31005i32, 31006, 31007, 31008] {
            let num: id = msg_send(env, (num_cls, nwi, fid));
            let dup: bool = msg_send(env, (frags, has_s, num));
            if !dup {
                let _: () = msg_send(env, (frags, add_s, num));
            }
        }
        log!("[MOLECHEAT] island: injected 4 volcano map fragments (31005-31008)");
    }

    log!("[MOLECHEAT] island: injected default mapData (5 shops 30101-30105 / restaurant 30002 / apartment 30001)");
    true
}

/// 调试菜单「进入黄金岛(一键)」入口准备:只开启 NewScene 岛功能。随后 mole_menu 调
/// `[村庄层 enterNewIslands]` 走游戏自然进岛链——开窗(enterNewIslands hook)、异步 SUCC
/// (gate#1)、注入 mapData(getAllObjects hook)、解 state1 活锁(updateLoading hook)
/// 全部由本模块 intercept 自动接管。不要直接调 startNewSceneFrom(会绕过前置、网络门 bail)。
pub fn island_arm_entry() {
    ENABLE_NEWSCENE_ISLAND.store(true, O);
}

// 【已删除 force_gamemode_standby】曾把岛上 NewGameManager.gameMode 顶成 1(待机)以让布兰的家
// 面板不早退,但实测 gameMode=1 会暂停 cocos2d director → 整岛 freeze(NPC/动画全停)。已废弃,
// 0x1 触摸崩改由 messages.rs 底层根治,不再需要顶 gameMode。

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
/// 供 environment.rs 的调度器层冻结转储器读取。
pub fn watchdog_frame_count() -> u64 {
    WD_FRAME.load(O)
}

/// 在 run_inner 每个 yield 点调用:若帧计数 >3 秒没推进(卡死),dump 死循环现场。
pub fn watchdog_check(env: &mut Environment) {
    // [诊断·点好友卡死取证] 放开看门狗到全场景:watchdog_frame 现每帧 drawScene 无条件推进,正常帧都
    // 秒级完成、WD_FRAME 持续增长 → 只有【单帧 drawScene 卡 >3s】才会触发 dump,不会误报正常慢帧。
    // 只排除启动早期(<100 帧,首屏解码可能单帧较久)。点好友若真死循环,这里会 dump 出卡住的 PC/LR/回溯。
    if WD_FRAME.load(O) < 100 {
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
            ENABLE_NEWSCENE_ISLAND.store(!ENABLE_NEWSCENE_ISLAND.load(O), O)
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
        "enable_newscene_island" => ENABLE_NEWSCENE_ISLAND.load(O),
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
// 字节表由 vanilla vs cracked 自动 diff 生成(勿手改)。不含贝壳写死 0xb9ce0:那个由
// UserInfoData.initWithCoder hook 忠于存档处理,不在此重新强制(避免溢出)。
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

/// [MoleWorld] 在线进村存档 mapExtend 写错的修复开关。mapExtend 低5位=已扩展地图区域位掩码;
/// -[VillageLayer curVisibleArea] 取 `(unsigned __int8)mapExtend & 0x1F` 查可视区矩形。在线下发
/// 的 userinfo.mapExtend=6(只2区)却配满图内容(到 y148)→ 查到小/空可视区 → 拖动摄像机夹值
/// 震荡闪屏错位。强制 mapExtend getter 返回 0x1F(满图全区=不闪存档 287 的有效低字节)消除矛盾。
/// MOLE_FIX_MAPEXTEND=1 启用(确认阶段);确认后改默认策略。
fn fix_mapextend_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    // [MoleWorld iOS 对齐] 桌面启动器已把 MOLE_FIX_MAPEXTEND 默认置 1(强制 mapExtend=0x1F 防拖地图闪,
    // 离线无服务器修不了坏存档只能客户端兜底);iOS 没有启动器/环境变量 → 这里默认开,MOLE_FIX_MAPEXTEND=0 可关。
    *V.get_or_init(|| {
        std::env::var("MOLE_FIX_MAPEXTEND")
            .map(|v| v != "0")
            .unwrap_or(cfg!(target_os = "ios"))
    })
}

/// Cheap gate so the hot message path pays nothing when all cheats are off.
pub fn any_enabled() -> bool {
    if fix_mapextend_on() || ui43_mode() {
        return true;
    }
    FREE_SHOP.load(O)
        || KILL_ANTICHEAT.load(O)
        || FORCE_VIP.load(O)
        || GOLD_MULT.load(O) > 1
        || XP_MULT.load(O) > 1
        || INSTANT_CROP.load(O)
        || NO_WITHER.load(O)
        || NO_COOLDOWN.load(O)
        || INSTANT_BUILD.load(O)
        || FORCE_LEVEL.load(O) > 0
        || ALL_UNLOCK.load(O)
        || MAX_FACILITY.load(O)
        || HARVEST_MULT.load(O)
        || FREE_QUEST.load(O)
        || SEABED_BEST.load(O)
        || MINIGAME_REWARD.load(O)
        || ALL_ACHIEVE.load(O)
        || ENABLE_NEWSCENE_ISLAND.load(O)
        || SAVE_HAS_DICT_AS_ARRAY.load(O)
        || CRACK_PATCHES_DIRTY.load(O)
        || KILL_JAILBREAK.load(O)
        || FIX_DIVINE.load(O)
        || ENTER_HOLIDAY.load(O)
        || STORE_NO_VIP.load(O)
        || ENTER_NEWISLANDS.load(O)
        || SKIP_PARSE_CHECK.load(O)
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
/// Toggle off with MOLE_HUD=0. armv7 ObjC ABI: float args to objc_msgSend are raw f32 bit
/// patterns in core registers; CGPoint = two consecutive 32-bit slots.
fn update_debug_hud(env: &mut Environment, mimi: u32) {
    if std::env::var("MOLE_HUD").map(|v| v == "0").unwrap_or(false) {
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
        return;
    }
    let _: () = msg_send(env, (hud, set_tag, 9000i32));
    let lbl_cls = env.objc.get_known_class("CCLabelTTF", &mut env.mem);
    let font =
        crate::frameworks::foundation::ns_string::from_rust_string(env, "Times New Roman".to_string());
    let label_with = env.objc.register_host_selector(
        "labelWithString:fontName:fontSize:".to_string(),
        &mut env.mem,
    );
    let lbl: id = msg_send(env, (lbl_cls, label_with, ns_text, font, 18.0f32.to_bits()));
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
    log!("[MOLECHEAT] 调试悬浮窗已创建(MOLE_HUD=0 可关)");
}

/// [MoleWorld iOS perf · 点好友卡死根治] 单个 AnimPlayer 两次"真重建"之间的最小 host 墙钟间隔
/// (≈15fps/头像)。用【host 时间】而非 curFrame 判据 → 对解释器单帧耗时免疫(dt 死亡螺旋里
/// curFrame 每帧都变也不会让它疯狂重建)。
const ANIM_REBUILD_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(66);
/// [MoleWorld iOS perf] 单个 drawScene 帧内允许的头像"真重建"数量【硬上限】。这是防冻结的关键:
/// 无论好友村有多少头像、dt 多大,一帧最多重建这么多个,其余的沿用上一帧已建好的 sprite、留到后续
/// 帧摊销 → 保证 drawScene 必然快速返回、必然出帧,不再"永不返回=冻死"。每帧在 drawScene 入口复位。
const ANIM_REBUILD_BUDGET_PER_FRAME: u32 = 16;

thread_local! {
    /// [MoleWorld iOS perf · 点好友卡死根治] 每个 AnimPlayer 的:上次"真重建"时的动画状态快照
    /// (m_parent, curAnim, curFrame, curFlags) + 上次真重建的 host 时刻。按 AnimPlayer 指针索引。
    /// 见 [anim_render_should_skip]。
    static ANIM_RENDER_SNAP: RefCell<HashMap<u32, ((u32, u32, u32, u32), Instant)>> =
        RefCell::new(HashMap::new());
    /// 本 drawScene 帧剩余的头像重建预算(在 drawScene 入口由 [anim_render_reset_frame_budget] 复位)。
    static ANIM_REBUILD_BUDGET: Cell<u32> = const { Cell::new(ANIM_REBUILD_BUDGET_PER_FRAME) };
}

/// [MoleWorld iOS perf] 每帧(drawScene 入口)复位头像重建预算。由 objc/messages.rs 在派发
/// `-[CCDirector drawScene]` 时调用,早于本帧的 updateTick→render 遍历。
pub fn anim_render_reset_frame_budget() {
    ANIM_REBUILD_BUDGET.with(|b| b.set(ANIM_REBUILD_BUDGET_PER_FRAME));
}

/// [MoleWorld iOS perf] 跳过冗余的每帧头像 ASprite 重建(★"点好友卡死"根治)。
///
/// 真因(IDA RE 5.5.0 armv7 + 影子调用栈交叉印证):每帧 `-[AnimManager updateTick:]`(0x20f9b0)
/// 对 m_AnimInstList 里**每个** AnimInstance 无条件发 `render` → `-[AnimPlayer render]`(0x20f2f4)
/// → `-[ASprite PaintAFrame…]`(0x20c894):先 `removeAllChildrenWithCleanup:` 清空 batchNode,
/// 再 `PaintFrame` 循环为该帧每个 module 走 `PaintModule`(0x20cbf4)—— 每个 module **新建一个
/// CCSprite**(`spriteWithBatchNode:rect:isStrech:` / `spriteWithFile:…` + setContentSize/Color/
/// Opacity/Scale/Position/Flip)再 `addChild:`。好友村里几十个好友头像、每个 ASprite 十几~几十个
/// module → 每帧 alloc/init/dealloc 数百个 CCSprite + 数千次 objc_msgSend。JIT 桌面无感;**无 JIT 的
/// iOS 解释器上单帧 drawScene 永远跑不完 = 从不出帧 = present 冻结 = 点好友卡死**(桌面 on_gl2 同图
/// 正常 → 长期被误判为"原生 GLES1 渲染特有",实为解释器算力差异)。
///
/// 而绝大多数重建是**冗余**的:动画帧(curFrame)每秒才推进几次,render 却每显示帧都重建一份一模
/// 一样的 sprite。本函数返回 `true` 让 messages.rs 直接 `return` 不派发真 IMP(=跳过整次重建),`false`
/// 则放行真重建。三层判据(任一命中即跳过):
///   1. **同状态**:(m_parent,curAnim,curFrame,curFlags) 与上次真重建完全一致 → 内容不变,跳过。
///   2. **host 时间节流**:距该头像上次真重建 < [ANIM_REBUILD_MIN_INTERVAL](≈66ms/≈15fps)→ 跳过。
///      判据用【host 墙钟】而非 curFrame,故【对解释器算力免疫】:掉帧导致 dt 暴涨、curFrame 每帧都跳,
///      也不会让它每帧重建(原快照版死穴)。
///   3. **每帧硬预算**:本 drawScene 帧已重建满 [ANIM_REBUILD_BUDGET_PER_FRAME] 个 → 跳过(留到后续帧
///      摊销)。这是【防冻结的硬保证】:无论多少头像、dt 多大,单帧重建量有上限 → drawScene 必然快速返回、
///      必然出帧。首帧进好友村几十头像也不会一次性全建卡死。
/// 只有"状态变了 且 距上次重建够久 且 本帧预算未满"才真重建。跳过时沿用 batchNode 里上一次建好的
/// sprite(位移/父节点变换由 CCNode visit 处理,与子 sprite 是否重建无关)。视觉代价:真卡时头像动画
/// 降到 ≤15fps 或延后一两帧刷新(有界、自愈),换来不冻结。在线/离线皆正确,不按 network_access 门控。
///
/// AnimPlayer ivar 偏移(IDA `_OBJC_IVAR_$_AnimPlayer.*`,5.5.0):m_pause@4 curFlags@16 curAnim@24
/// curFrame@28 m_parent@64。
pub fn anim_render_should_skip(env: &mut Environment, receiver: id) -> bool {
    let base = receiver.to_bits();
    if base == 0 {
        return false;
    }
    let m_pause: u8 = env.mem.read(ConstPtr::<u8>::from_bits(base + 4));
    let cur_anim: u32 = env.mem.read(ConstPtr::<u32>::from_bits(base + 24));
    let m_parent: u32 = env.mem.read(ConstPtr::<u32>::from_bits(base + 64));
    let cur_frame: u32 = env.mem.read(ConstPtr::<u32>::from_bits(base + 28));
    let cur_flags: u32 = env.mem.read(ConstPtr::<u32>::from_bits(base + 16));
    // 镜像 -[AnimPlayer render] 自身的前置守卫:暂停 / 无动画(curAnim<0)/ 无父节点时,真 render
    // 本就只做廉价 early-return、不建任何 sprite —— 放行让它自己跑(不跳、不缓存)。
    if m_pause != 0 || (cur_anim as i32) < 0 || m_parent == 0 {
        return false;
    }
    let snap = (m_parent, cur_anim, cur_frame, cur_flags);
    let now = Instant::now();
    ANIM_RENDER_SNAP.with(|m| {
        let mut map = m.borrow_mut();
        // 跨场景累积的死指针上限保护:超阈值清空 → 后续帧各头像重建一次(无害,自愈)。
        if map.len() >= 8192 {
            map.clear();
        }
        if let Some(&(prev_snap, last_render)) = map.get(&base) {
            if prev_snap == snap {
                return true; // ① 同状态 → 跳过
            }
            if now.duration_since(last_render) < ANIM_REBUILD_MIN_INTERVAL {
                return true; // ② host 时间节流 → 跳过(不更新快照,状态仍"待重建")
            }
        }
        // 想真重建:③ 受本帧硬预算约束。
        let budget = ANIM_REBUILD_BUDGET.with(|b| b.get());
        if budget == 0 {
            return true; // 本帧预算耗尽 → 跳过,留到下一帧(不更新快照)
        }
        ANIM_REBUILD_BUDGET.with(|b| b.set(budget - 1));
        map.insert(base, (snap, now));
        false // 真重建
    })
}

/// [MoleWorld iOS · 性能] `intercept` 可能命中的**全部选择子**集合的 O(1) 快判定。
///
/// 背景:`objc_msgSend_inner` 原本对【每条消息】都把类名和选择子各堆分配一个 `String` 再交给
/// `intercept` 做一长串 `strcmp`;而 `any_enabled()` 在本移植里恒为真,于是这是每条消息的固定成本
/// (60 万消息/秒量级下相当可观)。
///
/// 选择子是**内部化**的(每个名字只有一个规范 SEL 指针),所以这里把 intercept 体内【所有像选择子的
/// 字符串字面量】(覆盖 `sel == "..."`、`matches!(sel, ...)` 及其它写法;多收无害、漏收会让钩子静默失效——
/// 2026-09-05 黄金岛卡死就是漏收了 matches! 里的选择子)一次性注册成 SEL,之后每条消息只做几十次**整数比较**;不在集合里的选择子
/// 根本不可能命中 intercept 的任何分支,可以直接跳过字符串化。行为与原来完全等价。
///
/// ★ 新增/修改 intercept 里的 `sel == "..."` 分支时,必须同步更新这里的清单
/// (否则那条 hook 会静默失效)。
pub fn is_intercept_sel(objc: &mut crate::objc::ObjC, mem: &mut crate::mem::Mem, sel: crate::objc::SEL) -> bool {
    use std::sync::OnceLock;
    static NAMES: &[&str] = &[
        "CheckUserInfoData:",
        "OnLoginOk",
        "addChild:",
        "addChild:z:",
        "addChild:z:tag:",
        "addSubview:",
        "addGold:",
        "addShopItemsObject:",
        "addVipGold:",
        "addWorker:",
        "addXp:",
        "archivedDataWithRootObject:",
        "autoLoginWithUserID:",
        "availableWorkers",
        "changeStateTo:withMessage:",
        "check",
        "checkBeyoundLeftCircleBeach:",
        "checkCooltimeOver",
        "checkInAlreadyUnlockList:",
        "checkIsUnlockMusic:",
        "checkIsVipUser",
        "checkRequiredVipLevel:",
        "checkUserinfoMd5:",
        "count",
        "cropWitherHandler:",
        "curLevel",
        "currentGameMode",
        "currentProduceMoleNums",
        "disconnect",
        "drawScene",
        "encryptCurLevel",
        "endLoadCallBack",
        "enterLoadingWithDelegate:nextSceneId:",
        "enterNewIslands",
        "entermainmenu",
        "establishConnection",
        "gameMode",
        "generateDefaultMenuView",
        "generateItemsView:",
        "generateRandomRewardId",
        "getAllObjectsListFromServerWithStartId:",
        "getBuildTime:",
        "getCurLevelCoolTime",
        "getCurLevelCooltime:",
        "getFriendsInfo",
        "getGoldSpeedUpObjectMultiple",
        "getLastCooldownTime",
        "getLastGameCoolTime",
        "getLevel",
        "getLockType4Crop:",
        "getLockType4CropWithId:",
        "getLockType4Decorate:",
        "getLockType4Gift:",
        "getLockType4Object:",
        "getLockType4ShopItem:shop:",
        "getMatureTime",
        "getNewProductsIds",
        "getOutCoolTime",
        "getRewardCoin:",
        "getRewardXp:",
        "getServerListWithServiceName:andDelegate:",
        "getShopItemsIds:",
        "getStoreItemsIdsByType:",
        "getWitherTime",
        "getXPSpeedUpObjectMultiple",
        "gobackMainVillage",
        "iMoleVillageAppDelegate",
        "inRectOfAquaticAreaOrNot:",
        "initWithItemsType:",
        "isConnected",
        "isHackData",
        "isKindOfClass:",
        "isNetworkReachable",
        "isReachable",
        "isShowVIPFunctionsButton:",
        "isUnlockedItem:",
        "loadFromLocal",
        "loadMapFromData:",
        "loadNewScene:",
        "loadObjectsDataByType:",
        "loadResourceItems",
        "loadTarget",
        "loginWithDeviceInfoAndUserIDInfoInSendType:",
        "mainLoop",
        "mapExtend",
        "moleHudTick",
        "numberOfCellsInTableView:",
        "onButtonPlaySelected:",
        "onEnter",
        "onGameDataInMainVillageUpdateSUCC",
        "onServerListResult:",
        "performSelector:withObject:afterDelay:",
        "performSelectorOnMainThread:withObject:waitUntilDone:",
        "popScene",
        "replaceScene:",
        "runWithScene:",
        "saveMapData",
        "sendAllBuffDataInNewSceneLoading",
        "sendAllBufferDatas",
        "sendPacket:commandId:",
        "setCurrentProduceMoleNums:",
        "setIsReachable:",
        "setNextScene",
        "setSocketFromStreamsAndReturnError:",
        "setUserID:",
        "sharedInstance",
        "shellsNeeded",
        "showCheatWarningMessage",
        "showDifferentGameDataComparingView",
        "showLoginView",
        "showMessageOfDisableNonHDiPhone",
        "showMultiLoginErrorMessageInNewScene",
        "showNetConnectErrorMessageWithRetryButton",
        "showNoNetConnectErrorMessage",
        "showWithTarget:",
        "showWithTarget:selector:",
        "start",
        "startGame",
        "startGame:",
        "state",
        "storeDecorationsArray",
        "table:cellAtIndex:",
        "taomeePassword",
        "totalRooms",
        "totalWorkers",
        "unlocked:",
        "update:",
        "updateGameDateForEnterNewSceneWithTarget:andCallback:",
        "updateInfoToServer",
        "updateLoading:",
        "userInfoDataInNewScene",
        "vipLevelWithNewType",
        "vipValue",
        "winSize",
    ];
    // 每个名字的规范 SEL 指针(整数),只解析一次。
    thread_local! {
        static SET: OnceLock<Vec<u32>> = const { OnceLock::new() };
    }
    SET.with(|cell| {
        let v = cell.get_or_init(|| {
            let mut v: Vec<u32> = NAMES
                .iter()
                .map(|n| objc.register_host_selector((*n).to_string(), mem).to_bits())
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        });
        // 53 项线性扫描 → 二分(~6 次比较);每条 objc 消息都走这里。
        v.binary_search(&sel.to_bits()).is_ok()
    })
}

/// [MoleWorld 宽屏适配·UI 4:3 虚拟化] 喂给白名单 UI 的原生设计尺寸(iPad landscape 4:3)。
const UI43_W: f32 = 1024.0;
const UI43_H: f32 = 768.0;
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
/// 保持真实宽度的是:世界场景与相机(VillageLayer/FriendsVillageLayer/InGameLayer/MoveLayer/CameraLayer
/// 的 checkBounding/zoom/moveToBaseTile,必须真实宽才能 Hor+ 显示更多海洋)、贴边 HUD 与菜单条
/// (VillageMenuLayer/TopMenuLayer,必须真实宽才贴得住屏幕边)、全屏画面(MainMenuScene/Logo/Loading,
/// 现已完美不动它)、世界内移动对象与飘字(Porter/GoldSprite/XPSprite…)、天气粒子(TM*/Partical/Wipe*)、
/// cocos2d 内部(CC*)。
/// 纳入 4:3 的是【多元素复杂布局】UI——不喂设计尺寸就会被 Δ=164pt 拉散(实证:商店网格散架、
/// 捉虫结算 "TOTAL" 截断、切水果卡片末项裁切):商店全套、8 类小游戏及其选关/成就面板、
/// 各节日活动弹窗、好友/礼物/任务/VIP/兑换等面板。
const UI43_CALLSITES: &[u32] = &[
    0xb468, 0xa71fe, 0xc07fa, 0xc93d0, 0xde600, 0xf2386, 0xf29fc, 0xf2b14,
    0xf2f46, 0xfbbe2, 0xfc76a, 0xfdb48, 0xfe6f4, 0xfe91c, 0x10fe60, 0x1102fc,
    0x110754, 0x110c42, 0x111952, 0x123932, 0x129e0a, 0x12d7c2, 0x134144, 0x134a86,
    0x13577e, 0x1358b6, 0x135bae, 0x136024, 0x137042, 0x1371d2, 0x1381d2, 0x13836e,
    0x138e24, 0x139e34, 0x13aab2, 0x13c338, 0x13c6e0, 0x13e318, 0x13f52e, 0x13f82a,
    0x140d86, 0x144532, 0x14cef4, 0x14e130, 0x14f94e, 0x150418, 0x152bd0, 0x156604,
    0x156916, 0x156ab2, 0x156da6, 0x158354, 0x159486, 0x159ac0, 0x164fa6, 0x165146,
    0x16641e, 0x1676d6, 0x168adc, 0x168fa0, 0x169eda, 0x16a06a, 0x16a3d2, 0x16ba8c,
    0x17176a, 0x174ade, 0x177ea2, 0x17b8ba, 0x17e12c, 0x17e51e, 0x17ea02, 0x17ec6c,
    0x17ed3a, 0x17f1ea, 0x17f37a, 0x17f66c, 0x1806c8, 0x180d98, 0x18667c, 0x188bc2,
    0x18a138, 0x18c2bc, 0x18c3e8, 0x18cc24, 0x18d1fa, 0x18e790, 0x190a2e, 0x192de0,
    0x193704, 0x193b36, 0x19c3d0, 0x1a24ec, 0x1a6754, 0x1ac820, 0x1ae7e4, 0x1af46e,
    0x1b11ec, 0x1b1c74, 0x1b2898, 0x1b40da, 0x1bb4a0, 0x1ccf1c, 0x1cf50a, 0x1d0a5c,
    0x1d2274, 0x1d33b4, 0x1d40ee, 0x1e299e, 0x1e50aa, 0x1e6206, 0x1e73d4, 0x1eb2c8,
    0x1f00a2, 0x1f21fc, 0x1f2c3c, 0x1fea1e, 0x1fffb6, 0x1fffd6, 0x1fffec, 0x200314,
    0x210a9a, 0x2126f0, 0x213060, 0x217e7e, 0x233188, 0x235e68, 0x23687a, 0x23f17e,
    0x246ce6, 0x24a802, 0x24d4b2, 0x2553ae, 0x27abfe, 0x2c0942, 0x2d9d7a, 0x2ec99a,
    0x2f68d0, 0x2f8190, 0x301562, 0x30ba98, 0x30f5d2, 0x310186, 0x3107ec, 0x318ef2,
    0x323c0c, 0x32d78e, 0x32ffea, 0x3319a2, 0x3335fe, 0x336bc4, 0x339d5a, 0x345a52,
    0x352f00, 0x3565c6, 0x358390, 0x359bae, 0x35cbfc, 0x36a260, 0x36e3c6, 0x370270,
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
    "HalloweenMainLayer", "HelpLayer", "HouseRecyclerView", "IceSummerMainLayer",
    "InviteFriendsLayer", "JunkShopLayer", "LeaveMessageLayer", "LeoAdvanceLayer",
    "Level1", "Level2", "Level3", "Level4",
    "LevelChooseLayer", "LevelUpLayer", "MagicNumberView", "MessageBox",
    "MessageBoxGift", "MessageViewController", "MessagesLayer", "MinerAchivement",
    "MinerGame", "MinerLevelChoose", "MiniBase", "MusicHallLayer",
    "NaramGetTodayRewardLayer", "NaramSpringIntroduceLayer", "NaramSpringMainLayer", "NewRewardsLayer",
    "NewSceneLevelUp", "NewSceneQuestLayer", "NewSceneTestLayer", "NewStyleStoreItemsView",
    "NewStyleStoreMainLayer", "NewStyleStoreMenuView", "NoticeBoardLayer", "OpenTreasureChestMainLayer",
    "OptionLayer", "PaintingAchivement", "PaintingGame", "PaintingLevelChoose",
    "PaybackObjectsTableLayer", "PersonalTargetLayer", "Plow", "PlowAchivement",
    "PlowLevelChoose", "PopularItemsPKAdvanceLayer", "PopularItemsPKMainLayer", "PopularItemsPKVoteLayer",
    "PromoteSalesMainLayer", "PromoteShowItemsLayer", "QiXiAdvanceLayer", "QuestLayer",
    "QuestionnaireLayer", "ReceiveGiftLayer", "RegisterView", "RequestCodeLayer",
    "RestaurantView", "RewardLayer", "SeabedSeekingTreasureExchageRewardLayer", "SeabedSeekingTreasureMainLayer",
    "SeabedSeekingTreasureRuleLayer", "SealExchangeLayer", "SeekViewController", "ShopItemsLayer",
    "ShoppingView", "ShowActivityRuleLayer", "ShowFreeShellsLayer", "ShowMoreFriendsLayer",
    "ShowRuleLayer", "SpringPoemGetRewardLayer", "SpringPoemIntroduceLayer", "SpringPoemMainLayer",
    "SpringPoemPageLayer", "TeamTargetLayer", "TestLayer", "TourLineLayer",
    "TreasureHuntPopLayer", "TreasureRewardLayer", "VIPFunctionsLayer", "VIPLayer",
    "VerifyInviteCodeLayer", "WashRoomAchievement", "WashRoomGame", "WashRoomLevelChoose",
    "WaterTowerRewardView", "XmasMainLayer",
];

/// [MoleWorld 宽屏适配·虚拟世界换算] 白名单 UI 类(含其子类,按父类链 ≤6 层)全部方法的代码地址区间
/// (已合并、升序、[start,end)),离线生成:dev-scripts 的生成器直接遍历 __objc_classlist /
/// __objc_catlist 的 class_ro_t.baseMethods 拿到 imp→类.方法 的精确归属,再用 LC_FUNCTION_STARTS
/// 截断每个方法的结尾(191 类 3306 方法 → 100 段)。
/// ★两个必须踩住的坑:①不能靠 `otool -ov` 文本行的大小写猜类名(会把 app delegate 的方法记到
/// CommonChristmasFatherGiftLayer 名下);②不能拿"下一个 imp"当方法结尾,那会把方法之间的非 ObjC
/// 代码(含 `main` @0xe890)吞进区间——宿主发消息时 LR 正是 main 里 `blx _UIApplicationMain` 的返回
/// 地址,一旦落在区间内就会把 UIKit 控件的触摸坐标也错扣 off。生成后自检:LC_FUNCTION_STARTS 里
/// 落在区间内的非白名单函数起点必须为 0。
/// 调用者 LR 落在区间内 = "白名单代码在问",此时触摸/世界坐标要按虚拟世界 ±off 换算。
const UI43_CODE_RANGES: &[(u32, u32)] = &[
    (0xb2c0, 0xe890), (0xa70fc, 0xa7eb8), (0xc06c8, 0xc5a30), (0xc92a4, 0xcbda8),
    (0xde4cc, 0xdf040), (0xf2298, 0xf32b8), (0xfbb64, 0xfbd88), (0xfc628, 0x1001d4),
    (0x10fdd0, 0x111858), (0x1118b0, 0x112234), (0x123804, 0x123d9c), (0x128844, 0x12ad80),
    (0x12d698, 0x12dda0), (0x133e4c, 0x1430c0), (0x1444a0, 0x147050), (0x14ce50, 0x151580),
    (0x152b50, 0x1596f8), (0x159a28, 0x165b38), (0x166388, 0x17d5d0), (0x17e0ac, 0x180b54),
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
    // [MoleWorld iOS 对齐] 桌面靠启动器 export MOLE_UI43=1;iOS 没有环境变量,改为【宽屏(--fill-screen
    // 算出的 guest 逻辑屏比 4:3 宽)时自动开】,MOLE_UI43=0 可关、=1 可强开。4:3 下 UI43 本就无事可做。
    *S.get_or_init(|| {
        std::env::var("MOLE_UI43")
            .map(|v| v != "0")
            .unwrap_or_else(|_| crate::window::is_widescreen())
    })
}

pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> bool {
    // 启动时 / 任一破解开关变更后,按当前开关状态把破解补丁写入或还原到模拟内存(香草基底)。
    // 写在最前面、只在 dirty 时跑一次:invalidate_cache_range 让 dynarmic 重新编译被改的指令。
    if CRACK_PATCHES_DIRTY.swap(false, O) {
        apply_crack_patches(env);
    }

    // [MoleWorld 宽屏适配·UI 4:3 虚拟化] MOLE_UI43=1:拦截 `[[CCDirector sharedDirector] winSize]`
    // 返回原生 4:3(1024x768),让【按 winSize 定位的 UI】(商店 NewStyleStoreMainLayer init 实证
    // 0x3ae612 走 msgSend_stret 调 winSize 后 setContentSize:)仍按原设计布局,不被宽 winSize 拉散。
    // ★ABI:CGSize(两个 CGFloat=f32)>4 字节 → objc_msgSend_stret,r0=返回缓冲区指针(r1=self)。
    // touchHLE 的 intercept 挂在 objc_msgSend_inner(messages.rs:260),stret 与普通 msgSend 同源,
    // 故这里直接把 8 字节写进 r0 缓冲区即可完成"返回"。
    // 现版:按调用者 LR 白名单区分——UI 类的 240 处调用点拿 4:3,世界场景相机/边界、贴边 HUD、全屏画面、
    // cocos2d 内部拿真实宽度(世界 Hor+ 不受影响)。开关见 [ui43_mode]:iOS 宽屏时自动开。
    if sel == "winSize" && ui43_mode() {
        // 调用者返回地址(Thumb blx: LR = 调用点+4+1;查表前清 Thumb 位)。
        let lr = env.cpu.regs()[14] & !1u32;
        // [MoleWorld 宽屏适配·启动第一屏「一半白一半黑」根治] cocos2d 的 winSize 是**缓存 ivar**
        // (winSizeInPoints_,写于 setOpenGLView: / reshapeProjection:)。在 touchHLE 上,guest 建
        // EAGLView 时窗口还是【竖屏】bounds(768×1669),横屏 bounds 要等视图控制器旋转后才更新;
        // 而 iMoleVillageAppDelegate 的启动序列是 setOpenGLView:(0xf5a8)→ setDeviceOrientation:
        // Portrait(0xf63e)→ runWithScene:(0xf8ba),**首个场景在旋转之前就构造完了**。
        // 实测(真机 [WINSIZE] 日志):前 5 次 winSize 返回 (768,1669) 竖屏,第 6 次起才是 (1669,768);
        // 而第 3 次的调用者正是 TaomeeLogoLayer::init(0x3c0c32)——它把背景 CCScale9Sprite 的
        // preferredSize 设成竖屏 768×1669、logo 摆到 (384,834),在横屏 1669×768 画布上就只剩左下
        // 768×768 一块白,logo 整个在屏幕上方之外 = 用户看到的「50/50 黑白」。后面的场景因为 ivar
        // 已更新所以一直是对的,所以只有【启动第一屏】坏。
        // 修法:缓存值是竖屏(高>宽)时直接返回对调后的横屏尺寸。本游戏 Info.plist 只支持横屏,
        // 竖屏 winSize 在任何时候都是错的;cocos2d 稍后把 ivar 更新成横屏后本分支自然不再命中。
        // ★纯 ivar 读,不发消息、不碰 r0–r3(见 touchhle-intercept-register-clobber)。
        // ★闩:ivar 一旦变成横屏就永不回头(游戏只支持横屏、touchHLE 运行期不旋转),之后每次
        // winSize 只付一次原子读,不再做 ivar 查找——winSize 每帧被调多次,属热路径。
        static WINSIZE_STALE: AtomicBool = AtomicBool::new(true);
        if WINSIZE_STALE.load(O) {
            let recv: id = Ptr::from_bits(env.cpu.regs()[1]);
            let cached = env
                .objc
                .object_lookup_ivar(&env.mem, recv, &"winSizeInPoints_".to_string())
                .map(|p| {
                    let f: MutPtr<f32> = p.cast();
                    (env.mem.read(f), env.mem.read(f + 1))
                });
            if let Some((cw, ch)) = cached {
                if ch > cw + 1.0 {
                    let (rw, rh) = if UI43_CALLSITES.binary_search(&lr).is_ok() {
                        (UI43_W, UI43_H)
                    } else {
                        (ch, cw)
                    };
                    let buf = env.cpu.regs()[0];
                    env.mem.write(Ptr::from_bits(buf), rw);
                    env.mem.write(Ptr::from_bits(buf + 4), rh);
                    static N: AtomicU32 = AtomicU32::new(0);
                    let n = N.fetch_add(1, O);
                    if n < 12 {
                        log!(
                            "[UI43] winSize 竖屏缓存修正 #{n} lr={lr:#x} ({cw},{ch}) → ({rw},{rh})"
                        );
                    }
                    return true;
                }
                WINSIZE_STALE.store(false, O);
            }
        }
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

    // [MoleWorld iOS · P0 修复] 离线"发包风暴"死循环根治(★点好友/进好友村卡死的真因)。
    // 离线下游戏仍调 sendPacket:commandId: 发网络包:残留缓冲/各联网界面里成百上千个包逐个发,
    // 每包都被 encodeWithCoder: 深度序列化(touchHLE 归档器每步新建 NSMutableData、去重命不中→
    // 不收敛),在一次 drawScene 的同步栈里刷成千上万次 = 永不返回 run-loop = 从不出帧(present 冻结)
    // = 整局卡死(看门狗/心跳症状 CCNode visit 0x2d30cc、FriendVillageUnit/Map 渲染同源)。
    // 已有掐断(下方)只在【进岛窗口/在岛】生效;主村点好友进好友村不在该窗口 → 风暴未被掐 → 卡死。
    // 这里把它扩到【全程离线】:离线本就发不出包(无服务器),吞掉 = 空过且根治风暴;对在线
    // (--allow-network-access)零影响(network_access 为真时不进此分支)。
    if !env.options.network_access
        && (sel == "sendPacket:commandId:"
            || sel == "sendAllBufferDatas"
            || sel == "sendAllBuffDataInNewSceneLoading")
    {
        return true;
    }

    // [MoleWorld iOS · P0 ★点好友卡死【真正根因,IDA 静态铁证 + 真机日志双证】]:
    // -[FriendsVillageLayer getFriendsInfo](点好友后 showWithParent 用 scheduleSelector 触发)在
    // isReachable==true 时:showLoadingLayer(弹 LoadingLayer 半透明遮罩 + [MBProgressHUD showHUDAddedTo:openGLView])
    // + connect2Server + [NetworkManager getFriendsInfo:/getFriendsVIPInfo:/getIsHaveNewVisitor](发好友列表请求)。
    // 那个 MBProgressHUD 加载转圈【只靠网络回包才 dismiss】。离线下回包永不到达(且请求已被上面 sendPacket 吞)→
    // 加载遮罩永不消失、UIKit HUD 盖住全屏 CAEAGLLayer → find_fullscreen_eagl_layer 返 nil → present 跌入
    // glReadPixels 慢路径 → 画面定格 = 用户看到的"点好友卡死"。★真相:guest 根本没冻,一直每帧出帧转圈
    // (真机日志 [PRESENT] 持续涨到 10496、[ANIMDT] frameDur≠0、零 [WATCHDOG] 已铁证),不是 CPU 死循环、
    // 也不是解释器指令算错。之所以"只在离线/仿佛只在 no-JIT":桌面若带 --allow-network-access 就真连服务器、
    // 回包 dismiss 加载层,故长期被误判。
    // 修:离线吞掉 getFriendsInfo(= 游戏自身"isReachable 为 false 即整段空过"的等价路径)→ 不弹会死等的加载
    // 遮罩、不发注定无回的请求 → 好友村照常显示(离线自然无好友数据)、留在全屏快路径、可正常浏览/返回,不再冻。
    // 在线(--allow-network-access)不进此分支,好友真连服务器正常拉列表,零影响。
    // 同族修复:showLoadingLayer 是好友村真正卡死的元凶——它挂 LoadingLayer 半透明遮罩 + MBProgressHUD,
    // 而 hideLoadingLayer 只在【网络回包】(onCommandReceived:/onStateChangedTo:)里触发。离线无回包 → 遮罩永驻、
    // 盖住全屏 → present 跌慢路径 → 画面定格=用户看到的"卡死"。它被 getFriendsInfo / getRandomMapdata /
    // onUnitTouched:(点好友村里的格子,含"我的村"入口)多处调用。离线吞掉它 = 一刀端掉所有路径的死等遮罩:
    // 好友村能进(onEnter 三件套全本地:setBackground 读本地 friendFront.plist + updateUnits/UI4Friends → 空村可渲染),
    // 点"我的村"格子能触发 goToHomeVillage(纯本地读档重建主村,不需网络)回到主村。离线 loading 本无意义,零副作用。
    // ★注意:这里【只吞 getFriendsInfo】,不再吞 showLoadingLayer——后者是【地图分步加载的驱动器】,
    // -[GameManager loadMapFromData:selector:mapData:forNPC:](0x2099c)在 0x20b28 处正是靠它启动
    // 回主村的加载流程。之前连它一起吞,导致"从好友村返回后:背景画了,地面/建筑/人物和村庄UI全没加载"。
    // 好友村卡死的真正修法是 ca_eagl_layer 的"跳过未聚焦小浮层"(留在全屏快路径),不需要吞加载层。
    if !env.options.network_access
        && class == "FriendsVillageLayer"
        && sel == "getFriendsInfo"
    {
        static FRIEND_CUT_LOGGED: AtomicBool = AtomicBool::new(false);
        if !FRIEND_CUT_LOGGED.swap(true, O) {
            log!(
                "[MOLECHEAT] 离线:吞掉 FriendsVillageLayer.getFriendsInfo(不发注定无回的好友请求/不弹死等遮罩)"
            );
        }
        return true;
    }

    // 注:曾在此全局吞掉 MBProgressHUD.showHUD*(为把好友村拉回快路径)。现已撤除——真正的修法是
    // ca_eagl_layer::find_fullscreen_eagl_layer 跳过未聚焦的小浮层;而全局吞 HUD 有把游戏自身加载流程
    // 一并掐断的风险(返回主村的分步加载正是由加载层驱动)。

    // ★★ 血泪教训(勿再犯):intercept 在 objc_msgSend 真正派发【之前】被调用,此刻 guest 的调用
    // 参数还活在 CPU 寄存器(r0-r3)里。若在这里对一个【打算放行(return false)】的消息做 msg_send
    // 观测(哪怕只是读个 count),host 会去跑 guest 代码,把参数寄存器冲掉 → 放行后原方法拿到垃圾参数。
    // 实测:曾在此对 loadMapFromData: 加"读 mapdata.count"的诊断 → 主村地图加载失败、整屏纯绿(HUD 正常)。
    // 规则:intercept 里做 msg_send 只允许配 `return true`(吞掉该调用);要观测放行路径,另找安全时机
    // (如帧边界、或在 host 实现的框架函数里),不要在派发前动寄存器。

    // [MoleWorld iOS · P0 返回主村空村 · 修复的一半] 记住那份【地图数据字典】的指针。
    // 首次进村走 -[GameManager loadMapFromData:](无 selector 版本),其 arg1(r2)就是完整的地图字典
    // (实测 count=7)。记下它,messages.rs 便可按【指针精确比对】吞掉后续对这一个字典的
    // removeAllObjects —— 因为 -[GameData loadMapData](0x79054)会"先清空再读 map.dat",而离线读档
    // 失败时它永不回填(失败分支 resetUserGameData 的返回值被调用方丢弃),导致返回主村时
    // -[GameManager loadMapFromData:selector:mapData:forNPC:] 在 0x20b16 命中 `count==0` 早退 →
    // 背景画了但一个 loadMapObjects 都不跑 = 只剩背景。详见 memory: moleworld-return-home-empty-solved。
    // 纯读寄存器 + host 字典 count,不发任何消息(见下方血泪教训),放行路径安全。
    if !env.options.network_access && sel == "loadMapFromData:" {
        let r2 = env.cpu.regs()[2];
        if let Some(n) = crate::frameworks::foundation::ns_dictionary::host_dict_count(
            env,
            crate::objc::id::from_bits(r2),
        ) {
            if n > 0 && MAPDATA_PTR.swap(r2, O) != r2 {
                log!("[MOLECHEAT] 记住地图数据字典 {:#x}(count={}),将保护它不被清空", r2, n);
            }
        }
    }

    // ★★ 血泪教训(勿再犯):intercept 在 objc_msgSend 真正派发【之前】被调用,此刻 guest 的调用
    // 参数还活在 CPU 寄存器(r0-r3)里。若在这里对一个【打算放行(return false)】的消息做 msg_send
    // 观测(哪怕只是读个 count),host 会去跑 guest 代码,把参数寄存器冲掉 → 放行后原方法拿到垃圾参数。
    // 实测:曾在此对 loadMapFromData: 加"读 mapdata.count"的诊断 → 主村地图加载失败、整屏纯绿(HUD 正常)。
    // 规则:intercept 里做 msg_send 只允许配 `return true`(吞掉该调用);要观测放行路径,另找安全时机
    // (如帧边界、或在 host 实现的框架函数里),不要在派发前动寄存器。

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
            env.cpu.regs_mut()[2] = mimi;
            // 落到下面:返回 false,真 setUserID: 用我们的值
        }
        // (2) 登录密码 MD5 块的明文来源:taomeePassword getter 返回 MOLE_PASSWORD。
        //     未设则不拦(空哈希,宽松服务器接受)。
        if LOGIN_ARMED.load(O) && class == "TaomeeUserInfo" && sel == "taomeePassword" {
            if let Ok(p) = std::env::var("MOLE_PASSWORD") {
                let ns = crate::frameworks::foundation::ns_string::from_rust_string(env, p);
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
        if LOGIN_ARMED.load(O) && class == "TMADataManager" && sel == "autoLoginWithUserID:" {
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
        // establishConnection 开头 `if(self->isReachable_)` 读的是 IVAR(G1 只改了方法),
        // 进入前先 [self setIsReachable:YES] 置 ivar,否则直接 bail 不连。放行真方法。
        if LOGIN_ARMED.load(O) && class == "NetworkManager" && sel == "establishConnection" {
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            let set = env
                .objc
                .register_host_selector("setIsReachable:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (nm, set, true));
            // 落到下面 -> 返回 false,真 establishConnection 用 isReachable_=1 运行
        }
        // DIAG(pass-through):暴露收 1001 后 ~30-70s 断开的真因。checkTimeOut@0xe0748 在断开前
        // 调 changeStateTo:8 withMessage:@"Time out in command: %ld";打印 state+message 即可
        // 看清是不是看门狗超时(及哪个命令),以及状态机 4→6→7→… 的真实走向。
        if class == "NetworkManager" && sel == "changeStateTo:withMessage:" {
            let state = env.cpu.regs()[2] as i32;
            // Capture LR (return address) at method entry = who called changeStateTo: — for state 8
            // (the spurious "Error connecting" disconnect) this pins the offending caller function.
            let caller_lr = env.cpu.regs()[14];
            // HUD stats: state 6 = a packet was written, state 7 = a packet was parsed.
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
            let msg_id: id = Ptr::from_bits(env.cpu.regs()[3]);
            let msg = if msg_id == nil {
                String::new()
            } else {
                crate::frameworks::foundation::ns_string::to_rust_string(env, msg_id).into_owned()
            };
            if state == 8 {
                // NOTE: do NOT suppress this state-8. Empirically, the onServerListResult: HTTP-list
                // failure → changeStateTo:8 → entermainmenu is part of the connect-RETRY flow; skipping
                // it leaves the connection unestablished. The real village blocker is downstream (the
                // LoadingLayer update:/loadTarget not re-firing for the village showWithTarget:4).
                log!(
                    "[MOLECHEAT] 在线诊断: changeStateTo:8 调用者LR={:#x} msg=\"{}\"",
                    caller_lr,
                    msg
                );
            } else {
                log!("[MOLECHEAT] 在线诊断: changeStateTo:{} msg=\"{}\"", state, msg);
            }
            return false;
        }
        if class == "NetworkManager" && sel == "disconnect" {
            log!("[MOLECHEAT] 在线诊断: NetworkManager disconnect() 被调用");
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
            log!(
                "[MOLECHEAT] 在线诊断: onServerListResult: a3={}(其虚假 state-8 由 changeStateTo 钩子按 LR 抑制)",
                env.cpu.regs()[2] as i32
            );
            return false;
        }
        // Diagnose the village render: -[LoadingLayer update:] (scheduled by showWithTarget:) is what
        // schedules loadTarget on the main thread. If it never fires after showWithTarget:4, the village
        // scene (case 4 → loadFromLocal + startGame) is never built.
        if class == "LoadingLayer" && sel == "update:" {
            // Natural update: fired → loadTarget will run via the perform queue; cancel our fallback.
            PENDING_LOADTARGET.store(0, O);
            log!("[MOLECHEAT] 在线诊断: LoadingLayer update: 触发(将投递 loadTarget)");
            return false;
        }
        // Lightweight scene/flow transition log (fires only on these rare events).
        if sel == "onButtonPlaySelected:" || sel == "OnLoginOk" || sel == "showLoginView"
            || sel == "showDifferentGameDataComparingView"
            || sel == "loadTarget" || sel == "startGame" || sel == "startGame:"
            || sel == "loadFromLocal" || sel == "entermainmenu"
            || sel == "showMessageOfDisableNonHDiPhone" || sel == "loadMapFromData:"
            || sel == "endLoadCallBack"
            || sel == "runWithScene:" || sel == "popScene"
            || sel == "setNextScene" || sel == "replaceScene:"
        {
            log!("[MOLECHEAT] 在线诊断: {} {}", class, sel);
            return false;
        }
        if sel == "showWithTarget:" {
            let tgt = env.cpu.regs()[2] as i32;
            log!("[MOLECHEAT] 在线诊断: {} showWithTarget:{}", class, tgt);
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
            if LOGIN_FIRED.load(O) {
                for _ in 0..8 {
                    crate::frameworks::core_foundation::cf_stream::drive_streams(env);
                }
            }
            // Debug HUD: do NOT refresh it from this drawScene frame stack (that starved the
            // run-loop during the connect window and killed the Open event). Instead, ONCE the
            // connection reached state 7, kick off a 1s self-rescheduling tick (performSelector:
            // afterDelay:) that refreshes the HUD entirely in the run-loop perform phase. Gated on
            // STATE_IS_7 so nothing fires during state 4/6 (the疯狂发包 connect window).
            if LOGIN_FIRED.load(O)
                && STATE_IS_7.load(O)
                && !HUD_TIMER_SET.load(O)
                && std::env::var("MOLE_HUD").map(|v| v != "0").unwrap_or(true)
            {
                HUD_TIMER_SET.store(true, O);
                schedule_hud_tick(env);
            }
            // (Removed the direct getLocalUserAndMapInfo + byte_B409B0 force — that was the "spare
            // key" shortcut. The native 1234-reply handler must request the map itself; see the
            // isOptionLayerShow_ fix below.)
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
                        log!(
                            "[MOLECHEAT] 在线:庄园地图持久化上传(saveMapData+updateInfoToServer,帧{})",
                            n
                        );
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
            // 进岛卡死(cocos2d scheduler 重入活锁)的病根。"是否在岛上" ON_ISLAND 改用事件标志:
            // loadNewScene 置 true、gobackMainVillage 置 false(见下),不在此每帧探测。
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
        // target 不像指针(<0x1000)就吞掉整条 showWithTarget:(任意类,防别的建筑面板同样的崩),
        // 作为 0x1 的最后一道防线。寄存器:self=r0, _cmd=r1, target=r2, selector=r3。
        if ON_ISLAND.load(O) && sel == "showWithTarget:selector:" {
            let target = env.cpu.regs()[2];
            if target < 0x1000 {
                log!(
                    "[MOLECHEAT] island: {} showWithTarget: 无效 target={:#x},吞掉防崩",
                    class,
                    target
                );
                return true; // 吞掉:不跑真方法 → 不会 [0x1 isKindOfClass:] → 不崩
            }
            // target 有效:直接放行真方法(gameMode 门已由 LR 收窄 hook 放行,布兰的家正常弹面板)。
        }

        // ★Bug B 续(公寓雇用按了没真出摩尔):点雇用 NewSceneApartment 走 setCurrentProduceMoleNums:(old+1)
        // 设"在产数";真摩尔靠 createInterupdate 每秒计时器等满 build_time(~3600s)才 addWorker:→
        // initMoleActors: 出来,而计时器由 onInfoViewClosed 才 schedule(布兰的家面板 LR 硬开,关闭可能
        // 不走该回调)→ 永不出。改:hook 此 setter,雇用(new>old)时【立即】对 userInfoDataInNewScene
        // addWorker:(new-old)(实测 types v12@0:4i8=收 int,内含 initMoleActors: 出可见摩尔,无发包),
        // 再把在产数压回 old(改 r2 放行真 setter)避免每秒计时器到点二次 addWorker。
        if ON_ISLAND.load(O) && class == "NewSceneApartment" && sel == "setCurrentProduceMoleNums:" {
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
                env.cpu.regs_mut()[2] = old_v as u32; // 压回在产数,放行真 setter 写 old
                return false;
            }
        }

        // ★解 state1 等服务器回包的活锁(进岛加载卡死的根因):LoadingHoliday.updateLoading
        // 的唯一停点 state1(curStep_=2)置 updatePause_=1 后发 getAllObjects 等服务器回包;
        // 离线无回包→updatePause_ 永为1→每帧入口直接 return→curStep_ 永卡 2 = 活锁。每帧在
        // 真方法执行前,若 curStep_(self+0x10,int)>=2 就强清 updatePause_(self+0xC,char)=0,
        // 让状态机靠 curStep_ 自增走完(state2 的 mapData 已注入,其余态本地无门)。放行真方法。
        if ISLAND_ENTER_WINDOW.load(O) > 0 && class == "LoadingHoliday" && sel == "updateLoading:" {
            let self_bits = env.cpu.regs()[0];
            let cur_ptr: ConstPtr<i32> = Ptr::from_bits(self_bits + 0x10);
            let cur: i32 = env.mem.read(cur_ptr);
            // 诊断:只在 curStep 变化时打一行,看加载状态机推进/卡点(2=state1 停点)。
            if cur != ISLAND_LAST_STEP.with(|c| c.get()) {
                ISLAND_LAST_STEP.with(|c| c.set(cur));
                log!("[MOLECHEAT] island: loading curStep={}", cur);
            }
            if cur >= 2 {
                let pause_ptr: MutPtr<u8> = Ptr::from_bits(self_bits + 0xc);
                env.mem.write(pause_ptr, 0u8);
            }
        }

        // 诊断里程碑 + ON_ISLAND 事件标志(纯 AtomicBool.store,无 msg_send,安全)。
        if ISLAND_ENTER_WINDOW.load(O) > 0 {
            if sel == "enterLoadingWithDelegate:nextSceneId:" {
                log!("[MOLECHEAT] island: >> enterLoading (加载场景开始)");
            } else if sel == "loadNewScene:" {
                ON_ISLAND.store(true, O); // 进岛成功:标记在岛上,网络门据此续期整个岛会话
                // ★【已回滚】曾在此 load_island_shop_atlases 补加载 4 个建筑商店图集——实测它把黄金岛
                //   渲染搞坏成全绿场地(疑这4图集的贴图在 CCTextureCache/帧缓存里覆盖/冲突了岛背景贴图)。
                //   补图集要换更安全的时机/方式(只在进建设庄园那一刻、且不覆盖岛贴图),留后续。
                log!("[MOLECHEAT] island: >> loadNewScene (建 GameNewScene),ON_ISLAND=true");
            }
        }
        // 离岛回村:gobackMainVillage 是 returnToMainVillage 真正回村的方法 → 清在岛标志,
        // 网络门停止续期,恢复主村离线行为。
        if sel == "gobackMainVillage" {
            ON_ISLAND.store(false, O);
        }

        // ★【已删除 onEnter 顶 gameMode=1】实测铁证(log 771 行 onEnter 首次真顶了 gameMode=1):
        // gameMode=1=待机 → cocos2d director 被暂停 → drawScene 仍出帧(看门狗不报)但 scheduler/
        // 动作全停 = 整岛 freeze、NPC 不动、飞机落地动画卡。gameMode=1 唯一用途是让布兰的家
        // showWithTarget: 不早退,但代价是冻结全岛=不值。点建筑 0x1 崩已由 messages.rs 底层(野指针
        // 收信者当 nil)根治,不再依赖 gameMode 顶值。故彻底删除,岛保持一键进岛后的自然 gameMode
        // (动画/NPC 正常跑)。布兰的家面板留二期(需在不冻岛的前提下另想办法)。

        // 网络门 #2/#3:进岛窗口内【或在岛上全程】把 NetworkManager 在线判定强制为真
        // (state==6=已登录)。在岛上续期是问题2 的核心:否则窗口20s过期后岛上周期/触摸
        // 网络检查恢复离线值→弹断网框→被自动「返回」踢人;且触摸需 state∈{5,6,7} 才走
        // 正常 processTouch(state=6 满足),否则触摸被网络检查分支吞掉。
        if ISLAND_ENTER_WINDOW.load(O) > 0 || ON_ISLAND.load(O) {
            match (class, sel) {
                ("NetworkManager", "isConnected") => {
                    env.cpu.regs_mut()[0] = 1;
                    return true;
                }
                ("NetworkManager", "state") => {
                    env.cpu.regs_mut()[0] = 6;
                    return true;
                }
                // ★isReachable 必须匹配【任意类】= 进岛刚需(workflow 实证):进岛链上多处
                // `[self isReachable]` 的接收者是 NetworkManager 之外的类(GameManager/VillageLayer/
                // SceneMannager/HolidayVillageLayer/NewScenePorter/NewSceneQuestLayer/LoadingHoliday 等),
                // 收窄到 NetworkManager 会让这些门判离线走偏。任意类→1 的门已收在窗口/在岛,主村空过;
                // 触摸 0x1 崩另有 showWithTarget 兜底独立挡住,不靠收窄它。
                (_, "isReachable") => {
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
                ("NewGameManager", "gameMode") if env.cpu.regs()[14] == 0x2497a9 => {
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
                // ★【已删除】曾有 (_,"archivedDataWithRootObject:") => regs[0]=0(归 nil)兜底,
                // 但实测它把【岛会话内自动存 userinfo.dat】写成了 36 字节空壳 → 下次启动 loadFromFile
                // 解档 UnexpectedEof 崩。真 storm 驱动是 sendPacket:commandId:(上面已切),这条本就多余,
                // 删除以杜绝存档损坏。存档器另在 ns_keyed_unarchiver 加容错防坏档崩启动(双保险)。
                _ => {}
            }
        }

        // 商店诊断(★无门控,运行时实锤桶到底有没有货——上一版门控在岛期、漏了启动期加载):
        // (1) addShopItemsObject 出现 N 次 = propertyHV 填了 N 件食材;0 次 = propertyHV 根本没加载。
        if class == "NewSceneData" && sel == "addShopItemsObject:" {
            log!("[MOLECHEAT] shop diag: addShopItemsObject (propertyHV 填桶 +1)");
        }
        // (2) 开店读桶:回读 NewSceneData 5 个桶 ivar(+0x20/+0x24/+0x28/+0x2c/+0x30)的 count + 本次
        //     shopId。全 0 = 桶空(propertyHV 没填,要修 touchHLE 加载/AES);[4,4,4,4,4] = 桶满、空格
        //     子是渲染/明细问题(item 图标/名/价缺)。这一行直接定论商店空格子的真因。
        if class == "NewSceneData" && sel == "getShopItemsIds:" {
            let nsd_bits = env.cpu.regs()[0];
            let shop_id = env.cpu.regs()[2] as i32;
            let cnt_s = env
                .objc
                .register_host_selector("count".to_string(), &mut env.mem);
            let mut counts = [0u32; 5];
            for (i, off) in [0x20u32, 0x24, 0x28, 0x2c, 0x30].iter().enumerate() {
                let p: ConstPtr<u32> = Ptr::from_bits(nsd_bits + *off);
                let arr: id = Ptr::from_bits(env.mem.read(p));
                if arr != nil {
                    counts[i] = msg_send(env, (arr, cnt_s));
                }
            }
            log!(
                "[MOLECHEAT] shop diag: getShopItemsIds:{} buckets={:?}",
                shop_id,
                counts
            );
        }

        // ★建设庄园(建筑商店:卖建筑/装饰/动物/趣味设施/增强道具/探险地图)诊断——实测用户点的是
        // 这套、不是餐厅食材商店(getShopItemsIds=0 证实)。日志看打开"建设庄园"时走哪些方法/类,
        // 锁定真正的加载/渲染入口(之前一直分析错成 ShopItemsLayer 食材商店了)。
        // ★实测:NewStyleStoreMainLayer.showWithTarget: + CCTableView.reloadData 都触发了=面板进了、
        // 表格重载了,但格子空=cell数=0=数据没填。问题在【点分类→数据链】。把这条链全打 + 回读 cell 数。
        if ON_ISLAND.load(O)
            && matches!(
                sel,
                "generateDefaultMenuView"
                    | "generateItemsView:"
                    | "initWithItemsType:"
                    | "loadObjectsDataByType:"
                    | "numberOfCellsInTableView:"
                    | "table:cellAtIndex:"
                    | "getNewProductsIds"
                    | "storeDecorationsArray"
                    | "loadResourceItems"
                    | "getStoreItemsIdsByType:"
            )
        {
            if sel == "numberOfCellsInTableView:" {
                // ★已移除 ivar+0x108 回读 + 嵌套 [arr count](该 re-entrant msg_send 疑似害得真方法
                //   随后崩 @0x12b;count=35 已抓到=数据非空,不再需要)。只留纯日志,零内存读。
                log!(
                    "[MOLECHEAT] buildshop diag: {}.numberOfCellsInTableView:",
                    class
                );
            } else if matches!(
                sel,
                "loadObjectsDataByType:" | "initWithItemsType:" | "getStoreItemsIdsByType:"
            ) {
                log!(
                    "[MOLECHEAT] buildshop diag: {}.{} arg={}",
                    class,
                    sel,
                    env.cpu.regs()[2] as i32
                );
            } else {
                log!("[MOLECHEAT] buildshop diag: {}.{}", class, sel);
            }
        }
        // 门2 LR 诊断:岛上 currentGameMode 的实际 LR(确认建设庄园门2 是否真=0x3aeec5;这条在网络门
        // match 之后,若我的 hook 已命中 0x3aeec5 并 return 则不会打到这——所以"打出别的 LR"=我 hook 漏了)。
        if ON_ISLAND.load(O) && class == "WrapperManager" && sel == "currentGameMode" {
            log!("[MOLECHEAT] gmdiag: currentGameMode 未被hook命中, LR={:#x}", env.cpu.regs()[14]);
        }

        // 进岛起点:一看到 enterNewIslands 就开窗 + reset 注入标志,放行原方法。开窗是为
        // 下游 startNewSceneFrom 的三道 NetworkManager 门(isReachable/isConnected/state)在
        // SUCC 帧边界执行时铺路。(注:enterNewIslands 自身真实前置门是 GameManager.gameMode
        // ∈{0,1,6} 与 SceneMannager.isChangeSceneButtonSelected==NO;它的 isReachable 已被
        // 破解版 nop 掉、不是门。)
        if sel == "enterNewIslands" {
            ISLAND_INJECTED.with(|c| c.set(false));
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
            log!("[MOLECHEAT] island: gate#1 — scheduled SUCC via perform afterDelay:0, swallowed packet");
            return true; // 吞掉发包
        }

        // state-1 向服务器拉岛物件:离线没有回包,改成本地注入默认岛 mapData,使
        // state-2(mapData.count>0)放行;吞掉发包。每次进岛只注入一次。
        if sel == "getAllObjectsListFromServerWithStartId:" && ISLAND_ENTER_WINDOW.load(O) > 0 {
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
            // (原「修2」拦 GameData getVipInfoDataOfCurrentUser 已删:它调
            //  getVipInfoDataWithLevel: 读的 vipDataDic_ 只有服务器下发才填、离线恒空 →
            //  返回 nil,既无收益又拉长链路。删掉后该方法走原版逻辑、离线返回 nil,各调用点
            //  对 nil 续发消息 nil-safe、不崩。逆向实锤崩点在 vipLevelWithNewType 的裸 int,
            //  不在此处。若日后发现个别 VIP 专属面板需要非 nil 的 VIP 配置对象,可用
            //  `[[VipInfoData alloc] init]`(游戏自带的本地 blessed 构造器 imp 0x37503c)缓存
            //  返回——但当前最小修复不需要。)
            ("UserVIPInfoData", "vipValue") => {
                env.cpu.regs_mut()[0] = 999_999; // plenty of VIP growth value
                return true;
            }
            _ => {}
        }
    }

    // Player level: override the curLevel getter (and its encrypted / scene
    // variants) exactly the way force_vip overrides vipLevel.
    if FORCE_LEVEL.load(O) > 0 {
        match (class, sel) {
            ("UserInfoData", "curLevel")
            | ("UserInfoData", "encryptCurLevel")
            | ("NewSceneData", "getLevel") => {
                env.cpu.regs_mut()[0] = FORCE_LEVEL.load(O) as u32;
                return true;
            }
            _ => {}
        }
    }

    // [MoleWorld] mapExtend 修复(见 fix_mapextend_on() 注释):在线进村存档 mapExtend=6 与满图
    // 内容不一致 → curVisibleArea/curWalkableArea/curBornArea/setBkg 算出错误可视区 → 拖动闪。
    // 强制 mapExtend getter 返回 0x1F(满图全区)。
    if fix_mapextend_on() {
        if let ("UserInfoData", "mapExtend") = (class, sel) {
            env.cpu.regs_mut()[0] = 0x1F;
            return true;
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
    if FREE_QUEST.load(O) {
        match (class, sel) {
            ("Quest", "shellsNeeded") | ("TimeQuest", "shellsNeeded") => {
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

    // 小游戏奖励满:钓鱼/挖矿小游戏的发奖 getter(类方法)恒返回大值。
    if MINIGAME_REWARD.load(O) {
        match (class, sel) {
            ("FishingGame", "getRewardCoin:")
            | ("MinerGame", "getRewardCoin:")
            | ("MinerGame", "getRewardXp:") => {
                env.cpu.regs_mut()[0] = 99999;
                return true;
            }
            _ => {}
        }
    }

    // Achievements shown as already unlocked. ONLY the BOOL "is in the unlocked
    // list" getters — NEVER the void checkAchieve_* methods (wrong signature ->
    // EXC_BAD_ACCESS; the original tweak hit this and backed off).
    if ALL_ACHIEVE.load(O) {
        match (class, sel) {
            ("AchievementControl", "checkInAlreadyUnlockList:")
            | ("NewSceneAchievement", "checkInAlreadyUnlockList:")
            | ("AchievementItems", "unlocked:") => {
                env.cpu.regs_mut()[0] = 1;
                return true;
            }
            _ => {}
        }
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
    if class == "Farm" {
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
                "cropWitherHandler:" => {
                    env.cpu.regs_mut()[0..2].fill(0); // swallow the wither event
                    return true;
                }
                _ => {}
            }
        }
    }
    if INSTANT_BUILD.load(O) && class == "Building" && sel == "getBuildTime:" {
        ret_double(env, 0.0);
        return true;
    }
    if NO_COOLDOWN.load(O) {
        match (class, sel) {
            ("Building", "getCurLevelCoolTime")
            | ("Building", "getLastCooldownTime")
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
