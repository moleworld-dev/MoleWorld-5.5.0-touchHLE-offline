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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
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

thread_local! {
    /// The locally-built CaribbeanDiscoveringData (retained guest object) or nil.
    static CARIBBEAN_DATA: Cell<id> = const { Cell::new(nil) };
    /// 本次进岛是否已注入默认 mapData(每次进岛在 gate#1 reset,避免重复注入)。
    static ISLAND_INJECTED: Cell<bool> = const { Cell::new(false) };
    /// 诊断:上次记录的 LoadingHoliday curStep_,用于只在状态变化时打日志(看加载进度/卡点)。
    static ISLAND_LAST_STEP: Cell<i32> = const { Cell::new(-1) };
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
];

fn crack_group_on(g: CrackGroup) -> bool {
    match g {
        CrackGroup::Jailbreak => KILL_JAILBREAK.load(O),
        CrackGroup::DivineFix => FIX_DIVINE.load(O),
        CrackGroup::Holiday => ENTER_HOLIDAY.load(O),
        CrackGroup::StoreVip => STORE_NO_VIP.load(O),
        CrackGroup::Island => ENTER_NEWISLANDS.load(O),
        CrackGroup::ParseSkip => SKIP_PARSE_CHECK.load(O),
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

/// Cheap gate so the hot message path pays nothing when all cheats are off.
pub fn any_enabled() -> bool {
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
pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> bool {
    // 启动时 / 任一破解开关变更后,按当前开关状态把破解补丁写入或还原到模拟内存(香草基底)。
    // 写在最前面、只在 dirty 时跑一次:invalidate_cache_range 让 dynarmic 重新编译被改的指令。
    if CRACK_PATCHES_DIRTY.swap(false, O) {
        apply_crack_patches(env);
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
