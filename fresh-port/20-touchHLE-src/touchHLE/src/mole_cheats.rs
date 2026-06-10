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

use crate::frameworks::core_graphics::cg_geometry::CGPoint;
use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::objc::{id, msg_send, nil, retain};
use crate::Environment;
use std::cell::Cell;
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

/// 在 run_inner 每个 yield 点调用:若帧计数 >3 秒没推进(卡死),dump 死循环现场。
pub fn watchdog_check(env: &mut Environment) {
    // ★只在岛上(进岛窗口开 / 已在岛)才看门狗。ENABLE 现已默认 ON,若仍只 gate ENABLE,
    // 主村/启动期任何正常的慢帧(首屏解码等)都会误报死循环。岛会话外一律早退。
    if !(ISLAND_ENTER_WINDOW.load(O) > 0 || ON_ISLAND.load(O)) {
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
    *V.get_or_init(|| std::env::var_os("MOLE_FIX_MAPEXTEND").is_some())
}

/// Cheap gate so the hot message path pays nothing when all cheats are off.
pub fn any_enabled() -> bool {
    if fix_mapextend_on() {
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

pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> bool {
    // 启动时 / 任一破解开关变更后,按当前开关状态把破解补丁写入或还原到模拟内存(香草基底)。
    // 写在最前面、只在 dirty 时跑一次:invalidate_cache_range 让 dynarmic 重新编译被改的指令。
    if CRACK_PATCHES_DIRTY.swap(false, O) {
        apply_crack_patches(env);
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
