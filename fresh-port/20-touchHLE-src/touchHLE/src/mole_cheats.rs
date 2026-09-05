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

use crate::frameworks::core_graphics::cg_geometry::{CGPoint, CGSize};
use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::objc::{autorelease, id, msg_send, nil, release, retain, SEL};
use crate::Environment;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// [P3 商店空白真因诊断] 仅在首次强制 curSceneId 时打一行真实值(curSceneId 每帧多次读,防刷屏)。
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
    msg_send(env, (gd, pfd, fname))
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
        return false;
    }
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
fn save_island_map(env: &mut Environment) {
    let nsd_cls = env.objc.get_known_class("NewSceneData", &mut env.mem);
    if nsd_cls == nil {
        return;
    }
    let shared_s = env
        .objc
        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
    let nsd: id = msg_send(env, (nsd_cls, shared_s));
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
    let count_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let cnt: crate::mem::GuestUSize = msg_send(env, (md, count_s));
    if cnt == 0 {
        return; // 没东西可存,留默认岛兜底
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    if arch_cls == nil {
        return;
    }
    let arch_s = env
        .objc
        .register_host_selector("archivedDataWithRootObject:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (arch_cls, arch_s, md));
    if data == nil {
        return; // 归档失败,绝不写空壳坏档
    }
    let path = island_map_path(env);
    if path == nil {
        return;
    }
    let write_s = env
        .objc
        .register_host_selector("writeToFile:atomically:".to_string(), &mut env.mem);
    let ok: bool = msg_send(env, (data, write_s, path, true));
    log!("[MOLECHEAT] island: 存盘 island_map.dat(count={} ok={})", cnt, ok);
}

/// 火山碎片注入(从 build_default 抽出:持久化路径和默认路径都要保火山解锁;Phase 4 改真实获取)。
fn inject_volcano_fragments(env: &mut Environment, nsd: id) {
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
}

/// [P4-b 火山碎片持久化] 退岛把 NewSceneData.mapFragments_(玩家买到/已得的探险地图碎片 NSNumber 数组)
/// 归档存 island_fragments.dat。★为什么需要:mapFragments_ 不入 mapData 也不入 userinfo.dat(淘米设计成
/// 服务器权威 cmd addMapFragments/setModMapFragments 上行、纯内存),离线退岛即丢→玩家在建设庄园【买】的
/// 碎片(31006/31008/31009/31011,可买;addNewObject2Map→addAdventureMapFragment 本地已加)下次进岛全没。
/// 空(0 个)不写文件(避免空壳坏档),与 save_island_map 一致。
fn save_island_fragments(env: &mut Environment) {
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
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let cnt: crate::mem::GuestUSize = msg_send(env, (frags, cnt_s));
    if cnt == 0 {
        return;
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    if arch_cls == nil {
        return;
    }
    let arch_s = env
        .objc
        .register_host_selector("archivedDataWithRootObject:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (arch_cls, arch_s, frags));
    if data == nil {
        return;
    }
    let path = island_data_path(env, "island_fragments.dat");
    if path == nil {
        return;
    }
    let write_s = env
        .objc
        .register_host_selector("writeToFile:atomically:".to_string(), &mut env.mem);
    let ok: bool = msg_send(env, (data, write_s, path, true));
    log!(
        "[MOLECHEAT] island: 存盘 island_fragments.dat(碎片 count={} ok={})",
        cnt,
        ok
    );
}

/// [P4-b 火山碎片持久化] 进岛读回 island_fragments.dat 里玩家买到的碎片,逐个并入 mapFragments_
/// (containsObject 去重,与 inject_volcano_fragments 同法,不发包)。坏档/无档=静默跳过(NSKeyedUnarchiver
/// 已有数值解码容错)。★注:火山必需的 31005/31007 商店【不卖】(propertyHV 实证 shop_type=None),只能
/// 靠 inject_volcano_fragments bootstrap;故本函数只负责【恢复买到的】,火山可达仍由 inject 保证(不锁死)。
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
        return;
    }
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
    msg_send(env, (gd, pfd, f))
}

/// [P5 内容持久化命门] 黄金岛专属进度(任务/剧情/成就/扩地/建设值/NPC)淘米设计成【服务器权威+纯内存】:
/// NewSceneUserInfoData 无 NSCoding、无本地存读,saveUserinfoToLocal 存的是另一个对象(主庄园 UserInfoData)。
/// → 离线退岛即丢。这里自建 island_userinfo.dat:退岛把岛 userInfo 标量字段 + npcs(NpcData 有 NSCoding)
/// + achieveAlreadyUnlock(标准 NSMutableDict)塞进一个 dict 整体 NSKeyedArchiver 归档落盘。
fn save_island_userinfo(env: &mut Environment) {
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
    let ui_s = env
        .objc
        .register_host_selector("userInfoDataInNewScene".to_string(), &mut env.mem);
    let ui: id = msg_send(env, (nsd, ui_s));
    if ui == nil {
        return;
    }
    let dict = island_alloc_init(env, "NSMutableDictionary");
    if dict == nil {
        return;
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
        let k = crate::frameworks::foundation::ns_string::from_rust_string(env, key.to_string());
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
        let k = crate::frameworks::foundation::ns_string::from_rust_string(
            env,
            "curQuestResult".to_string(),
        );
        let _: () = msg_send(env, (dict, sfk, num, k));
    }
    // 对象字段 npcs(NSMutableArray<NpcData>)/ achieveAlreadyUnlock(NSMutableDict)整体入 dict,
    // 随 NSKeyedArchiver 递归归档(NpcData 有 encodeWithCoder、字典 keyed-archive 往返已支持)。
    for key in ["npcs", "achieveAlreadyUnlock"] {
        let g = env.objc.register_host_selector(key.to_string(), &mut env.mem);
        let o: id = msg_send(env, (ui, g));
        if o != nil {
            let k =
                crate::frameworks::foundation::ns_string::from_rust_string(env, key.to_string());
            let _: () = msg_send(env, (dict, sfk, o, k));
        }
    }
    let arch_cls = env.objc.get_known_class("NSKeyedArchiver", &mut env.mem);
    if arch_cls == nil {
        return;
    }
    let arch_s = env
        .objc
        .register_host_selector("archivedDataWithRootObject:".to_string(), &mut env.mem);
    let data: id = msg_send(env, (arch_cls, arch_s, dict));
    if data == nil {
        return;
    }
    let path = island_data_path(env, "island_userinfo.dat");
    if path == nil {
        return;
    }
    let write_s = env
        .objc
        .register_host_selector("writeToFile:atomically:".to_string(), &mut env.mem);
    let ok: bool = msg_send(env, (data, write_s, path, true));
    log!("[MOLECHEAT] island: 存盘 island_userinfo.dat(任务/剧情/成就/扩地 ok={})", ok);
}

/// [P5] 进岛读回 island_userinfo.dat,覆盖到岛 userInfo(在 server-fed/默认值之后、渲染之前)。
fn load_island_userinfo(env: &mut Environment) {
    let path = island_data_path(env, "island_userinfo.dat");
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
    let dict: id = msg_send(env, (unarch_cls, unarch_s, path));
    if dict == nil {
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
    let ui_s = env
        .objc
        .register_host_selector("userInfoDataInNewScene".to_string(), &mut env.mem);
    let ui: id = msg_send(env, (nsd, ui_s));
    if ui == nil {
        return;
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
        ("setCurIdleWorkerCount:", "curIdleWorkerCount"),
    ] {
        let k = crate::frameworks::foundation::ns_string::from_rust_string(env, key.to_string());
        let num: id = msg_send(env, (dict, ofk, k));
        if num != nil {
            let v: i32 = msg_send(env, (num, iv));
            let s = env.objc.register_host_selector(setter.to_string(), &mut env.mem);
            let _: () = msg_send(env, (ui, s, v));
        }
    }
    {
        let k = crate::frameworks::foundation::ns_string::from_rust_string(
            env,
            "curQuestResult".to_string(),
        );
        let num: id = msg_send(env, (dict, ofk, k));
        if num != nil {
            let dv = env
                .objc
                .register_host_selector("doubleValue".to_string(), &mut env.mem);
            let v: f64 = msg_send(env, (num, dv));
            let s = env
                .objc
                .register_host_selector("setCurQuestResult:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (ui, s, v));
        }
    }
    for (setter, key) in [
        ("setNpcs:", "npcs"),
        ("setAchieveAlreadyUnlock:", "achieveAlreadyUnlock"),
    ] {
        let k = crate::frameworks::foundation::ns_string::from_rust_string(env, key.to_string());
        let o: id = msg_send(env, (dict, ofk, k));
        if o != nil {
            let s = env.objc.register_host_selector(setter.to_string(), &mut env.mem);
            let _: () = msg_send(env, (ui, s, o));
        }
    }
    log!("[MOLECHEAT] island: 读回 island_userinfo.dat(任务/剧情/成就/扩地进度恢复)");
}

/// [P2b] 快照 TMMapData → mapData 的类型 key。★最具体子类优先(餐厅/公寓/咖啡馆/船/超级贝壳树
/// 继承自 TMMapDataShop,必须先判,否则全被误判成 28)。核心经营类=28商店/29餐厅/32公寓/39船/41咖啡馆。
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
    let key = match island_class_to_key(env, snap) {
        Some(k) => k,
        None => return,
    };
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
    let keystr = crate::frameworks::foundation::ns_string::from_rust_string(env, key.to_string());
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
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let n: crate::mem::GuestUSize = msg_send(env, (arr, cnt_s));
    let oai = env
        .objc
        .register_host_selector("objectAtIndex:".to_string(), &mut env.mem);
    let mut found: i64 = -1;
    for i in 0..n {
        let old: id = msg_send(env, (arr, oai, i));
        let oseq: i32 = msg_send(env, (old, seq_s));
        if oseq == seqid {
            found = i as i64;
            break;
        }
    }
    if found >= 0 {
        let rep = env.objc.register_host_selector(
            "replaceObjectAtIndex:withObject:".to_string(),
            &mut env.mem,
        );
        let _: () = msg_send(env, (arr, rep, found as crate::mem::GuestUSize, snap));
    } else {
        let add = env
            .objc
            .register_host_selector("addObject:".to_string(), &mut env.mem);
        let _: () = msg_send(env, (arr, add, snap));
    }
    // [观测性] 经营态写回锚点:升级/公寓雇用/出海返航等都经此把快照按 seqId 回写进 mapData[key]。
    // key=="39" 即探险船(航海状态持久化命门,原本全程静默无法看日志确认);按动作触发非每帧,不刷屏。
    log!(
        "[MOLECHEAT] island: 经营态写回 mapData[key={}] seqId={} ({})",
        key,
        seqid,
        if found >= 0 { "replace" } else { "add" }
    );
}

/// [P3-a 新建筑放置持久化] 退岛前把活对象表(ObjectManager.objects)里【尚未在 mapData 的新放置建筑】
/// 并入 [NewSceneData mapData],使随后 save_island_map 把新买的建筑一起存盘。否则:原版退岛【不】整体
/// 存岛布局(saveUserinfoToLocal 只存 userinfo.dat),会话期放建筑只进 ObjectManager.objects + 发送缓冲、
/// 【不回写活 mapData_】(workflow B/C 路实证)→ save_island_map 存的 mapData 只有进岛初态+经营回写、漏新放置。
/// C 路又证:ObjectManager 全局单例但切场景整体清空重填→任意时刻只含当前场景对象→在 gobackMainVillage
/// (真方法里 startNewSceneFrom/unloadMap【之前】,本 hook 是 pre-method 故安全)枚举安全。
/// ★安全:全 additive(只 add,不删 mapData 已有项=零数据丢失)+ 全程 nil-guard + snap==nil(Firework 等)
///   或 seqId==0(未分配)跳过 + seqId 去重(种子/经营回写已存的不重复加)。key 用活对象 type 字符串
///   (=loadMapObjects 读 mapData 的 key,saveTMMapDataFromObject: 实证 type40→SuperShellTree/type5→Spacials),
///   island_class_to_key 精确6类优先;mis-key 最坏=该项 reload 时不被读(同现状,不崩不污染别项)。
/// ⚠️已知局限:新建筑 seqId 来自 NewSceneCommand.currentMaxSequenceId_ 本地自增,跨会话重启归0→第二次
///   进岛新放置 seqId 可能与上次持久的撞号(去重会误判)。首轮持久化够用;跨会话游标恢复(load 后置
///   currentMaxSequenceId_=max(已存 seqId))留后续。
fn merge_new_island_objects_into_mapdata(env: &mut Environment) {
    let om_cls = env.objc.get_known_class("ObjectManager", &mut env.mem);
    if om_cls == nil {
        return;
    }
    let sm = env
        .objc
        .register_host_selector("sharedManager".to_string(), &mut env.mem);
    let om: id = msg_send(env, (om_cls, sm));
    if om == nil {
        return;
    }
    let objs_s = env
        .objc
        .register_host_selector("objects".to_string(), &mut env.mem);
    let objs: id = msg_send(env, (om, objs_s));
    if objs == nil {
        return;
    }
    let av_s = env
        .objc
        .register_host_selector("allValues".to_string(), &mut env.mem);
    let all: id = msg_send(env, (objs, av_s));
    if all == nil {
        return;
    }
    let cnt_s = env
        .objc
        .register_host_selector("count".to_string(), &mut env.mem);
    let n: crate::mem::GuestUSize = msg_send(env, (all, cnt_s));
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
    let md_s = env
        .objc
        .register_host_selector("mapData".to_string(), &mut env.mem);
    let md: id = msg_send(env, (nsd, md_s));
    if md == nil {
        return;
    }
    let ngm_cls = env.objc.get_known_class("NewGameManager", &mut env.mem);
    if ngm_cls == nil {
        return;
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
                continue;
            }
            let _: () = msg_send(env, (md, sfk, arr, keystr));
        }
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
        log!(
            "[MOLECHEAT] island: 退岛合并 {} 个新放置建筑进 mapData(持久化)",
            merged
        );
    }
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
    // [P5 内容持久化] 读回岛专属进度(任务/剧情/成就/扩地/建设值/NPC),覆盖到载体上。
    load_island_userinfo(env);
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
            let seed = if gmode > 0 { gmode } else { 1 };
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
    // [P1 离线持久化] 先试读 island_map.dat;读到有效布局就用它、跳过默认岛注入(火山碎片仍补)。
    if load_island_map(env) {
        load_island_fragments(env); // [P4-b] 先恢复玩家买到的碎片
        inject_volcano_fragments(env, nsd); // 再 bootstrap 火山必需(31005/31007 不可买),去重不覆盖
        restore_seqid_cursor(env); // [P3-a] 抬 seqId 游标到已存最大,防新放置撞号
        return true;
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

    // ★Bug D(火山碎片)补偿:mapFragments 离线无回包→恒空→探险船凑不齐;注入 4 块 31005-31008
    // (activatedAdventureMap 只判这 4 槽)。已抽成 inject_volcano_fragments,持久化路径也复用。
    load_island_fragments(env); // [P4-b] 先恢复玩家买到的碎片(默认岛首进通常无,空过)
    inject_volcano_fragments(env, nsd); // 再 bootstrap 火山必需(31005/31007 不可买)
    restore_seqid_cursor(env); // [P3-a] 默认岛种子 seqId 90001-90008,抬游标到 90008 防新放置撞号

    log!("[MOLECHEAT] island: injected default mapData (5 shops 30101-30105 / restaurant 30002 / apartment 30001 / ship 34001)");
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

/// 热路径粗筛(P0-B):这条消息的 class 或【不绑定 class 的】sel 是否【可能】被 intercept() 命中。
/// 游戏每帧约 16000 次 objc_msgSend 都过这里(any_enabled() 恒真),让 99% 不相关的消息在进
/// intercept(及其两次 to_string 堆分配 + 长比较链)之前就 return false。命中的少数才付出代价。
///
/// ⚠️【不变量——改 intercept() 时必须同步维护,漏一个 → release 下那个 hook 静默失效、破坏游戏】:
///   · CLASSES 必须含 intercept() 里每一个 `class == "X"` 与 `match (class,sel)` 臂里的 X;
///   · SELS 必须含每一个【不绑定具体 class】的 sel(裸 `if sel == "Y"`、`(_, "Y")` 通配臂)。
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
    ) || matches!(
        sel,
        "drawScene"
            | "mainLoop"
            | "moleHudTick"
            | "showWithTarget:"
            | "showWithTarget:selector:"
            | "checkPromptForLoadingNewApp" // [去广告] 赛尔号跨游戏广告弹窗触发器(GameManager)
            | "getMoleCartAdImageFromServer" // [去广告] AdViewForMoleCart 拉广告图入口(兜底拦截)
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
            | "enterNewIslands"
            | "getAllObjectsListFromServerWithStartId:"
            | "getMatureTime"
            | "isReachable"
            | "winSize" // [宽屏适配·UI 4:3 虚拟化] MOLE_UI43=1 时返回 1024x768(见 intercept)
            | "onEnter" // [宽屏适配·居中偏移] MOLE_UI43=1 时白名单 UI 根层进场整体右移居中
            | "addChild:" // [宽屏适配·居中偏移] 已处理根层的迟到子节点当场居中/拉伸
            | "addChild:z:"
            | "addChild:z:tag:"
            | "sendPacket:commandId:"
            | "sendAllBufferDatas"
            | "sendAllBuffDataInNewSceneLoading"
            | "generateRandomRewardId"
            | "onTaomeeLoginViewDidUnloadWithUserID:password:returnCode:"
            // [P3 商店空白真因] -[SceneMannager curSceneId]:离线进岛后常卡在过场态 2(非10),
            //   loadObjectsDataByType: 据它选数据源→返回空→建设庄园/食材店空格。在岛上强制 10。
            | "curSceneId"
    )
}

/// [MoleWorld 宽屏适配·UI 4:3 虚拟化] 喂给白名单 UI 的原生设计尺寸(iPad landscape 4:3)。
const UI43_W: f32 = 1024.0;
const UI43_H: f32 = 768.0;
/// [MoleWorld 宽屏适配·居中偏移] "根层已处理"标记:处理后把根层 contentSize.width 设为 真实宽+0.5。
/// 为什么不能用"宽==真实宽":CCLayer 基类 init 自带 contentSize=winSize,而 cocos2d 内部类不在白名单
/// → 拿到真实宽 1188,任何没自己 setContentSize: 的 UI 层一出生就是 1188,会被误判"已处理"
/// (实证:NewStyleStoreMenuView 构造期被当已就绪根层处理子节点,挂树后又整体 +82 → 2×;
/// QuestLayer/RewardLayer 等弹窗 onEnter 被误判 done 直接跳过没居中)。
/// 小数 .5 是对象自身特征,不受地址复用影响;CCLayerColor 色块宽 0.5px 差异不可见。
const UI43_MARK: f32 = 0.5;
fn ui43_marked(w: f32, real_w: f32) -> bool {
    (w - (real_w + UI43_MARK)).abs() < 0.01
}
/// [MoleWorld 宽屏适配·居中偏移] 已处理过的 (根层指针, 子节点指针)。同一根层会话内同一子节点只平移一次——
/// 既挡 cocos2d addChild: → addChild:z: → addChild:z:tag: 调用链的连续重复触发,也挡"同一对象被
/// removeChild 后再 addChild"(商店刷新物品区就是这样:加 A、加 B、再重新加 A)。根层重新进场
/// (contentSize 还是 1024 = 新会话)时清掉该根层的记录;超过上限整体清空防止无限增长。
/// ★锁绝不跨 msg_send 持有(msg_send → intercept → 本 hook 会重入 → 死锁)。
static UI43_DONE: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());
fn ui43_done_mark(root: u32, child: u32) -> bool {
    // 返回 true = 首次(已记录,可处理);false = 已处理过,跳过。
    let mut d = UI43_DONE.lock().unwrap();
    if d.iter().any(|&(r, c)| r == root && c == child) {
        return false;
    }
    if d.len() > 4096 {
        d.clear();
    }
    d.push((root, child));
    true
}
fn ui43_done_reset_root(root: u32) {
    UI43_DONE.lock().unwrap().retain(|&(r, _)| r != root);
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
    set_cs: SEL,
    ap: SEL,
    sx: SEL,
    set_sx: SEL,
    children: SEL,
    count: SEL,
    oai: SEL,
    parent: SEL,
}
fn ui43_sels(env: &mut Environment) -> Ui43Sels {
    let mut r = |n: &str| env.objc.register_host_selector(n.to_string(), &mut env.mem);
    Ui43Sels {
        pos: r("position"),
        set_pos: r("setPosition:"),
        cs: r("contentSize"),
        set_cs: r("setContentSize:"),
        ap: r("anchorPoint"),
        sx: r("scaleX"),
        set_sx: r("setScaleX:"),
        children: r("children"),
        count: r("count"),
        oai: r("objectAtIndex:"),
        parent: r("parent"),
    }
}

/// [MoleWorld 宽屏适配·诊断] MOLE_UI43_DEBUG=1 才输出 [UI43] 逐节点日志(默认关:正常游玩一次
/// 会产生上百行,会把真正有价值的告警冲掉;排查布局问题时再开)。
fn ui43_debug() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("MOLE_UI43_DEBUG").map(|v| v != "0").unwrap_or(false))
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

/// [MoleWorld 宽屏适配·居中偏移] 处理"UI 根层的一个直接子节点":
///   · 非白名单、且是 **无子节点的叶子** CCSprite/CCLayerColor、且有效宽 ≥900 = 全宽背景 → `setScaleX:` 横向
///     拉伸到真实宽并按 anchorPoint 归位(木纹/面板底图拉 16% 肉眼不可见);
///   · 其余(按钮/表格/文字/白名单子层等一切容器)→ 只 +offset 平移居中,绝不拉伸(防表格/子层内容变形)。
fn ui43_process_child(env: &mut Environment, ch: id, off: f32, real_w: f32, s: &Ui43Sels) {
    if ch == nil {
        return;
    }
    let pos: CGPoint = msg_send(env, (ch, s.pos));
    let whitelisted = ui43_class_hit(env, ch);
    let cname = ui43_cls_name(env, ch);
    if !whitelisted && (ui43_is_kind(env, ch, "CCSprite") || ui43_is_kind(env, ch, "CCLayerColor")) {
        let cs: CGSize = msg_send(env, (ch, s.cs));
        let sx: f32 = msg_send(env, (ch, s.sx));
        let kids: id = msg_send(env, (ch, s.children));
        let nkids: crate::mem::GuestUSize = if kids == nil {
            0
        } else {
            msg_send(env, (kids, s.count))
        };
        if cs.width * sx >= 900.0 && cs.width > 1.0 && nkids == 0 {
            let ap: CGPoint = msg_send(env, (ch, s.ap));
            let _: () = msg_send(env, (ch, s.set_sx, real_w / cs.width));
            let _: () = msg_send(env, (ch, s.set_pos, CGPoint { x: ap.x * real_w, y: pos.y }));
            let (cw, px, py, apx) = (cs.width, pos.x, pos.y, ap.x);
            if ui43_debug() { log!(
                "[UI43]     child {} STRETCH w={} sx={}→{} pos=({},{})→({},{})",
                cname, cw, sx, real_w / cw, px, py, apx * real_w, py
            ); }
            return;
        }
    }
    let _: () = msg_send(env, (ch, s.set_pos, CGPoint { x: pos.x + off, y: pos.y }));
    let (px, py) = (pos.x, pos.y);
    if ui43_debug() { log!(
        "[UI43]     child {} OFFSET wl={} pos=({},{})→({},{})",
        cname, whitelisted, px, py, px + off, py
    ); }
}

/// [MoleWorld 宽屏适配·居中偏移] `onEnter` 拦截:白名单 UI **根层**(父节点不在白名单)进场时做
/// "4:3 虚拟窗口居中 + 底铺满"。
///
/// 商店实证结构(NewStyleStoreMainLayer init 反汇编):根层是 **CCLayerColor**(`initWithColor:` +
/// `setContentSize:winSize` = 纯色遮罩底),子节点有 `storeback.png`(1024 宽顶部木条)、`storeBackBoard.png`
/// (游戏自己 `setScaleX:` 拉伸的背板)、按钮/表格等;**物品区 ItemsView 与底部详情面板是 onEnter 之后才
/// `addChild` 进来的**(showItemsViewWithType:)。结尾还调 `scaleImageForIPhone5:` ×2 +
/// `adjustPositionForIPhone5:`——淘米当年就是靠"拉伸背景图 + 平移子节点"适配 iPhone5,只是 iPad 被
/// `isIpad` 门挡死。本模块 = 它的通用复刻:
///   ① 根层**不动位置**,contentSize 拉到真实宽 → 纯色底铺满整屏;
///   ② 进场时已有的子节点逐个交给 [ui43_process_child](拉伸背景 / 平移其余);
///   ③ **之后再 addChild 进来的子节点由 [ui43_on_add_child] 当场处理**(接住迟到的 ItemsView/详情面板)。
/// 幂等:处理后根层 contentSize.width==真实宽,再次 onEnter 直接跳过;嵌套的白名单子层(父在白名单)
/// 在自己的 onEnter 什么都不做——它已作为父层的 child 被平移过,其内部子节点随之整体移动,不再单独处理。
/// msg_send 会 clobber r0–r3,本拦截在真方法之前、之后放行,故保存/恢复。
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
    let saved = [
        env.cpu.regs()[0],
        env.cpu.regs()[1],
        env.cpu.regs()[2],
        env.cpu.regs()[3],
    ];
    let s = ui43_sels(env);
    let parent: id = msg_send(env, (recv, s.parent));
    let self_pos: CGPoint = msg_send(env, (recv, s.pos));
    let root_cs0: CGSize = msg_send(env, (recv, s.cs));
    let pwl = ui43_class_hit(env, parent);
    let (spx, spy, cw0, ch0) = (self_pos.x, self_pos.y, root_cs0.width, root_cs0.height);
    let marked = ui43_marked(cw0, real_w);
    if ui43_debug() { log!(
        "[UI43] onEnter {} @{:#x} parent={} pos=({},{}) cs=({},{}) → {}",
        ui43_cls_name(env, recv), recv.to_bits(), ui43_cls_name(env, parent),
        spx, spy, cw0, ch0,
        if pwl { "NESTED(skip)" } else if marked { "ROOT(done)" } else { "ROOT-PASS" }
    ); }
    if !pwl {
        let root_cs: CGSize = msg_send(env, (recv, s.cs));
        if !marked {
            let rb = recv.to_bits();
            ui43_done_reset_root(rb); // 新会话:清掉该根层的旧记录
            let h = if root_cs.height > 1.0 { root_cs.height } else { UI43_H };
            // 宽设为 真实宽+0.5 = 同时完成"底铺满"与"已处理"标记。
            let _: () = msg_send(env, (recv, s.set_cs, CGSize { width: real_w + UI43_MARK, height: h }));
            let children: id = msg_send(env, (recv, s.children));
            if children != nil {
                let n: crate::mem::GuestUSize = msg_send(env, (children, s.count));
                for i in 0..n {
                    let ch: id = msg_send(env, (children, s.oai, i));
                    if ch != nil && ui43_done_mark(rb, ch.to_bits()) {
                        ui43_process_child(env, ch, off, real_w, &s);
                    }
                }
            }
        }
    }
    for (i, v) in saved.iter().enumerate() {
        env.cpu.regs_mut()[i] = *v;
    }
}

/// [MoleWorld 宽屏适配·居中偏移] `addChild:` / `addChild:z:` / `addChild:z:tag:` 拦截(r0=父, r2=子):
/// 父是**已处理过的 UI 根层**(白名单 + 父之父不在白名单 + contentSize 已==真实宽)→ 对迟到的新子节点
/// 当场做 [ui43_process_child]。根层 init 期间的 addChild(contentSize 还是 1024)自然跳过,留给 onEnter 一并处理。
fn ui43_on_add_child(env: &mut Environment) {
    let recv: id = Ptr::from_bits(env.cpu.regs()[0]);
    let child: id = Ptr::from_bits(env.cpu.regs()[2]);
    if child == nil || !ui43_class_hit(env, recv) {
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
    let parent: id = msg_send(env, (recv, s.parent));
    let root_cs: CGSize = msg_send(env, (recv, s.cs));
    let attached = parent != nil; // 构造期(未挂树)一律不动:子节点由 onEnter 根层遍历或作为整体被父层平移
    let pwl = ui43_class_hit(env, parent);
    let cw = root_cs.width;
    let ready = ui43_marked(cw, real_w);
    let fresh = if attached && !pwl && ready { ui43_done_mark(recv.to_bits(), child.to_bits()) } else { false };
    if ui43_debug() { log!(
        "[UI43] addChild {} ← {} @{:#x} (recv.parent={} cs.w={}) → {}",
        ui43_cls_name(env, recv), ui43_cls_name(env, child), child.to_bits(),
        ui43_cls_name(env, parent), cw,
        if !attached { "recv-detached(skip)" } else if pwl { "recv-is-nested(skip)" } else if !ready { "root-not-ready(skip)" } else if !fresh { "dup(skip)" } else { "PROCESS" }
    ); }
    if attached && !pwl && ready && fresh {
        ui43_process_child(env, child, off, real_w, &s);
    }
    for (i, v) in saved.iter().enumerate() {
        env.cpu.regs_mut()[i] = *v;
    }
}

/// [MoleWorld 宽屏适配·UI 4:3 虚拟化] MOLE_UI43=1 是否开启(winSize 返回 1024x768)。仅解析一次。
fn ui43_mode() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("MOLE_UI43").map(|v| v != "0").unwrap_or(false))
}

pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> bool {
    // ★[2026-06-22 飞机进岛卡死修复] 离线黄金岛总开关 ENABLE_NEWSCENE_ISLAND 默认 ON(飞机/作弊菜单
    // 两条进岛路径等价)。仅【在线模式】(--allow-network-access)强制 OFF——在线下岛 hook(网络门强制
    // 在线/吞包/解活锁)会干扰私服真连接,且在线岛非功能点;离线(默认)保持 ON,飞机点击即进岛。
    // 注:主村期间岛 hook 本就空过(网络门 gated ISLAND_ENTER_WINDOW||ON_ISLAND),此处只为在线模式
    // 额外保险关掉总闸,确保你的服务器/在线工作零干扰。
    if env.options.network_access {
        ENABLE_NEWSCENE_ISLAND.store(false, O);
    }
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
    // 【本版=最小验证】无条件全局 4:3(世界场景也会退回 4:3,失去 Hor+),仅用于验证"UI 是否因此归位";
    // 验证通过后改为按调用者 LR/类白名单区分(世界场景 VillageLayer/MoveLayer/CameraLayer 等返回真实
    // 宽尺寸,UI 类返回 4:3)。默认关(未设 env)=零影响。
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
    // [MoleWorld 宽屏适配·居中偏移] 已处理根层收到迟到子节点 → 当场居中/拉伸(见 ui43_on_add_child)。
    if ui43_mode() && (sel == "addChild:" || sel == "addChild:z:" || sel == "addChild:z:tag:") {
        ui43_on_add_child(env);
        return false;
    }

    // [MoleWorld 去广告] 淘米跨游戏广告弹窗 AdViewForMoleCart(如"赛尔号:王者归来 / 立即参战")。
    // 实测:它【不】走 showWithTarget(那条没命中过),而是 -[GameManager checkPromptForLoadingNewApp]
    // 触发 → getMoleCartAdImageFromServer → onImageRecieved → 直接 addChild 上屏(有 defaultAdImage 兜底,
    // 本端 HTTP 已 drop 也照弹)。所以正确的拦点是【触发器本身】:掐掉 checkPromptForLoadingNewApp,
    // 整条广告流程不启动。按 selector 收窄,不影响别的类。
    // 诊断:记录拉图入口是否被调用(确认 banner 确实走 AdView 这条路)。
    if sel == "getMoleCartAdImageFromServer" {
        log!("[MOLECHEAT] 去广告诊断:{class}.getMoleCartAdImageFromServer 被调用(AdView 广告流程在跑)");
    }
    // ★将来接私服「自定义公告推送」:这里改成——不 return,而是放行/改喂我们后台的 PNG;现在=纯 ban。
    if sel == "checkPromptForLoadingNewApp"
        || (class == "AdViewForMoleCart" && (sel == "showWithTarget:selector:" || sel == "showWithTarget:"))
    {
        log!("[MOLECHEAT] 去广告:吞掉 {class} {sel}(淘米跨游戏广告/赛尔号弹窗触发器)");
        return true; // handled —— 跳过真方法,广告不展示
    }
    // [去广告·真凶] 淘米「更多游戏」跨游戏推荐弹窗(赛尔号/摩尔卡丁车整屏弹窗):直接吞掉它的展示方法
    // showMoreGame*(OnRootView/WithScale/WithUrl)。无论推荐数据从哪来,整屏弹窗都不再展示。
    // 比 fake SDK 数据类干净(fake 数据反而可能弹"无游戏可推→试试其他"兜底)。showMoreGameButton(村里
    // 的小入口按钮)没列入白名单,保留不动,只掐自动整屏弹窗。
    if sel.starts_with("showMoreGame") {
        log!("[MOLECHEAT] 去广告:吞掉 {class} {sel}(淘米「更多游戏」跨游戏推荐弹窗)");
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
        log!("[MOLECHEAT] 去广告:吞掉 {class} {sel}(进村自动弹的跨游戏推荐弹窗 真凶)");
        return true;
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
            if class == "TMA_SSKeychain" && sel == "allAccounts" {
                let arr = crate::frameworks::foundation::ns_array::from_vec(env, vec![]);
                let arr = autorelease(env, arr);
                env.cpu.regs_mut()[0] = arr.to_bits();
                return true;
            }
            // (J) ★绕开 touchHLE 没实现的 JSONKit(JKDictionary/JKArray 是 unimplemented class → 解析 nil → 崩):
            //     拦 TMAHttpManager getDictionaryWithJsonData:,自己在 Rust 解析 passport 响应 JSON
            //     构造【标准 NSDictionary】喂回,客户端 requestFinish: 照常 objectForKey: 取 status_code/extra_data 分发。
            if class == "TMAHttpManager" && sel == "getDictionaryWithJsonData:" {
                let data: id = Ptr::from_bits(env.cpu.regs()[2]);
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
            }
            // (P1) 拦 TMA_ASINetworkQueue addOperation:(passport 真正的发送动作),代理到私服 shim。
            //      只在切换账号激活后代理(避免碰进村自动 autoLogin 的静默登录崩溃分支)。
            if MENU_ACTIVE.load(O) && class == "TMA_ASINetworkQueue" && sel == "addOperation:" {
                let req: id = Ptr::from_bits(env.cpu.regs()[2]);
                if passport_proxy_enqueue(env, req) {
                    return true;
                }
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
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            let set = env
                .objc
                .register_host_selector("setIsReachable:".to_string(), &mut env.mem);
            let _: () = msg_send(env, (nm, set, true));
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
            // [P5 持久化] 退岛先存岛 userInfo(任务进度nextQuestId/剧情nextStoryId/成就achieveUnlock/
            //   NPC npcs/经济 → userinfo.dat,游戏自有 saveUserinfoToLocal@0x21dcad,确保每次退岛都落盘
            //   而非靠游戏不定时存)。
            {
                let nsd_cls2 = env.objc.get_known_class("NewSceneData", &mut env.mem);
                if nsd_cls2 != nil {
                    let sh = env
                        .objc
                        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                    let nsd2: id = msg_send(env, (nsd_cls2, sh));
                    if nsd2 != nil {
                        let save_ui = env
                            .objc
                            .register_host_selector("saveUserinfoToLocal".to_string(), &mut env.mem);
                        let _: () = msg_send(env, (nsd2, save_ui));
                    }
                }
            }
            save_island_userinfo(env); // [P5] 存岛专属进度(任务/剧情/成就/扩地)到 island_userinfo.dat
            // [P3-a] ★先把会话期新放置的建筑(只在 ObjectManager 活表、没回写 mapData)并入 mapData,
            //   再 save_island_map 才能把新买的建筑一起存盘。★必须在真 gobackMainVillage(内含
            //   startNewSceneFrom→unloadMap 清空活表)之前——本 hook 是 pre-method,此刻活表满载岛对象。
            merge_new_island_objects_into_mapdata(env);
            save_island_map(env); // [P1] 再存建筑布局 island_map.dat(下次进岛读回)
            save_island_fragments(env); // [P4-b] 存玩家买到的火山/探险碎片(下次进岛读回)
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
                // [P2b 经营进度回写] 升级餐厅/雇用公寓/出海改的活建筑,游戏 saveTMMapDataFromObject:
                //   现造快照(a3,其 objectSequenceId 已对齐活对象 objSequenceId)喂 setModObjectToServer:
                //   发 1060;离线发包被吞、从不写回 mapData → 经营进度退岛丢。先把快照按 seqId 写回
                //   mapData,再 return true 跳过原方法(原方法只发被吞的包+push buffer,跳过顺带免积压)。
                //   注:新建筑 add 的 seqId 在 addObjectToServer: 内才分配,pre-hook 拿不到 → P3 放置链
                //   另解;此处只保【经营态】(mod,seqId 已就绪,覆盖默认岛 90001+ 的种子建筑)。
                ("NetworkManager", "setModObjectToServer:") => {
                    let snap: id = Ptr::from_bits(env.cpu.regs()[2]);
                    writeback_island_object(env, snap);
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
                // ★【已删除】曾有 (_,"archivedDataWithRootObject:") => regs[0]=0(归 nil)兜底,
                // 但实测它把【岛会话内自动存 userinfo.dat】写成了 36 字节空壳 → 下次启动 loadFromFile
                // 解档 UnexpectedEof 崩。真 storm 驱动是 sendPacket:commandId:(上面已切),这条本就多余,
                // 删除以杜绝存档损坏。存档器另在 ns_keyed_unarchiver 加容错防坏档崩启动(双保险)。
                _ => {}
            }
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
