/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [扫描修 2026-09-15] 物品与 VIP:隐藏物品进商店(静态白名单)、节日商店、按 ID 发物品/入仓库、
//! 充值解锁物补状态、VIP 本地持久化、连续登录/累计在线成就计数、头像建筑锁补全。
//! 由 mole_cheats::intercept 统一调度。
//!
//! 本模块钩子一览(`wants` 只做字符串比较):
//! - `-[NewStyleStoreItemsView loadObjectsDataByType:]`@0x3b9534:前置注入隐藏物品/节日物品(F1-1/F1-5/F1-8)。
//! - `-[AvatarLayer checkRequiredID:]`@0xfedb8:「全物品解锁」开着时头像建筑锁放开(F5-8)。
//! - `-[GameData onlineTimer]`@0x8b2a8 / `loginTimesCounter`@0x8b2c8:离线返回本地计数(F5-3/F9-5)。
//! - `-[GameManager startGame:]`@0x19164:进村记一次登录日、开始累计在线时长
//!   ([复核修 2026-09-15] R5-3:原挂 startGame@0x1914c,会漏掉直接发 startGame:1 的进村入口)。
//! - `-[UserVIPInfoData …]` 三个 getter/三个 setter/reset:VIP 本地持久化(F5-2)。
//! - `-[iMoleVillageAppDelegate applicationWillResignActive:/applicationWillTerminate:/applicationDidBecomeActive:]`:
//!   在线时长暂停/落盘。
//! - (非钩子)objc/messages.rs 的 SHELLHOOK 假购买贝壳后调 `on_shells_purchased`:充值解锁物补状态(F2-1),
//!   [补完 2026-09-15] 以及 VIP 值按档位价累计、按门槛升级(门槛为移植者自拟,非原版数据,可用 MOLE_VIP_THRESHOLDS 覆盖)。
//!
//! 返回值约定:归本模块独占的钩子(商店注入、头像锁、计数 getter)返回 Some(..);
//! 与 mole_cheats 共用的消息(startGame:、UserVIPInfoData、AppDelegate 生命周期)只做旁路副作用并返回 None,
//! 让 mole_cheats 后续的同名钩子(强制 VIP、离岛落盘等)照常执行——若返回 Some(false) 会把它们短路掉。
//! 发过宿主 msg_send 的分支在返回前一律恢复 r0-r3。

use crate::frameworks::foundation::ns_string;
use crate::fs::GuestPathBuf;
use crate::objc::{id, msg_send, nil, release, SEL};
use crate::Environment;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

const O: Ordering = Ordering::Relaxed;

/// 菜单用的隐藏物品目录条目。
/// `island == false` = 主村物品(主村商店上架/主村放置/入仓库);`island == true` = 黄金岛物品(岛商店上架/岛上放置)。
/// 同一 ID 两边都能用时目录里各有一条。
#[derive(Clone, Copy, Debug)]
pub struct HiddenItem {
    pub id: u32,
    pub name: &'static str,
    pub category: &'static str,
    pub island: bool,
}

// ============================================================================
// 通用小工具
// ============================================================================

fn sel_of(env: &mut Environment, name: &str) -> SEL {
    env.objc.register_host_selector(name.to_string(), &mut env.mem)
}

/// `[ClassName getter]`(单例取法)。类不存在返回 nil。
fn shared(env: &mut Environment, class_name: &str, getter: &str) -> id {
    let cls = env.objc.get_known_class(class_name, &mut env.mem);
    if cls == nil {
        return nil;
    }
    let s = sel_of(env, getter);
    msg_send(env, (cls, s))
}

/// 宿主侧沿 isa/superclass 链判断类型(不发消息,不碰寄存器)。
fn is_kind_of(env: &Environment, obj: id, want: &str) -> bool {
    if obj == nil {
        return false;
    }
    let mut cls = crate::objc::ObjC::read_isa(obj, &env.mem);
    for _ in 0..16 {
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

fn save_regs(env: &Environment) -> [u32; 4] {
    let r = env.cpu.regs();
    [r[0], r[1], r[2], r[3]]
}

fn restore_regs(env: &mut Environment, saved: [u32; 4]) {
    env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
}

// ============================================================================
// 本地日期(游戏时区 + 开发者时间旅行偏移)
// ============================================================================

/// 当前"本地墙钟秒"(unix 秒 + 本地时区偏移)。时区规则与引擎一致(默认 Asia/Shanghai,MOLE_TZ=host 跟随宿主),
/// 并加上 mole_dev 时间旅行偏移,保证节日窗口/登录日界与游戏内看到的时间一致。
fn local_wall_secs() -> i64 {
    let unix = crate::libc::time::host_now_unix_secs() + crate::libc::time::time_offset_secs();
    unix + crate::libc::time::local_utc_offset_at(unix) as i64
}

/// 本地日序号(1970-01-01 = 0)。
fn local_day_index() -> i64 {
    local_wall_secs().div_euclid(86400)
}

/// 公历 → 日序号(Howard Hinnant 算法)。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 日序号 → 公历 (年, 月, 日)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 复活节(公历,匿名算法)。
fn easter_date(y: i64) -> (u32, u32) {
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    (month as u32, day as u32)
}

/// 春节(农历正月初一)公历日期表。表外年份回落到 1/20~2/28 固定窗口。
const SPRING_FESTIVAL: &[(i64, u32, u32)] = &[
    (2020, 1, 25), (2021, 2, 12), (2022, 2, 1), (2023, 1, 22), (2024, 2, 10), (2025, 1, 29),
    (2026, 2, 17), (2027, 2, 6), (2028, 1, 26), (2029, 2, 13), (2030, 2, 3), (2031, 1, 23),
    (2032, 2, 11), (2033, 1, 31), (2034, 2, 19), (2035, 2, 8), (2036, 1, 28), (2037, 2, 15),
    (2038, 2, 4), (2039, 1, 24), (2040, 2, 12),
];

/// 七夕(农历七月初七)公历日期表。表外年份回落到 8 月整月。
const QIXI_DATE: &[(i64, u32, u32)] = &[
    (2020, 8, 25), (2021, 8, 14), (2022, 8, 4), (2023, 8, 22), (2024, 8, 10), (2025, 8, 29),
    (2026, 8, 19), (2027, 8, 8), (2028, 8, 26), (2029, 8, 16), (2030, 8, 5),
];

/// (月,日) 是否落在 [from, to] 内;from > to 表示跨年(如 12/10~1/6)。
fn md_in(m: u32, d: u32, from: (u32, u32), to: (u32, u32)) -> bool {
    let x = m * 100 + d;
    let a = from.0 * 100 + from.1;
    let b = to.0 * 100 + to.1;
    if a <= b {
        a <= x && x <= b
    } else {
        x >= a || x <= b
    }
}

/// day 是否在 (y,m,d) 前 before 天到后 after 天之内。
fn near_day(day: i64, y: i64, m: u32, d: u32, before: i64, after: i64) -> bool {
    let c = days_from_civil(y, m, d);
    day >= c - before && day <= c + after
}

// ============================================================================
// 节日商店(F1-5)
// ============================================================================

struct Festival {
    /// MOLE_FESTIVAL 可用的英文名
    key: &'static str,
    /// 中文名(菜单标签;MOLE_FESTIVAL 也接受中文)
    cn: &'static str,
    ids: &'static [u32],
}

/// 节日表。窗口取舍(原版由服务器按活动日期开放,客户端无日期表;以下按物品描述与节日惯例写死):
/// - 万圣 10/20~11/05;周年庆 10/20~11/03(原版「2 周年庆典」活动为 10/25~10/28,见 ANNIVERSARY_ACTIVITY_HINT);
/// - 丰收 11/10~11/30;圣诞 12/10~次年 1/06;冬季(雪人「4 月份融化」、雪花地毯/路灯)12/01~3/31;
/// - 新年 = 春节前后 15 天(含元宵,按农历年查表);世界杯(原版 2014 巴西世界杯/世界名胜系列)每年 6/10~7/20;
/// - 儿童节 5/25~6/05;七夕前 7 天~后 3 天(查表);复活节前后 7 天(公历算法)。
/// 七夕(鹊桥三件)与复活节(小兔)在包内没有商店图集美术,当前 ID 集合为空,只保留日期窗口。
const FESTIVALS: [Festival; 10] = [
    Festival { key: "halloween", cn: "万圣", ids: FEST_HALLOWEEN },
    Festival { key: "harvest", cn: "丰收", ids: FEST_HARVEST },
    Festival { key: "christmas", cn: "圣诞", ids: FEST_CHRISTMAS },
    Festival { key: "winter", cn: "冬季", ids: FEST_WINTER },
    Festival { key: "newyear", cn: "新年", ids: FEST_NEWYEAR },
    Festival { key: "worldcup", cn: "世界杯", ids: FEST_WORLDCUP },
    Festival { key: "childrens", cn: "儿童节", ids: FEST_CHILDRENS },
    Festival { key: "qixi", cn: "七夕", ids: FEST_QIXI },
    Festival { key: "anniversary", cn: "周年庆", ids: FEST_ANNIVERSARY },
    Festival { key: "easter", cn: "复活节", ids: FEST_EASTER },
];

fn festival_on_day(idx: usize, day: i64) -> bool {
    let (y, m, d) = civil_from_days(day);
    match FESTIVALS[idx].key {
        "halloween" => md_in(m, d, (10, 20), (11, 5)),
        "harvest" => md_in(m, d, (11, 10), (11, 30)),
        "christmas" => md_in(m, d, (12, 10), (1, 6)),
        "winter" => md_in(m, d, (12, 1), (3, 31)),
        "newyear" => match SPRING_FESTIVAL.iter().find(|e| e.0 == y) {
            Some(&(yy, mm, dd)) => near_day(day, yy, mm, dd, 15, 15),
            None => md_in(m, d, (1, 20), (2, 28)),
        },
        "worldcup" => md_in(m, d, (6, 10), (7, 20)),
        "childrens" => md_in(m, d, (5, 25), (6, 5)),
        "qixi" => match QIXI_DATE.iter().find(|e| e.0 == y) {
            Some(&(yy, mm, dd)) => near_day(day, yy, mm, dd, 7, 3),
            None => md_in(m, d, (8, 1), (8, 31)),
        },
        "anniversary" => md_in(m, d, (10, 20), (11, 3)),
        "easter" => {
            let (em, ed) = easter_date(y);
            near_day(day, y, em, ed, 7, 7)
        }
        _ => false,
    }
}

const FEST_UNINIT: u8 = 0xff;
const FEST_BY_DATE: u8 = 0;
const FEST_ALL: u8 = 1;
const FEST_OFF: u8 = 2;
/// 16 + 节日下标 = MOLE_FESTIVAL=<节日名> 强制只开这一个节日(调试用)。
const FEST_FORCE_BASE: u8 = 16;
/// 节日商店模式。默认「按日期」(忠实功能:原版节日物本就按活动日期上架);MOLE_FESTIVAL=all/off/<节日名> 覆盖初值。
static FEST_MODE: AtomicU8 = AtomicU8::new(FEST_UNINIT);

fn fest_mode() -> u8 {
    let m = FEST_MODE.load(O);
    if m != FEST_UNINIT {
        return m;
    }
    let init = match std::env::var("MOLE_FESTIVAL") {
        Err(_) => FEST_BY_DATE,
        Ok(raw) => {
            let v = raw.trim().to_ascii_lowercase();
            match v.as_str() {
                "" | "date" | "auto" => FEST_BY_DATE,
                "all" | "on" | "1" => FEST_ALL,
                "off" | "none" | "0" => FEST_OFF,
                other => match FESTIVALS.iter().position(|f| f.key == other || f.cn == other) {
                    Some(i) => FEST_FORCE_BASE + i as u8,
                    None => {
                        log!("[MOLEITEMS] MOLE_FESTIVAL={} 无法识别(可用 all/off/date 或节日名),按日期处理", raw);
                        FEST_BY_DATE
                    }
                },
            }
        }
    };
    let _ = FEST_MODE.compare_exchange(FEST_UNINIT, init, O, O);
    FEST_MODE.load(O)
}

/// 当前生效的节日位图(第 i 位 = FESTIVALS[i])。
fn festival_active_mask() -> u16 {
    let mode = fest_mode();
    match mode {
        FEST_OFF => 0,
        FEST_ALL => (1u16 << FESTIVALS.len()) - 1,
        m if m >= FEST_FORCE_BASE && ((m - FEST_FORCE_BASE) as usize) < FESTIVALS.len() => {
            1u16 << (m - FEST_FORCE_BASE)
        }
        _ => {
            let day = local_day_index();
            let mut mask = 0u16;
            for i in 0..FESTIVALS.len() {
                if festival_on_day(i, day) {
                    mask |= 1 << i;
                }
            }
            mask
        }
    }
}

fn festival_names(mask: u16) -> String {
    let mut names: Vec<&str> = Vec::new();
    for (i, f) in FESTIVALS.iter().enumerate() {
        if mask & (1 << i) != 0 {
            names.push(f.cn);
        }
    }
    names.join("/")
}

/// 节日商店当前模式的菜单标签(如「节日商店:按日期(万圣/周年庆)」「节日商店:全开」「节日商店:关」)。
pub fn festival_shop_label() -> String {
    let mode = fest_mode();
    match mode {
        FEST_OFF => "节日商店:关".to_string(),
        FEST_ALL => "节日商店:全开".to_string(),
        m if m >= FEST_FORCE_BASE && ((m - FEST_FORCE_BASE) as usize) < FESTIVALS.len() => {
            format!("节日商店:强制{}", FESTIVALS[(m - FEST_FORCE_BASE) as usize].cn)
        }
        _ => {
            let names = festival_names(festival_active_mask());
            if names.is_empty() {
                "节日商店:按日期(今日无节日)".to_string()
            } else {
                format!("节日商店:按日期({})", names)
            }
        }
    }
}

/// 轮换节日商店模式(按日期 → 全开 → 关 → 按日期),返回新标签。
/// 已上架的节日物在下一次打开/切换商店分页时按新模式增删(见 store_reconcile)。
pub fn cycle_festival_mode() -> String {
    let next = match fest_mode() {
        FEST_BY_DATE => FEST_ALL,
        FEST_ALL => FEST_OFF,
        _ => FEST_BY_DATE,
    };
    FEST_MODE.store(next, O);
    let label = festival_shop_label();
    log!("[MOLEITEMS] 切换 {}", label);
    label
}

// ============================================================================
// 隐藏物品进商店(F1-1 / F1-8①②)+ 节日物进「强烈推荐」(F1-5)
// ============================================================================

/// 「隐藏物品进商店」作弊开关,默认关。
static HIDDEN_SHOP: AtomicBool = AtomicBool::new(false);

/// 隐藏物品目录(只含有美术、可摆放/可入仓库的条目),供菜单分页浏览。
pub fn hidden_catalog() -> &'static [HiddenItem] {
    HIDDEN_CATALOG_TABLE
}

/// 「隐藏物品进商店」开关当前状态。
pub fn hidden_shop_on() -> bool {
    HIDDEN_SHOP.load(O)
}

/// 切换「隐藏物品进商店」,返回切换后的状态。关掉后已上架的隐藏物在下一次打开对应分页时撤下。
pub fn toggle_hidden_shop() -> bool {
    let on = !HIDDEN_SHOP.load(O);
    HIDDEN_SHOP.store(on, O);
    log!(
        "[MOLEITEMS] 隐藏物品进商店:{}(主村 {} 件 / 黄金岛 {} 件,打开或切换商店分页时生效)",
        if on { "开" } else { "关" },
        MAIN_SHOP.len(),
        ISLAND_SHOP.len()
    );
    on
}

/// 已核对/注入过的商店数组:(数组指针, 处理后的 count, 期望集签名)。
/// 岛上 NewSceneData 的桶会被 resetNewSceneDataExceptObjectData 清空并由 mole_cheats 强制重填(同一数组指针、count 变化),
/// 按「指针 + count + 签名」判定,count 一变就重新核对注入;开关/节日变化时签名变化,同样重新核对(增删)。
static STORE_CACHE: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(Vec::new());

fn store_cache() -> MutexGuard<'static, Vec<(u32, u32, u32)>> {
    STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

fn shop_table(island: bool) -> &'static [(u32, u8, u8)] {
    if island {
        ISLAND_SHOP
    } else {
        MAIN_SHOP
    }
}

fn shop_entry(island: bool, item: u32) -> Option<(u8, u8)> {
    let t = shop_table(island);
    t.binary_search_by_key(&item, |e| e.0)
        .ok()
        .map(|i| (t[i].1, t[i].2))
}

/// 返回 (本分页期望出现的 ID, 本分页归本模块管理的 ID 全集),都已排序去重。
/// 生成表保证:归本模块管理的 ID 在该场景的原版解析里不会进入同一个数组
/// (无 shop_type/缺子分页/store_able 无 bit1),因此"管理集里却不在期望集"的元素一定是我们以前加的,可以安全撤下。
fn store_desired(island: bool, list_type: i32, hidden_on: bool, mask: u16) -> (Vec<u32>, Vec<u32>) {
    let mut want = Vec::new();
    let mut managed = Vec::new();
    if list_type == 3 {
        for (i, f) in FESTIVALS.iter().enumerate() {
            for &item in f.ids {
                if shop_entry(island, item).is_none() {
                    continue;
                }
                managed.push(item);
                if mask & (1 << i) != 0 {
                    want.push(item);
                }
            }
        }
    } else if (5..=16).contains(&list_type) {
        let (kind, sub) = if list_type <= 10 {
            (1u8, (list_type - 4) as u8)
        } else {
            (2u8, (list_type - 10) as u8)
        };
        for &(item, k, s) in shop_table(island) {
            if k == kind && s == sub {
                managed.push(item);
                if hidden_on {
                    want.push(item);
                }
            }
        }
    }
    want.sort_unstable();
    want.dedup();
    managed.sort_unstable();
    managed.dedup();
    (want, managed)
}

fn store_sig(island: bool, list_type: i32, hidden_on: bool, mask: u16) -> u32 {
    (mask as u32)
        | ((hidden_on as u32) << 16)
        | ((island as u32) << 17)
        | (((list_type as u32) & 0xff) << 18)
}

fn store_page_name(list_type: i32) -> &'static str {
    match list_type {
        3 => "强烈推荐",
        5 => "建设庄园/基础建筑",
        6 => "建设庄园/农场园艺",
        7 => "建设庄园/装饰建筑",
        8 => "建设庄园/动物",
        9 => "建设庄园/趣味设施",
        10 => "建设庄园/增强道具",
        11 => "美化庄园/路面地形",
        12 => "美化庄园/标识物",
        13 => "美化庄园/花卉植被",
        14 => "美化庄园/多彩生活",
        15 => "美化庄园/小装饰",
        16 => "美化庄园/灯火",
        _ => "?",
    }
}

/// -[NewStyleStoreItemsView loadObjectsDataByType:]@0x3b9534 前置钩子。
/// 原版:r2=3 → [数据源 getNewProductsIds](强烈推荐);4 → loadResourceItems(贝壳商店,不动);
/// 5..10 → storeBuildingsArray[r2-5];11..16 → storeDecorationsArray[r2-11];数据源按 curSceneId:1=GameData,10=NewSceneData。
/// 数组元素是 ObjectData 对象(-[GameData parseObjectData:]@0x6f52c `addObject: r6=ObjectData`),
/// 所以这里同样取 [数据源 getObjectDataWithId:] 的对象 addObject: 进去,原版表格/购买/摆放链全部复用。
fn store_hook(env: &mut Environment) -> Option<bool> {
    let saved = save_regs(env);
    let list_type = saved[2] as i32;
    if env.options.network_access {
        return None; // 在线以私服为准,不改商店
    }
    if !(list_type == 3 || (5..=16).contains(&list_type)) {
        return None;
    }
    let hidden_on = HIDDEN_SHOP.load(O);
    let mask = festival_active_mask();
    if !hidden_on && mask == 0 && store_cache().is_empty() {
        return None; // 什么都不需要上架,也没有待撤下的旧注入
    }
    store_reconcile(env, list_type, hidden_on, mask);
    restore_regs(env, saved);
    Some(false)
}

fn store_reconcile(env: &mut Environment, list_type: i32, hidden_on: bool, mask: u16) {
    // ① 场景 → 数据源(与原版同一判据)。黄金岛会话里主村商店注入不动。
    let scene_mgr = shared(env, "SceneMannager", "sharedManager");
    if scene_mgr == nil {
        return;
    }
    let cur_s = sel_of(env, "curSceneId");
    let scene: i32 = msg_send(env, (scene_mgr, cur_s));
    let island = match scene {
        1 if !crate::mole_cheats::island_session_active() => false,
        10 => true,
        _ => return,
    };
    let data = if island {
        shared(env, "NewSceneData", "sharedInstance")
    } else {
        shared(env, "GameData", "sharedInstance")
    };
    if data == nil {
        return;
    }
    let cnt_s = sel_of(env, "count");
    let oai_s = sel_of(env, "objectAtIndex:");
    // ② 找到本分页读的那个数组
    let arr: id = if list_type == 3 {
        let s = sel_of(env, "getNewProductsIds");
        msg_send(env, (data, s))
    } else {
        let (getter, idx) = if list_type <= 10 {
            ("storeBuildingsArray", (list_type - 5) as u32)
        } else {
            ("storeDecorationsArray", (list_type - 11) as u32)
        };
        let gs = sel_of(env, getter);
        let outer: id = msg_send(env, (data, gs));
        if outer == nil || !is_kind_of(env, outer, "NSArray") {
            return;
        }
        let n: u32 = msg_send(env, (outer, cnt_s));
        if idx >= n {
            return;
        }
        msg_send(env, (outer, oai_s, idx))
    };
    if arr == nil || !is_kind_of(env, arr, "NSMutableArray") {
        return;
    }
    let (want, managed) = store_desired(island, list_type, hidden_on, mask);
    let sig = store_sig(island, list_type, hidden_on, mask);
    let arr_bits = arr.to_bits();
    let count: u32 = msg_send(env, (arr, cnt_s));
    {
        let cache = store_cache();
        match cache.iter().find(|e| e.0 == arr_bits) {
            Some(&(_, c, s)) if c == count && s == sig => return, // 已核对过且没变化
            None if want.is_empty() => return,                    // 从没注入过、这次也不需要
            _ => {}
        }
    }
    if managed.is_empty() {
        return;
    }
    // ③ 倒序扫描:撤下"归本模块管、但当前不该出现"的条目(关开关/节日过期),顺便去重
    let oid_s = sel_of(env, "objectId");
    let rm_s = sel_of(env, "removeObjectAtIndex:");
    let mut present: Vec<u32> = Vec::new();
    let mut removed = 0u32;
    let mut i = count;
    while i > 0 {
        i -= 1;
        let obj: id = msg_send(env, (arr, oai_s, i));
        if obj == nil {
            continue;
        }
        let oid: i32 = msg_send(env, (obj, oid_s));
        let oid = oid as u32;
        if managed.binary_search(&oid).is_err() {
            continue;
        }
        if want.binary_search(&oid).is_err() || present.contains(&oid) {
            let _: () = msg_send(env, (arr, rm_s, i));
            removed += 1;
        } else {
            present.push(oid);
        }
    }
    // ④ 补上缺的(按 ID 升序追加在原版条目之后)
    let get_s = sel_of(env, "getObjectDataWithId:");
    let add_s = sel_of(env, "addObject:");
    let mut added = 0u32;
    for &item in &want {
        if present.contains(&item) {
            continue;
        }
        let od: id = msg_send(env, (data, get_s, item as i32));
        if od == nil {
            continue;
        }
        let _: () = msg_send(env, (arr, add_s, od));
        added += 1;
    }
    let new_count: u32 = msg_send(env, (arr, cnt_s));
    {
        let mut cache = store_cache();
        cache.retain(|e| e.0 != arr_bits);
        if !want.is_empty() {
            if cache.len() >= 64 {
                cache.remove(0);
            }
            cache.push((arr_bits, new_count, sig));
        }
    }
    if added > 0 || removed > 0 {
        log!(
            "[MOLEITEMS] 商店注入:{} {} 上架 {} 件、撤下 {} 件(隐藏物品={} 节日={})",
            if island { "黄金岛" } else { "主村" },
            store_page_name(list_type),
            added,
            removed,
            if hidden_on { "开" } else { "关" },
            festival_names(mask)
        );
    }
}

// ============================================================================
// 本地 sidecar:Documents/vip.dat(VIP 三值 + 登录日/连续天数 + 累计在线毫秒)
// ============================================================================

const SIDE_FILE: &str = "Documents/vip.dat";
const SIDE_MAGIC: &str = "MOLEVIP 1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct VipVals {
    level: i32,
    value: i32,
    next: i32,
}

struct Side {
    loaded: bool,
    /// 读到坏档且备份失败:本会话不覆盖原文件。
    save_blocked: bool,
    /// None = 从没记录过 VIP(不注入,保持原版 0)。
    vip: Option<VipVals>,
    /// 上次进村的本地日序号,0 = 从没进过村。
    last_day: i64,
    streak: u32,
    online_ms: u64,
    /// 在线计时起点(进村后开始,切后台暂停)。
    anchor: Option<Instant>,
    last_flush: Option<Instant>,
    village_entered: bool,
}

impl Side {
    const fn new() -> Side {
        Side {
            loaded: false,
            save_blocked: false,
            vip: None,
            last_day: 0,
            streak: 0,
            online_ms: 0,
            anchor: None,
            last_flush: None,
            village_entered: false,
        }
    }
}

static SIDE: Mutex<Side> = Mutex::new(Side::new());
static SIDE_BLOCK_LOGGED: AtomicBool = AtomicBool::new(false);

/// 注意:持锁期间绝不能发 msg_send(嵌套的 objc_msgSend 可能再次进入本模块钩子而死锁)。
fn side() -> MutexGuard<'static, Side> {
    SIDE.lock().unwrap_or_else(|e| e.into_inner())
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn side_serialize(s: &Side) -> Vec<u8> {
    let mut body = String::new();
    body.push_str(SIDE_MAGIC);
    body.push('\n');
    if let Some(v) = s.vip {
        body.push_str(&format!("vip={},{},{}\n", v.level, v.value, v.next));
    }
    body.push_str(&format!("login={},{}\n", s.last_day, s.streak));
    body.push_str(&format!("online_ms={}\n", s.online_ms));
    let sum = fnv1a(body.as_bytes());
    body.push_str(&format!("sum={:08x}\n", sum));
    body.into_bytes()
}

struct SideParsed {
    vip: Option<VipVals>,
    last_day: i64,
    streak: u32,
    online_ms: u64,
}

/// 解析失败(魔数/校验和/字段格式不对)返回 None = 坏档。
fn side_parse(bytes: &[u8]) -> Option<SideParsed> {
    let text = std::str::from_utf8(bytes).ok()?;
    let pos = text.rfind("sum=")?;
    let (body, tail) = text.split_at(pos);
    let sum = u32::from_str_radix(tail.trim_start_matches("sum=").trim(), 16).ok()?;
    if fnv1a(body.as_bytes()) != sum {
        return None;
    }
    let mut lines = body.lines();
    if lines.next()? != SIDE_MAGIC {
        return None;
    }
    let mut p = SideParsed {
        vip: None,
        last_day: 0,
        streak: 0,
        online_ms: 0,
    };
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once('=')?;
        match k {
            "vip" => {
                let mut it = v.split(',');
                let level: i32 = it.next()?.trim().parse().ok()?;
                let value: i32 = it.next()?.trim().parse().ok()?;
                let next: i32 = it.next()?.trim().parse().ok()?;
                p.vip = Some(VipVals { level, value, next });
            }
            "login" => {
                let (a, b) = v.split_once(',')?;
                p.last_day = a.trim().parse().ok()?;
                p.streak = b.trim().parse().ok()?;
            }
            "online_ms" => p.online_ms = v.trim().parse().ok()?,
            _ => {} // 未知键:向前兼容,忽略
        }
    }
    Some(p)
}

/// 懒加载 sidecar。坏档先原样备份为 vip.dat.corrupt(已存在则 .corrupt-<unix秒>),
/// 备份成功才按空档继续(之后允许覆盖);备份失败则本会话禁止覆盖原文件。
fn side_ensure_loaded(env: &mut Environment) {
    if side().loaded {
        return;
    }
    let path: GuestPathBuf = env.fs.home_directory().join(SIDE_FILE);
    let mut blocked = false;
    let mut parsed: Option<SideParsed> = None;
    if let Ok(bytes) = env.fs.read(&path) {
        match side_parse(&bytes) {
            Some(p) => parsed = Some(p),
            None => {
                let mut bak = format!("{}.corrupt", SIDE_FILE);
                if env.fs.read(env.fs.home_directory().join(&bak)).is_ok() {
                    bak = format!(
                        "{}.corrupt-{}",
                        SIDE_FILE,
                        crate::libc::time::host_now_unix_secs()
                    );
                }
                let bak_path = env.fs.home_directory().join(&bak);
                if env.fs.write_atomic(&bak_path, &bytes).is_ok() {
                    log!(
                        "[MOLEITEMS] ⚠️ {} 校验失败(坏档/写残)→ 已原样备份为 {},本次按空档处理",
                        SIDE_FILE,
                        bak
                    );
                } else {
                    blocked = true;
                    log!(
                        "[MOLEITEMS] ⚠️ {} 校验失败且备份失败 → 本会话不覆盖它(VIP/登录计数本会话不落盘)",
                        SIDE_FILE
                    );
                }
            }
        }
    }
    let mut s = side();
    if s.loaded {
        return;
    }
    s.loaded = true;
    s.save_blocked = blocked;
    if let Some(p) = parsed {
        s.vip = p.vip;
        s.last_day = p.last_day;
        s.streak = p.streak;
        s.online_ms = p.online_ms;
    }
}

fn side_save(env: &mut Environment) {
    let (bytes, blocked) = {
        let s = side();
        (side_serialize(&s), s.save_blocked)
    };
    if blocked {
        if !SIDE_BLOCK_LOGGED.swap(true, O) {
            log!("[MOLEITEMS] 跳过写 {}(原文件是未能备份的坏档)", SIDE_FILE);
        }
        return;
    }
    let path: GuestPathBuf = env.fs.home_directory().join(SIDE_FILE);
    if env.fs.write_atomic(&path, &bytes).is_err() {
        log!("[MOLEITEMS] 写 {} 失败", SIDE_FILE);
    }
}

/// [复核修 2026-09-15] R5-1:调试菜单「删本地存档」删掉 vip.dat 之后调用。
/// 返修后那条路径删完就直接 std::process::exit(0)(否则游戏关窗时会用 saveToLocal: 把内存里的旧主档写回,
/// 见 mole_menu.rs run_action),本函数只作兜底:万一以后去掉退出,也不让 vip.dat 被旧值写回。
/// 根因:SIDE 在内存里仍是 loaded=true、带着旧的 VIP 三值/登录日/连续天数/在线毫秒;玩家正常关窗时
/// app_lifecycle 收到 applicationWillResignActive:/applicationWillTerminate: 会 side_save,把旧值原子写回 vip.dat,
/// 新档就继承旧号的连续登录天数与在线时长(原版 checkAchieve_ContinueLogin/TotalOnline 可能提前真发奖)。
/// 做法:内存状态换成全新空档并保持 loaded=true(不再从磁盘懒加载),同时 save_blocked=true,
/// 本会话后续任何 side_save 都不落盘,下次启动 vip.dat 不存在 → 从零开始。
/// 只动宿主状态、不发消息、不碰寄存器,任何上下文都能调;在线模式本模块不用 sidecar,调用也无副作用。
pub fn forget_sidecar() {
    {
        let mut s = side();
        *s = Side::new();
        s.loaded = true;
        s.save_blocked = true;
    }
    // side_save 在 save_blocked 时打印的是"坏档未能备份"的提示;这里是有意禁写,先置位免得日志误导。
    SIDE_BLOCK_LOGGED.store(true, O);
    log!(
        "[MOLEITEMS] 已清空内存里的 VIP/登录/在线计数,本会话不再写 {}(重启后从零开始)",
        SIDE_FILE
    );
}

fn online_flush(s: &mut Side) {
    let now = Instant::now();
    if let Some(a) = s.anchor {
        let ms = now.duration_since(a).as_millis() as u64;
        s.online_ms = s.online_ms.saturating_add(ms);
        s.anchor = Some(now);
    }
    s.last_flush = Some(now);
}

fn online_total_secs(s: &Side) -> u64 {
    let extra = s
        .anchor
        .map(|a| a.elapsed().as_millis() as u64)
        .unwrap_or(0);
    s.online_ms.saturating_add(extra) / 1000
}

// ============================================================================
// 连续登录 / 累计在线(F5-3 / F9-5)
// ============================================================================

/// -[GameManager startGame:]@0x19164 前置。离线专属,只动宿主状态,不发消息。
/// [复核修 2026-09-15] R5-3:原来挂的是 -[GameManager startGame]@0x1914c,它只是转调 [self startGame:0](0x1915e)。
/// 进村入口有三处:-[LoadingLayer loadTarget] targetScene==4 分支发 startGame(0x12ee2e)、
/// -[SceneMannager loadMainVillageScene] 发 startGame(0x241092)、以及 loadTarget 先 reset 各任务系统再
/// loadFromLocal 的分支【直接】发 startGame:1(0x12f0e6)——最后这条完全绕过 startGame,
/// 当天不记登录日、在线计时也不开始。改挂 startGame:,三处入口都经过它且每次进村只触发一次。
/// 规则:同一本地日重复进村不变;昨天进过 → 连续天数 +1;断档或首次 → 1;宿主时钟回拨 → 保持原记录。
fn on_enter_village(env: &mut Environment) {
    side_ensure_loaded(env);
    let today = local_day_index();
    let (changed, streak, secs) = {
        let mut s = side();
        let before = (s.last_day, s.streak);
        if s.last_day != today {
            if s.last_day > 0 && today == s.last_day + 1 {
                s.streak = s.streak.saturating_add(1);
                s.last_day = today;
            } else if s.last_day > 0 && today < s.last_day {
                // 宿主时钟回拨:不清零也不累加
            } else {
                s.streak = 1;
                s.last_day = today;
            }
        } else if s.streak == 0 {
            s.streak = 1;
        }
        s.village_entered = true;
        if s.anchor.is_none() {
            s.anchor = Some(Instant::now());
        }
        online_flush(&mut s);
        (
            (s.last_day, s.streak) != before,
            s.streak,
            online_total_secs(&s),
        )
    };
    side_save(env);
    if changed {
        log!(
            "[MOLEITEMS] 离线登录计数:连续登录 {} 天,累计在线 {} 秒(成就「衷心感谢」要 30 天、「超感动」要 100 小时)",
            streak,
            secs
        );
    }
}

/// -[GameData onlineTimer]/loginTimesCounter 前置:离线返回本地计数。
/// 原版这两个 ivar 只由 -[NetworkManager parseOnlineTime:]/parseLoginCount: 写入(离线恒 0),读点全二进制只有
/// -[AchievementControl checkAchieve_TotalOnline]@0x1f5a40(onlineTimer/3600 与 total_online 小时数比较,
/// 0x1f5afc 魔数 0x91a2b3c5 = 除以 3600,故单位是秒)与 checkAchieve_ContinueLogin@0x1f5bc0(直接与 continue_login 天数比较)。
/// 直接拦 getter 比找时机调 setter 更稳:不怕 reset/读档顺序;成就自带 checkInAlreadyUnlockList: 去重。
fn stats_getter(env: &mut Environment, sel: &str) -> Option<bool> {
    if env.options.network_access {
        return None;
    }
    side_ensure_loaded(env);
    let (value, need_save) = {
        let mut s = side();
        let v = if sel == "onlineTimer" {
            online_total_secs(&s).min(u32::MAX as u64) as u32
        } else {
            s.streak
        };
        let due = s.anchor.is_some()
            && s.last_flush.map_or(true, |t| t.elapsed().as_secs() >= 300);
        if due {
            online_flush(&mut s);
        }
        (v, due)
    };
    if need_save {
        side_save(env);
    }
    env.cpu.regs_mut()[0] = value;
    Some(true)
}

/// 应用切后台/退出:在线计时暂停并落盘;回前台:进过村才恢复计时。只做宿主状态,不发消息。
fn app_lifecycle(env: &mut Environment, sel: &str) -> Option<bool> {
    if env.options.network_access || !side().loaded {
        return None;
    }
    match sel {
        "applicationWillResignActive:" | "applicationWillTerminate:" => {
            {
                let mut s = side();
                online_flush(&mut s);
                s.anchor = None;
            }
            side_save(env);
        }
        "applicationDidBecomeActive:" => {
            let mut s = side();
            if s.village_entered && s.anchor.is_none() {
                s.anchor = Some(Instant::now());
                s.last_flush = Some(Instant::now());
            }
        }
        _ => {}
    }
    None
}

// ============================================================================
// VIP 本地持久化(F5-2)
// ============================================================================

/// 本地玩家的 UserVIPInfoData 指针(GameData.userVIPInfoData_,init 时创建、不替换;好友 VIP 是另外的对象)。
static VIP_LOCAL: AtomicU32 = AtomicU32::new(0);
/// 本地对象当前是否已写入 sidecar 的 VIP 值(-[UserVIPInfoData reset] 后清掉,下次读时重写)。
static VIP_INJECTED: AtomicBool = AtomicBool::new(false);
/// 宿主正在调 VIP setter/取对象时置位,防止嵌套消息再进本钩子。
static VIP_BUSY: AtomicBool = AtomicBool::new(false);
/// -[UserVIPInfoData init]@0x6abc8 / initWithVIPInfoData:@0x6ac0c / reset@0x6aca0 三个方法的地址范围:
/// 它们内部调 setter 属于对象初始化/清零,不是"VIP 值变化",不记账。
const VIP_INTERNAL_LR: std::ops::Range<u32> = 0x6abc8..0x6ace4;

fn local_vip_object(env: &mut Environment) -> id {
    let gd = shared(env, "GameData", "sharedInstance");
    if gd == nil {
        return nil;
    }
    let s = sel_of(env, "userVIPInfoData");
    msg_send(env, (gd, s))
}

/// 用原版 setter 写入(setter 自己走 CryptUtils encryptInt: 加密,不直写 ivar)。
fn vip_apply(env: &mut Environment, obj: id, v: VipVals) {
    let s1 = sel_of(env, "setVipLevel:");
    let _: () = msg_send(env, (obj, s1, v.level));
    let s2 = sel_of(env, "setVipValue:");
    let _: () = msg_send(env, (obj, s2, v.value));
    let s3 = sel_of(env, "setVipValueOfNextLevel:");
    let _: () = msg_send(env, (obj, s3, v.next));
}

/// UserVIPInfoData 没有 encodeWithCoder,VIP 三值原版只由 -[NetworkManager parseVipInfo:] 写入,离线每次启动恒 0。
/// 做法:本地对象第一次被读(vipLevelWithNewType/vipValue/vipValueOfNextLevel)时把 sidecar 的值用 setter 写回;
/// -[GameData resetObjectInfoUnaddedInMap]@0x7abc8 会对它 reset,reset 后下一次读再写回。
/// 值的来源:任何代码(含菜单)经 setter 改了本地对象的 VIP 值,这里记账并原子落盘。
/// 在线模式整段跳过(交给 parseVipInfo)。除下面这一处外所有分支返回 None,不遮挡 mole_cheats 的强制 VIP 钩子。
/// [补完 2026-09-15] 唯一例外:强制 VIP 开着时,本地对象的 vipValueOfNextLevel 返回 0(Some(true))。
/// 根因:-[VIPLayer showWithTarget:selector:] 读 vipValue(0x37f07e,被 mole_cheats 强制成 999999)和
/// vipValueOfNextLevel(0x37f0a0,mole_cheats 不拦、读真实值)。VIP 累计升级之后真实 next 不再是 0,
/// 于是跳过 0x37f136 的满级分支,进度 = 999999×100/next,0x37f240 按无符号算 (next−999999)/10,
/// 例如强制 VIP4、真实 next=1000 时,界面出现「达到 VIP 5 您还需充值 429396829 元」,进度 99999%。强制显示值和真实累计值混在一起了。
/// 这里把 next 也按「强制 = 满级」返回 0,和强制 vipValue 配套,等价于累计升级之前离线恒为 0 时的满级显示。
/// 全二进制读 vipValueOfNextLevel 的只有 3 处(initWithVIPInfoData:@0x6ac7e、在线的 parseVipInfo、VIPLayer),
/// 离线没有「读 next → 调 setter 写回」的路径,返回强制值 0 不会经 setter 写进侧档;真实值仍在对象和 vip.dat 里,关掉强制 VIP 就恢复。
fn vip_hook(env: &mut Environment, sel: &str) -> Option<bool> {
    if env.options.network_access || VIP_BUSY.load(O) {
        return None;
    }
    let saved = save_regs(env);
    let recv = saved[0];
    match sel {
        "reset" => {
            if recv != 0 && recv == VIP_LOCAL.load(O) {
                VIP_INJECTED.store(false, O);
            }
            None
        }
        "vipLevelWithNewType" | "vipValue" | "vipValueOfNextLevel" => {
            // [补完 2026-09-15] 强制 VIP 下本地对象的 next 返回 0(原因见函数注释)。
            // 强制 VIP 关着时 force_next 恒 false,下面的读档注入流程与原来逐字一致。
            let force_next =
                sel == "vipValueOfNextLevel" && crate::mole_cheats::is_on("force_vip");
            if recv == VIP_LOCAL.load(O) && VIP_INJECTED.load(O) {
                if force_next {
                    env.cpu.regs_mut()[0] = 0;
                    return Some(true);
                }
                return None;
            }
            side_ensure_loaded(env);
            let vals = side().vip;
            if vals.is_none() && !force_next {
                return None;
            }
            VIP_BUSY.store(true, O);
            let local = local_vip_object(env);
            let is_local = local != nil && local.to_bits() == recv;
            if is_local {
                VIP_LOCAL.store(recv, O);
                if let Some(v) = vals {
                    vip_apply(env, local, v);
                    VIP_INJECTED.store(true, O);
                    log!(
                        "[MOLEITEMS] VIP 读档:从 {} 写回 vipLevel={} vipValue={} vipValueOfNextLevel={}",
                        SIDE_FILE,
                        v.level,
                        v.value,
                        v.next
                    );
                }
            }
            VIP_BUSY.store(false, O);
            restore_regs(env, saved);
            if force_next && is_local {
                // 读档注入(若有)已先做完,对象里是真实值;这里只改本次返回值。
                env.cpu.regs_mut()[0] = 0;
                return Some(true);
            }
            None
        }
        "setVipLevel:" | "setVipValue:" | "setVipValueOfNextLevel:" => {
            let lr = env.cpu.regs()[14] & !1;
            if VIP_INTERNAL_LR.contains(&lr) {
                return None;
            }
            let val = saved[2] as i32;
            let local_bits = if recv != 0 && recv == VIP_LOCAL.load(O) {
                recv
            } else {
                VIP_BUSY.store(true, O);
                let l = local_vip_object(env);
                VIP_BUSY.store(false, O);
                restore_regs(env, saved);
                if l != nil {
                    VIP_LOCAL.store(l.to_bits(), O);
                }
                l.to_bits()
            };
            if local_bits == 0 || local_bits != recv {
                return None;
            }
            side_ensure_loaded(env);
            let changed = {
                let mut s = side();
                let mut v = s.vip.unwrap_or(VipVals {
                    level: 0,
                    value: 0,
                    next: 0,
                });
                match sel {
                    "setVipLevel:" => v.level = val,
                    "setVipValue:" => v.value = val,
                    _ => v.next = val,
                }
                if s.vip == Some(v) {
                    false
                } else {
                    s.vip = Some(v);
                    true
                }
            };
            if changed {
                side_save(env);
                log!("[MOLEITEMS] VIP 值变化 {}={} → 已记入 {}", sel, val, SIDE_FILE);
            }
            None
        }
        _ => None,
    }
}

// ============================================================================
// 充值解锁物(F2-1)+ [补完 2026-09-15] 贝壳档位表 / VIP 随贝壳购买累计升级
// ============================================================================

/// [补完 2026-09-15] 贝壳商店的一个充值档位。数据逐字取自客户端 zh-Hans.lproj/100_0.dat
/// (-[GameData loadShopItems]@0x70264 读入 GameData.shopItems_,键 itemid/count/price/productid;
/// productid「saleN」在 0x70518 用 "%@.%@" 拼成 com.taomee.MoleWorld.saleN,N = itemid − 1)。
/// SHELLHOOK 用它决定发几个贝壳,VIP 累计用它的价格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellPack {
    /// ShopItemData.itemid,即 -[NewStyleStoreMainLayer onBuyVIPGold:] 的 int 参数(1..7)。
    pub item_id: u32,
    /// 100_0.dat 的 count:本档贝壳数。
    pub shells: i32,
    /// 100_0.dat 的 price,单位美分。客户端只存美元价:-[InAppPurchaseManager onPurchaseSuccessful] 调
    /// +[TaomeeAnalytics logIAP:productId:price:currency:] 时币种写死 "USD"(0x117cf4)。
    pub usd_cents: u32,
    /// 同一 App Store 价格档在中国区的人民币价(元)。⚠️ 外部资料换算(App Store 中国区价格矩阵:
    /// $0.99/4.99/9.99/14.99/24.99/49.99/99.99 → ¥6/30/68/98/163/328/648),客户端里没有这项数据,未能在二进制内核实;
    /// 想换口径直接改这张表。
    pub cny_yuan: u32,
}

/// [补完 2026-09-15] 100_0.dat 的 7 个充值档位(按 itemid 升序)。
pub const SHELL_PACKS: [ShellPack; 7] = [
    ShellPack { item_id: 1, shells: 20, usd_cents: 99, cny_yuan: 6 },
    ShellPack { item_id: 2, shells: 105, usd_cents: 499, cny_yuan: 30 },
    ShellPack { item_id: 3, shells: 225, usd_cents: 999, cny_yuan: 68 },
    ShellPack { item_id: 4, shells: 370, usd_cents: 1499, cny_yuan: 98 },
    ShellPack { item_id: 5, shells: 650, usd_cents: 2499, cny_yuan: 163 },
    ShellPack { item_id: 6, shells: 1500, usd_cents: 4999, cny_yuan: 328 },
    ShellPack { item_id: 7, shells: 3500, usd_cents: 9999, cny_yuan: 648 },
];

/// [补完 2026-09-15] 按 onBuyVIPGold: 的参数(itemid)查档位;itemid 8(广告墙「免费贝壳」格,
/// -[NewStyleStoreItemsView loadResourceItems]@0x3b998a setItemid:8)等非充值参数返回 None。
pub fn shell_pack(item_id: u32) -> Option<ShellPack> {
    SHELL_PACKS.iter().copied().find(|p| p.item_id == item_id)
}

/// [补完 2026-09-15] 客户端 VIP 值的单位是 0.1 元:-[VIPLayer showWithTarget:selector:] 显示
/// MONEY_NEEDED_TO_NEXT_VIP_LEVEL「达到 VIP %d 您还需充值 %d 元」时,元数 = (vipValueOfNextLevel − vipValue) / 10
/// (0x37f240 sub、0x37f24e umull 0xCCCCCCCD、0x37f256 lsrs #3);进度条 = vipValue × 100 / vipValueOfNextLevel(0x37f13c)。
/// 所以 vipValueOfNextLevel 是「升到下一级所需的累计总额」,不是差额。
const VIP_VALUE_PER_YUAN: i64 = 10;

/// [补完 2026-09-15] 客户端实际支持的最高 VIP 等级是 4,不是 250_1.dat 的 6 行:
/// -[NetworkManager parseVipInfo:pos:len:] 把服务器下发等级 clamp 到 4(0x1c0c8e)、VIP4 时把 next 置 0(0x1c0d38);
/// -[GameData loadVipUserInfoData]@0x73e08 只读 250_1.dat 前 4 行(0x7416e cmp r5,#4,getVipInfoDataWithLevel:5/6 取不到);
/// -[VIPLayer showWithTarget:selector:] 显示也 clamp 到 4(0x37f03e)。与 mole_cheats 的 VIP_LEVEL_MAX 一致。
const VIP_CLIENT_MAX_LEVEL: usize = 4;

/// [补完 2026-09-15] 默认升级门槛:累计充值元数,依次为 VIP1..VIP4。
/// ⚠️ 移植者自拟,非原版数据——原版门槛只在淘米服务器(parseVipInfo 直接收服务器算好的三值),客户端没有任何门槛表。
/// 取值思路(保守):VIP1 = 最小档 ¥6,呼应客户端文案 CHARGE_FOR_VIP_HINT「只要充值到 VIP %d,即可获得首充大礼包」
/// (VIPLayer 在 vipValue==0 时以 level+1 填 %d,0x37f39e);之后逐级明显拉开,单笔最大档 ¥648 只到 VIP3。
const VIP_DEFAULT_THRESHOLDS_YUAN: [u32; VIP_CLIENT_MAX_LEVEL] = [6, 100, 500, 2000];

/// [补完 2026-09-15] 单个门槛的上限(元):×10 换算成 VIP 值后仍装得进 i32。
const VIP_THRESHOLD_YUAN_MAX: u32 = 200_000_000;

/// [补完 2026-09-15] 解析 MOLE_VIP_THRESHOLDS:逗号分隔的累计充值元数(正整数、严格递增,依次为 VIP1、VIP2…),
/// 空段忽略,也接受全角逗号。Err(原因) 时调用方回落默认表;多于 4 个由调用方截断。纯函数,不打日志。
fn parse_vip_thresholds(raw: &str) -> Result<Vec<u32>, String> {
    let mut out: Vec<u32> = Vec::new();
    for part in raw.split(|c: char| c == ',' || c == '，') {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        let v: u32 = t.parse().map_err(|_| format!("「{}」不是正整数", t))?;
        if v == 0 || v > VIP_THRESHOLD_YUAN_MAX {
            return Err(format!("「{}」超出范围 1..={}", t, VIP_THRESHOLD_YUAN_MAX));
        }
        if let Some(&prev) = out.last() {
            if v <= prev {
                return Err(format!("门槛必须严格递增({} 之后是 {})", prev, v));
            }
        }
        out.push(v);
    }
    if out.is_empty() {
        return Err("没有任何数字".to_string());
    }
    Ok(out)
}

/// [补完 2026-09-15] 本会话生效的升级门槛(单位同 VIP 值 = 0.1 元),第 i 项是 VIP(i+1) 的门槛。
/// 第一次用到时读一次 MOLE_VIP_THRESHOLDS 并打日志,之后不再变化。
fn vip_thresholds() -> &'static [i32] {
    static TH: OnceLock<Vec<i32>> = OnceLock::new();
    TH.get_or_init(|| {
        let default = VIP_DEFAULT_THRESHOLDS_YUAN.to_vec();
        let (yuan, source) = match std::env::var("MOLE_VIP_THRESHOLDS") {
            Ok(raw) if !raw.trim().is_empty() => match parse_vip_thresholds(&raw) {
                Ok(mut v) => {
                    if v.len() > VIP_CLIENT_MAX_LEVEL {
                        log!(
                            "[MOLEITEMS] ⚠️ MOLE_VIP_THRESHOLDS 给了 {} 个门槛,客户端最高只到 VIP{},多出的忽略",
                            v.len(),
                            VIP_CLIENT_MAX_LEVEL
                        );
                        v.truncate(VIP_CLIENT_MAX_LEVEL);
                    }
                    (v, "环境变量 MOLE_VIP_THRESHOLDS")
                }
                Err(why) => {
                    log!(
                        "[MOLEITEMS] ⚠️ MOLE_VIP_THRESHOLDS=「{}」无效({}),改用默认门槛",
                        raw,
                        why
                    );
                    (default, "默认(环境变量无效)")
                }
            },
            _ => (default, "默认"),
        };
        let desc: Vec<String> = yuan
            .iter()
            .enumerate()
            .map(|(i, y)| format!("VIP{} ≥ ¥{}", i + 1, y))
            .collect();
        log!(
            "[MOLEITEMS] VIP 升级门槛(移植者自拟,非原版数据;原版门槛在服务器,客户端无表):{}(累计充值,来源:{})",
            desc.join(" / "),
            source
        );
        yuan.iter()
            .map(|&y| (y as i64 * VIP_VALUE_PER_YUAN) as i32)
            .collect()
    })
}

/// [补完 2026-09-15] 纯函数:累计 VIP 值 → (等级, 下一级门槛)。thresholds 单位同 VIP 值、严格递增。
/// - 等级只升不降:取 old_level 与门槛推导值的较大者。原版 parseVipInfo 发现下发等级低于本地会
///   showCheatWarningMessage 并拒收(0x1c0c96),等级在客户端眼里本就单调不降。
/// - 到 VIP4(或门槛表用完)时下一级门槛写 0,与原版 parseVipInfo 在 VIP4 把 next 置 0(0x1c0d38)一致;
///   VIPLayer 看到 next==0 走满级分支(0x37f136),不会除以 0。
fn vip_progress(old_level: i32, value: i32, thresholds: &[i32]) -> (i32, i32) {
    let derived = thresholds
        .iter()
        .take_while(|&&t| value >= t)
        .count()
        .min(VIP_CLIENT_MAX_LEVEL) as i32;
    let level = old_level.max(derived);
    let next = if level >= VIP_CLIENT_MAX_LEVEL as i32 {
        0
    } else {
        thresholds.get(level as usize).copied().unwrap_or(0)
    };
    (level, next)
}

/// [补完 2026-09-15] 离线假购买后替原版服务器做「累计充值 → VIP 等级」。
/// 原版链路:-[InAppPurchaseManager onPurchaseSuccessful]@0x117a70 从 productIdentifier 截出 saleN 的 N(0x117d74,N ≤ 6),
/// 发 -[NetworkManager sendCostMoneyInfoToServerWithUserId:andNumber:]@0xeabb4(8B userId+N),紧接 getVipInfo@0xeac2c;
/// 服务器回包进 -[NetworkManager parseVipInfo:pos:len:]@0x1c0b9c,按 setVipLevel: → setVipValue: → setVipValueOfNextLevel:
/// (0x1c0cea/0x1c0d1c/0x1c0d5a)写本地 UserVIPInfoData。离线这一段由这里在宿主侧补上:
/// ① 旧值只取 vip.dat 侧档(vip_hook 经原版 setter 记下的真实值),绝不读 getter:强制 VIP 开着时
///    vipLevelWithNewType/vipValue 会被 mole_cheats 改写成强制值(vipValue 固定 999999),读 getter 会把强制值累加进侧档;
/// ② 本次增量 = 档位人民币价 × 10(VIP 值单位 0.1 元,见 VIP_VALUE_PER_YUAN);
/// ③ 等级/下一级门槛由 vip_progress 按 vip_thresholds()(移植者自拟,非原版数据)算出;
/// ④ 用 vip_apply 按 parseVipInfo 的顺序调三个原版 setter(setter 自己 CryptUtils 加密),不置 VIP_BUSY,
///    让 vip_hook 自然记账并原子落盘;万一没记上(比如本地 VIP 对象还没建),直接写侧档,下次读 VIP 时由 vip_hook 注入。
/// [补完 2026-09-15] 待补项(有意省略,不是原版不弹):parseVipInfo 的首充大礼包。
/// 原版进村 -[GameManager startGame:]+0xb38(0x19ca0)每次都发 getVipInfo,没充过值的玩家也会先收到一次 VIP 信息,
/// 本地旧 next 不为 0;第一次真充值后 parseVipInfo 在 0x1c0e34-0x1c0e48 判定「旧等级 [sp+4]=0、旧 VIP 值 [sp+8]=0、
/// 旧 next [sp+0x18]≠0、新 VIP 值 [sp+0x20]≠0」成立,调 [[WrapperManager sharedManager] initFirstChargeGifts],
/// 再 [FirstChargeGiftsLayer layerWithRewards:[wm firstChargeGiftsArray]] showWithTarget:NetworkManager selector:nil。
/// 所以原版每个玩家第一次充值都会弹首充礼包(-[WrapperManager initFirstChargeGifts]@0x262d58:702×2000、704×10、22022、22023);
/// 移植版第一次假购买只发贝壳、升 VIP1,没有礼包。
/// 暂不复刻的原因(都没法无头验证):① -[FirstChargeGiftsLayer showWithTarget:selector:]@0x3803cc 会取 currentUiLayer 的
/// tag 6、tag 5 子层(0x380444/0x38048e getChildByTag:)调 performSelector:detach;这里还在商店购买按钮回调的调用栈上,
/// 若商店正挂在这两个 tag 上,当场拆掉可能留下悬空对象(原版是异步回包触发,不在按钮栈上);
/// ② 领礼物走 releaseFirstChargeGift:@0x2630a4 → onAddFirstChargeGiftToMap:@0x262f2c 把物品摆到地图上,
/// 黄金岛会话里、商店开着时能否正常摆放不清楚;③ 关闭按钮 onButtonCloseSelected@0x3806e4 还要弹「放弃礼包」确认框。
/// 以后要补:锁存「应弹首充」标志,等商店关闭、回到主村安全点再按上面的顺序调用,先用开关门控、无头验证弹层能关再默认打开。
/// 调用方已判离线;本函数发宿主 msg_send 但不保存/恢复 r0-r3(由 on_shells_purchased 统一做)。
fn vip_accumulate(env: &mut Environment, item_id: u32, shells: i32, pack: Option<ShellPack>) {
    let Some(pack) = pack else {
        log!(
            "[MOLEITEMS] VIP 累计:onBuyVIPGold: 参数 {} 不是 100_0.dat 的充值档位(本次发了 {} 贝壳),不计入 VIP 值",
            item_id,
            shells
        );
        return;
    };
    let thresholds = vip_thresholds();
    side_ensure_loaded(env);
    let old = side().vip.unwrap_or(VipVals {
        level: 0,
        value: 0,
        next: 0,
    });
    let add = pack.cny_yuan as i64 * VIP_VALUE_PER_YUAN;
    let value = (old.value.max(0) as i64 + add).min(i32::MAX as i64) as i32;
    let (level, next) = vip_progress(old.level, value, thresholds);
    let new = VipVals { level, value, next };

    let obj = local_vip_object(env);
    if obj != nil {
        vip_apply(env, obj, new);
    }
    let recorded = side().vip == Some(new);
    if !recorded {
        {
            let mut s = side();
            s.vip = Some(new);
        }
        side_save(env);
        log!(
            "[MOLEITEMS] ⚠️ VIP 累计:本地 UserVIPInfoData {}未经 setter 记账,已直接写入 {}(下次读 VIP 时注入)",
            if obj == nil { "还没建好," } else { "" },
            SIDE_FILE
        );
    }

    let force_note = if crate::mole_cheats::is_on("force_vip") {
        ";强制 VIP 开着:界面仍显示强制值(next 也按满级返回 0),这里只累计真实值,强制值不写进侧档"
    } else {
        ""
    };
    log!(
        "[MOLEITEMS] VIP 累计:itemid {}(sale{},{} 贝壳,标价 ${}.{:02},按 App Store 中国区同档 ¥{} 计)→ vipValue {} → {}(单位 0.1 元),VIP{} → VIP{},下一级门槛 {}(门槛为移植者自拟,非原版数据){}",
        pack.item_id,
        pack.item_id.saturating_sub(1),
        pack.shells,
        pack.usd_cents / 100,
        pack.usd_cents % 100,
        pack.cny_yuan,
        old.value,
        new.value,
        old.level,
        new.level,
        new.next,
        force_note
    );
    if new.level > old.level {
        log!(
            "[MOLEITEMS] VIP 升级:VIP{} → VIP{}(累计充值 ¥{},门槛为移植者自拟,非原版数据)",
            old.level,
            new.level,
            new.value as i64 / VIP_VALUE_PER_YUAN
        );
    }
}

/// objc/messages.rs 的 SHELLHOOK 发完贝壳后调用。离线专属;内部发宿主 msg_send,返回前恢复 r0-r3。
/// [补完 2026-09-15] 参数扩展:item_id = onBuyVIPGold: 的参数(ShopItemData.itemid);shells = 本次实发贝壳数;
/// pack = shell_pack(item_id) 查到的档位(含标价),非充值参数(如 itemid 8 免费贝壳格)为 None。
/// 先补充值解锁物(F2-1),再做 VIP 累计升级——与原版 onPurchaseSuccessful 先本地发贝壳/解锁、
/// VIP 三值要等服务器回包(parseVipInfo)才写入的先后一致。
pub fn on_shells_purchased(
    env: &mut Environment,
    item_id: u32,
    shells: i32,
    pack: Option<ShellPack>,
) {
    if env.options.network_access {
        return;
    }
    let saved = save_regs(env);
    recharge_unlock_side_effects(env);
    vip_accumulate(env, item_id, shells, pack);
    restore_regs(env, saved);
}

/// [扫描修 2026-09-15] F2-1 充值解锁物:补上原版「活动期充值」应有的副作用(由 on_shells_purchased 调用)。
/// 原版 -[GameData addAlreadyPurchaseVipgoldWithPurchaseInfo:]:canShowADForExchange(服务器 1064/1182 活动开关)
/// bit31 置位时 gamedataFlag|=0x20(0x7f3bc),bit29 置位时 gamedataFlag|=0x10 并 unlockItem:16283(0x7f438/0x7f458),
/// 随后 saveToLocal(0x7f3dc/0x7f476)。离线没有活动开关,也绕过了 IAP,这里按"活动期充值"补齐:
/// ① unlockedItemList 为 nil 时先建空数组(-[WrapperManager unlockItem:]@0x261336 遇 nil 直接返回);
/// ② gamedataFlag 只 OR 0x30(bit0 是新信件标志,endLoadMap/parseData 在用,绝不整体覆写);
/// ③ unlockItem:16283(都教授,与原版逐字对齐)+ unlockItem:14974(克劳神父:客户端没有任何解锁它的代码,
///    原版只能来自服务器 1001 地图里的 unlockedItemList——这里是模拟服务器下发,非原版客户端路径);
/// ④ [GameData saveToLocal]:gamedataFlag 存 map.dat 键 "13"、unlockedItemList 存键 "63"(saveMapData:@0x78374/0x78f02)。
/// 不改 canShowADForExchange_(它还驱动广告墙/免费贝壳/评分弹窗)。离线专属。
/// [补完 2026-09-15] 从 on_shells_purchased 拆出:行为不变;发宿主 msg_send 但不保存/恢复 r0-r3(由调用方统一做)。
fn recharge_unlock_side_effects(env: &mut Environment) {
    let gd = shared(env, "GameData", "sharedInstance");
    let wm = shared(env, "WrapperManager", "sharedManager");
    if gd == nil || wm == nil {
        return;
    }
    let list_s = sel_of(env, "unlockedItemList");
    let list: id = msg_send(env, (gd, list_s));
    if list == nil {
        let cls = env.objc.get_known_class("NSMutableArray", &mut env.mem);
        let alloc_s = sel_of(env, "alloc");
        let init_s = sel_of(env, "init");
        let arr: id = msg_send(env, (cls, alloc_s));
        let arr: id = msg_send(env, (arr, init_s));
        let set_s = sel_of(env, "setUnlockedItemList:");
        // setUnlockedItemList:@0x8c3b4 是 objc_setProperty(retain),设完释放我们的 +1。
        let _: () = msg_send(env, (gd, set_s, arr));
        release(env, arr);
    }
    let get_f = sel_of(env, "gamedataFlag");
    let flag: u32 = msg_send(env, (wm, get_f));
    if flag & 0x30 != 0x30 {
        let set_f = sel_of(env, "setGamedataFlag:");
        let _: () = msg_send(env, (wm, set_f, flag | 0x30));
    }
    let unlock_s = sel_of(env, "unlockItem:");
    let _: () = msg_send(env, (wm, unlock_s, 16283i32));
    let _: () = msg_send(env, (wm, unlock_s, 14974i32));
    let save_s = sel_of(env, "saveToLocal");
    let _: () = msg_send(env, (gd, save_s));
    log!(
        "[MOLEITEMS] 充值副作用:gamedataFlag {:#x} → {:#x},解锁 16283 都教授/14974 克劳神父(乐乐水塔/织女鹊桥锁同时解除)",
        flag,
        flag | 0x30
    );
}

// ============================================================================
// 按 ID 入仓库 / 放置(F4-1 / F7-11 / F7-3)
// ============================================================================

/// 不支持直接发放的类型:18 礼盒、23 么么公主、24 乐乐侠(会 addNpc: 生成 NPC)、27 道具/扩地/碎片、38 拍照相框;
/// 以及 90000+ 相框资源 ID(F4-1 value_note)。
const NON_GIVABLE_TYPES: [i32; 5] = [18, 23, 24, 27, 38];

fn object_data(env: &mut Environment, island: bool, item: u32) -> id {
    let data = if island {
        shared(env, "NewSceneData", "sharedInstance")
    } else {
        shared(env, "GameData", "sharedInstance")
    };
    if data == nil {
        return nil;
    }
    let s = sel_of(env, "getObjectDataWithId:");
    msg_send(env, (data, s, item as i32))
}

fn object_name(env: &mut Environment, od: id, item: u32) -> String {
    let s = sel_of(env, "name");
    let n: id = msg_send(env, (od, s));
    if n == nil {
        return format!("物品 {}", item);
    }
    let name = ns_string::to_rust_string(env, n).into_owned();
    if name.is_empty() {
        format!("物品 {}", item)
    } else {
        name
    }
}

fn check_givable(env: &mut Environment, od: id, item: u32) -> Result<(), String> {
    let s = sel_of(env, "type");
    let t: i32 = msg_send(env, (od, s));
    if item >= 90000 || NON_GIVABLE_TYPES.contains(&t) {
        Err(format!(
            "物品 {} 是特殊类型(type {}),不支持直接发放",
            item, t
        ))
    } else {
        Ok(())
    }
}

/// 往仓库加 count 个物品 id(走原版 ObjectManager addGoodsNumber:andCount: + 存盘)。返回给菜单 toast 的文案。
/// -[ObjectManager addGoodsNumber:(int) andCount:(int)]@0x42ea0 自己用 [WrapperManager getObjectDataWithId:] 校验 ID、
/// 写 goodsInStorage_(+296);goodsInStorage 随 map.dat 落盘,所以之后调 [GameData saveMapData]@0x76804(无需 saveUserInfoData)。
/// 仓库只属于主村;须在可发消息的上下文调用(非 drawScene/mainLoop 帧栈);内部自己快照/恢复 r0-r3。
pub fn give_goods(env: &mut Environment, id: u32, count: u32) -> Result<String, String> {
    if env.options.network_access {
        return Err("在线模式不提供发放物品(以私服数据为准)".to_string());
    }
    if crate::mole_cheats::island_session_active() {
        return Err("仓库属于主村:请回主村后再发放".to_string());
    }
    let count = count.clamp(1, 999);
    let saved = save_regs(env);
    let result = give_goods_inner(env, id, count);
    restore_regs(env, saved);
    result
}

fn give_goods_inner(env: &mut Environment, item: u32, count: u32) -> Result<String, String> {
    let gm = shared(env, "GameManager", "sharedManager");
    let ui_s = sel_of(env, "villageUILayer");
    let layer: id = if gm != nil {
        msg_send(env, (gm, ui_s))
    } else {
        nil
    };
    if layer == nil {
        return Err("还没进主村,暂时不能发放".to_string());
    }
    let od = object_data(env, false, item);
    if od == nil {
        return Err(format!("主村物品表里没有 ID {}", item));
    }
    check_givable(env, od, item)?;
    let name = object_name(env, od, item);
    let om = shared(env, "ObjectManager", "sharedManager");
    if om == nil {
        return Err("ObjectManager 未就绪".to_string());
    }
    let add_s = sel_of(env, "addGoodsNumber:andCount:");
    let _: () = msg_send(env, (om, add_s, item as i32, count as i32));
    let gd = shared(env, "GameData", "sharedInstance");
    if gd != nil {
        let save_s = sel_of(env, "saveMapData");
        let _: () = msg_send(env, (gd, save_s));
    }
    log!("[MOLEITEMS] 入仓库:{}({}) ×{},已 saveMapData", name, item, count);
    Ok(format!("已放入仓库:{} ×{}(打开仓库取出摆放)", name, count))
}

/// 把物品 id 直接放到当前场景地图上(主村/黄金岛各走原版放置路径)。
/// 不在可放置状态(gameMode≠1、菜单层为 nil)时返回 Err 文案。内部自己快照/恢复 r0-r3。
pub fn place_item(env: &mut Environment, id: u32) -> Result<String, String> {
    if env.options.network_access {
        return Err("在线模式不提供直接放置(以私服数据为准)".to_string());
    }
    let saved = save_regs(env);
    let result = place_item_route(env, id);
    restore_regs(env, saved);
    result
}

/// [复核修 2026-09-15] R5-2:按场景号路由,不再只看 island_session_active()。
/// 根因:island_session_active() 除 ON_ISLAND 外还含 ISLAND_ENTER_WINDOW(enterNewIslands 放行时置 1200 帧,
/// 这段时间画面仍在主村)、ISLAND_LOADING、ISLAND_EXITING 三个过渡标志。窗口内主村的放置请求会被送进
/// place_item_island,读上一次岛会话残留的 NewGameManager(curScene_ 由 objc_setProperty 持有,unloadMap 不清):
/// 要么报误导性的"没找到黄金岛菜单层",要么对已卸载的岛菜单层发 onAddExchangeRewardToMap:(gameMode 被改成 15)。
/// 场景号取法与原版 -[GameData saveToLocal:] 相同(0x7ca9c/0x7caaa):+[SceneMannager sharedManager]@0x240cec
/// (懒建单例,init@0x240c20 只把各 ivar 清零)→ -[SceneMannager curSceneId]@0x241730,返回 int ivar curSceneId_(+12):
/// 1 = 主村,10 = 黄金岛,2 = 切场景过场/加载中。在岛上 mole_cheats 会把卡在过场态的 curSceneId 强制为 10
/// (宿主 msg_send 同样经过 objc_msgSend 钩子)。调用方 place_item 负责快照/恢复 r0-r3。
fn place_item_route(env: &mut Environment, id: u32) -> Result<String, String> {
    let sm = shared(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return Err("场景管理器未就绪:请进入主村或黄金岛后再放置".to_string());
    }
    let cur_s = sel_of(env, "curSceneId");
    let cur: i32 = msg_send(env, (sm, cur_s));
    match cur {
        10 => place_item_island(env, id),
        1 if !crate::mole_cheats::island_session_active() => place_item_main(env, id),
        1 | 2 => Err("场景切换中(正在进出黄金岛或加载),请稍后再放置".to_string()),
        _ => Err(format!(
            "当前场景(curSceneId={})不支持直接放置:请在主村或黄金岛地图上使用",
            cur
        )),
    }
}

/// 主村:走原版奖励放置入口 -[VillageMenuLayer onAddIceCreamRewardToMap:(int)]@0x6606c
/// (即 [[WrapperManager sharedManager] onAddIceCreamRewardToMap:]@0x265bb8 在 curSceneId==1 时转发的目标):
/// `[GameManager setGameMode:15]` → 写 cuObjectId_(+236)=ID → `addNewObject2Map:ID gift:YES`。
/// 不能直接调 addNewObject2Map:gift::它内部把 cuObjectId_ 当作交给 EditMenuLayer 的物品 ID(0x64ebe/0x65002/0x65b18),
/// 不先写 cuObjectId_ 会摆出上一次的物品。gift=YES 跳过 showCostGoldView: 扣费(0x64e48);
/// 放下后 -[VillageMenuLayer onEditMenuClosed] 在 gameMode==15 时跳过"继续购买"分支(0x636ca),
/// 也不触发首充/水塔/活动礼包的连发链(那些是 14/16/17),与冰淇淋/兑换奖励同一条干净路径。
fn place_item_main(env: &mut Environment, item: u32) -> Result<String, String> {
    let gm = shared(env, "GameManager", "sharedManager");
    if gm == nil {
        return Err("还没进主村,暂时不能放置".to_string());
    }
    let ui_s = sel_of(env, "villageUILayer");
    let layer: id = msg_send(env, (gm, ui_s));
    if layer == nil || !is_kind_of(env, layer, "VillageMenuLayer") {
        return Err("当前不在主村界面,无法放置".to_string());
    }
    let mode_s = sel_of(env, "gameMode");
    let mode: i32 = msg_send(env, (gm, mode_s));
    if mode != 1 {
        return Err(format!(
            "庄园正处于其它操作状态(gameMode={}),请先关闭商店/退出编辑再放置",
            mode
        ));
    }
    let od = object_data(env, false, item);
    if od == nil {
        return Err(format!("主村物品表里没有 ID {}", item));
    }
    check_givable(env, od, item)?;
    let name = object_name(env, od, item);
    let put_s = sel_of(env, "onAddIceCreamRewardToMap:");
    let _: () = msg_send(env, (layer, put_s, item as i32));
    log!("[MOLEITEMS] 主村放置:{}({}) 走 onAddIceCreamRewardToMap:(gift=YES)", name, item);
    Ok(format!(
        "已发放「{}」:建筑/装饰进入摆放模式,点地块放下;动物/NPC 直接出现",
        name
    ))
}

/// 黄金岛:NewSceneVillageMenuLayer 由 -[GameNewScene addMainVillageLayer:]@0x23ee36 以 `addChild:z:2 tag:2` 挂到场景上,
/// 场景由 [[NewGameManager sharedManager] setCurScene:](0x23edc6)登记,所以取 curScene 的 tag 2 子节点;
/// 找不到时退回遍历 curScene 的直接子节点按类名匹配。放置走岛上的原版奖励入口
/// -[NewSceneVillageMenuLayer onAddExchangeRewardToMap:(int)]@0x25d0f8:ID<1 直接返回 → 写 cuObjectId_(+236)=ID →
/// `[[NewGameManager sharedManager] setGameMode:15]` → `addNewObject2Map:ID gift:YES`。
/// 与主村同理不能直接调 addNewObject2Map:gift:(它读 cuObjectId_ 作为摆放物 ID,0x25be76/0x25c732)。
fn place_item_island(env: &mut Environment, item: u32) -> Result<String, String> {
    let ngm = shared(env, "NewGameManager", "sharedManager");
    if ngm == nil {
        return Err("黄金岛尚未就绪".to_string());
    }
    let mode_s = sel_of(env, "gameMode");
    let mode: i32 = msg_send(env, (ngm, mode_s));
    if mode != 1 {
        return Err(format!(
            "黄金岛正处于其它操作状态(gameMode={}),请先关闭商店/退出编辑再放置",
            mode
        ));
    }
    let cs = sel_of(env, "curScene");
    let scene: id = msg_send(env, (ngm, cs));
    if scene == nil {
        return Err("黄金岛场景尚未就绪".to_string());
    }
    let tag_s = sel_of(env, "getChildByTag:");
    let mut layer: id = msg_send(env, (scene, tag_s, 2i32));
    if !is_kind_of(env, layer, "NewSceneVillageMenuLayer") {
        layer = nil;
        let ch_s = sel_of(env, "children");
        let children: id = msg_send(env, (scene, ch_s));
        if children != nil {
            let cnt_s = sel_of(env, "count");
            let oai_s = sel_of(env, "objectAtIndex:");
            let n: u32 = msg_send(env, (children, cnt_s));
            for i in 0..n.min(64) {
                let c: id = msg_send(env, (children, oai_s, i));
                if is_kind_of(env, c, "NewSceneVillageMenuLayer") {
                    layer = c;
                    break;
                }
            }
        }
    }
    if layer == nil {
        return Err("没找到黄金岛菜单层(NewSceneVillageMenuLayer),无法放置".to_string());
    }
    let od = object_data(env, true, item);
    if od == nil {
        return Err(format!("黄金岛物品表里没有 ID {}", item));
    }
    check_givable(env, od, item)?;
    let name = object_name(env, od, item);
    let put_s = sel_of(env, "onAddExchangeRewardToMap:");
    let _: () = msg_send(env, (layer, put_s, item as i32));
    log!("[MOLEITEMS] 岛上放置:{}({}) 走 onAddExchangeRewardToMap:(gift=YES)", name, item);
    Ok(format!(
        "已发放「{}」到黄金岛:建筑/装饰进入摆放模式,点地块放下",
        name
    ))
}

// ============================================================================
// 钩子入口
// ============================================================================

/// 本模块是否要拦截这个 (类, 选择子)。会被 OR 进 mole_cheats::intercept_wants,只做字符串比较。
pub fn wants(class: &str, sel: &str) -> bool {
    match class {
        "NewStyleStoreItemsView" => sel == "loadObjectsDataByType:",
        "AvatarLayer" => sel == "checkRequiredID:",
        "GameData" => sel == "onlineTimer" || sel == "loginTimesCounter",
        // [复核修 2026-09-15] R5-3:startGame → startGame:(startGame 自己转调 startGame:,另有入口直发 startGame:1)。
        "GameManager" => sel == "startGame:",
        "UserVIPInfoData" => matches!(
            sel,
            "vipLevelWithNewType"
                | "vipValue"
                | "vipValueOfNextLevel"
                | "setVipLevel:"
                | "setVipValue:"
                | "setVipValueOfNextLevel:"
                | "reset"
        ),
        "iMoleVillageAppDelegate" => matches!(
            sel,
            "applicationWillResignActive:"
                | "applicationWillTerminate:"
                | "applicationDidBecomeActive:"
        ),
        _ => false,
    }
}

/// 前置拦截。None = 不归本模块管(或只做了旁路副作用,交给后续钩子/真方法);Some(true) = 已吞掉调用(r0 已写好);
/// Some(false) = 做完副作用后放行真方法(发过宿主 msg_send 的分支已恢复 r0-r3)。
pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> Option<bool> {
    match class {
        "NewStyleStoreItemsView" if sel == "loadObjectsDataByType:" => store_hook(env),
        // F5-8:250_0 头像 40-46 的 requireid 6009 么么公主的别墅 / 6010 花艺工作室锁。
        // -[AvatarLayer checkRequiredID:]@0xfedb8 = [[ObjectManager sharedManager] objectCount:id type:1] > 0,纯判定无副作用,
        // 唯一调用点 -[AvatarLayer test]@0xfe04a(头像网格构建)。与 mole_cheats 的 checkRequiredVipLevel: 臂语义一致:返回 YES。
        "AvatarLayer" if sel == "checkRequiredID:" => {
            if crate::mole_cheats::is_on("all_unlock") {
                env.cpu.regs_mut()[0] = 1;
                Some(true)
            } else {
                None
            }
        }
        "GameData" if sel == "onlineTimer" || sel == "loginTimesCounter" => stats_getter(env, sel),
        "GameManager" if sel == "startGame:" => {
            if !env.options.network_access {
                on_enter_village(env);
            }
            None
        }
        "UserVIPInfoData" => vip_hook(env, sel),
        "iMoleVillageAppDelegate" => app_lifecycle(env, sel),
        _ => None,
    }
}


// ===== 以下静态表由离线脚本从解密数据表与 iPad 图集生成(勿手改;改规则请重新生成)=====
// 数据源:解密 property.dat / propertyHV.dat / zh-Hans 描述表 / 240_0 / 250_2 / 340_0;
// 美术判定:包内 *.plist 图集 frames 有 md5("<ID>").png,且该图集是本场景在售物品实际使用的 iPad 图集
//   (主村:scenesRoadItemsiPad.plist、scenesShareStore1iPad.plist、scenesShareStore2iPad.plist、store1iPad.plist;岛:hv_store2iPad.plist、scenesRoadItemsiPad.plist、scenesShareStore1iPad.plist、scenesShareStore2iPad.plist)。
/// 主村隐藏物品上架表:(物品 ID, 桶 1=建设庄园/2=美化庄园, 子分页 1..6)。按 ID 升序,二分查找。共 158 条。
const MAIN_SHOP: &[(u32, u8, u8)] = &[
    (5010, 1, 1), (5011, 1, 1), (5012, 1, 1), (14032, 2, 4), (14061, 2, 4), (14062, 2, 4),
    (14110, 2, 4), (14153, 2, 4), (14154, 2, 4), (14155, 2, 4), (14158, 2, 4), (14160, 2, 4),
    (14161, 2, 4), (14169, 2, 4), (14170, 2, 4), (14175, 2, 4), (14179, 2, 4), (14180, 2, 4),
    (14184, 2, 4), (14188, 2, 4), (14194, 2, 4), (14195, 2, 4), (14208, 2, 4), (14209, 2, 4),
    (14230, 2, 1), (14231, 2, 1), (14232, 2, 1), (14291, 2, 4), (14502, 2, 4), (14505, 2, 4),
    (14525, 2, 4), (14801, 2, 4), (14802, 2, 4), (14805, 2, 4), (14806, 2, 4), (14809, 2, 4),
    (14810, 2, 4), (14901, 2, 4), (14907, 2, 4), (14908, 2, 4), (14911, 2, 4), (14926, 1, 3),
    (14927, 1, 3), (14928, 1, 3), (14929, 1, 3), (14933, 2, 6), (14937, 2, 6), (14939, 2, 5),
    (14946, 2, 5), (14953, 2, 4), (14956, 2, 4), (14968, 2, 4), (14969, 2, 4), (14978, 1, 1),
    (14982, 2, 5), (14983, 2, 5), (16046, 1, 3), (16103, 1, 3), (16131, 2, 4), (16132, 2, 4),
    (16133, 2, 4), (16137, 2, 6), (16138, 2, 3), (16139, 2, 1), (16141, 2, 2), (16142, 2, 4),
    (16143, 2, 4), (16144, 2, 5), (16148, 2, 1), (16149, 2, 2), (16150, 2, 5), (16151, 2, 5),
    (16152, 2, 6), (16153, 2, 6), (16154, 2, 6), (16155, 2, 6), (16179, 2, 4), (16180, 2, 3),
    (16181, 1, 1), (16182, 2, 1), (16183, 2, 1), (16184, 2, 1), (16186, 2, 5), (16187, 2, 4),
    (16188, 2, 4), (16189, 2, 4), (16190, 2, 4), (16191, 2, 5), (16192, 2, 6), (16193, 2, 4),
    (16194, 2, 4), (16195, 2, 4), (16196, 2, 4), (16197, 2, 4), (16198, 2, 4), (16212, 1, 3),
    (16214, 2, 5), (16222, 2, 4), (16223, 2, 4), (16224, 2, 4), (16225, 2, 5), (16226, 2, 5),
    (16227, 2, 5), (16228, 2, 6), (16229, 2, 1), (16313, 2, 4), (16314, 2, 4), (16393, 2, 4),
    (16394, 2, 4), (16395, 2, 4), (16396, 2, 3), (16397, 2, 3), (16398, 2, 5), (16399, 2, 5),
    (16400, 2, 3), (16401, 2, 3), (16402, 2, 3), (16403, 2, 3), (16404, 2, 4), (17003, 2, 4),
    (17005, 2, 4), (17011, 2, 4), (17014, 2, 4), (17015, 2, 4), (17018, 2, 4), (17020, 2, 4),
    (17106, 2, 4), (17107, 2, 4), (17108, 2, 4), (18004, 2, 4), (18005, 2, 4), (18006, 2, 4),
    (18007, 2, 4), (18008, 2, 4), (18009, 2, 4), (18010, 2, 4), (18011, 2, 4), (18012, 2, 4),
    (18101, 2, 4), (18102, 2, 4), (18103, 2, 4), (18104, 2, 4), (18105, 2, 4), (18106, 2, 4),
    (18107, 2, 4), (18108, 2, 4), (18109, 2, 4), (18110, 2, 4), (18111, 2, 4), (18112, 2, 4),
    (18113, 2, 4), (18114, 2, 4), (18115, 2, 4), (18116, 2, 4), (22014, 2, 4), (22028, 2, 4),
    (22029, 2, 4), (22030, 2, 4),
];
/// 黄金岛隐藏物品上架表:(物品 ID, 桶 1=建设庄园/2=美化庄园, 子分页 1..6)。按 ID 升序,二分查找。共 57 条。
const ISLAND_SHOP: &[(u32, u8, u8)] = &[
    (14160, 2, 4), (14183, 2, 4), (16133, 2, 4), (16137, 2, 6), (16138, 2, 3), (16139, 2, 1),
    (16141, 2, 2), (16142, 2, 4), (16143, 2, 4), (16144, 2, 5), (16148, 2, 1), (16149, 2, 2),
    (16150, 2, 5), (16151, 2, 5), (16152, 2, 6), (16153, 2, 6), (16154, 2, 6), (16155, 2, 6),
    (16179, 2, 4), (16180, 2, 4), (16182, 2, 1), (16183, 2, 1), (16184, 2, 1), (16186, 2, 4),
    (16187, 2, 4), (16188, 2, 4), (16189, 2, 4), (16190, 2, 4), (16191, 2, 4), (16192, 2, 4),
    (16213, 2, 5), (16214, 2, 5), (16222, 2, 4), (16223, 2, 4), (16224, 2, 4), (16225, 2, 5),
    (16226, 2, 5), (16227, 2, 5), (16228, 2, 6), (16229, 2, 1), (16396, 2, 3), (16397, 2, 3),
    (16398, 2, 5), (16399, 2, 5), (16400, 2, 3), (16401, 2, 3), (16402, 2, 3), (16403, 2, 3),
    (16404, 2, 4), (22005, 2, 4), (22014, 2, 4), (22028, 2, 4), (22029, 2, 4), (22030, 2, 4),
    (32001, 2, 4), (32032, 2, 4), (32033, 2, 4),
];
/// 菜单浏览用目录(主村条目在前,黄金岛条目在后;同一 ID 两边都可用时各一条)。
const HIDDEN_CATALOG_TABLE: &[HiddenItem] = &[
    HiddenItem { id: 5010, name: "金砖尖顶房", category: "其它", island: false },
    HiddenItem { id: 5011, name: "尖尖小红屋", category: "其它", island: false },
    HiddenItem { id: 5012, name: "海滨茅草屋", category: "其它", island: false },
    HiddenItem { id: 14032, name: "一堆木箱子", category: "其它", island: false },
    HiddenItem { id: 14061, name: "向日葵", category: "其它", island: false },
    HiddenItem { id: 14062, name: "向日葵", category: "其它", island: false },
    HiddenItem { id: 14110, name: "足球摩尔雕像", category: "其它", island: false },
    HiddenItem { id: 14153, name: "摩尔宝宝屋", category: "礼包专属", island: false },
    HiddenItem { id: 14154, name: "彩色厕所", category: "礼包专属", island: false },
    HiddenItem { id: 14155, name: "圣诞摩尔雕塑", category: "节日·圣诞", island: false },
    HiddenItem { id: 14158, name: "大大稻草人", category: "其它", island: false },
    HiddenItem { id: 14160, name: "特色路灯", category: "其它", island: false },
    HiddenItem { id: 14161, name: "特色晾衣架", category: "其它", island: false },
    HiddenItem { id: 14169, name: "小象灌木", category: "其它", island: false },
    HiddenItem { id: 14170, name: "圣诞花", category: "节日·圣诞", island: false },
    HiddenItem { id: 14175, name: "粉色郁金香", category: "其它", island: false },
    HiddenItem { id: 14179, name: "驯鹿木马", category: "节日·圣诞", island: false },
    HiddenItem { id: 14180, name: "铃兰", category: "其它", island: false },
    HiddenItem { id: 14184, name: "七色花", category: "其它", island: false },
    HiddenItem { id: 14188, name: "迎春花", category: "其它", island: false },
    HiddenItem { id: 14194, name: "桃树", category: "其它", island: false },
    HiddenItem { id: 14195, name: "梨树", category: "其它", island: false },
    HiddenItem { id: 14208, name: "蓝躺椅", category: "其它", island: false },
    HiddenItem { id: 14209, name: "保温杯", category: "其它", island: false },
    HiddenItem { id: 14230, name: "长条沙地", category: "其它", island: false },
    HiddenItem { id: 14231, name: "小块沙地", category: "其它", island: false },
    HiddenItem { id: 14232, name: "大块沙地", category: "其它", island: false },
    HiddenItem { id: 14291, name: "竹盆景", category: "其它", island: false },
    HiddenItem { id: 14502, name: "蒲公英", category: "其它", island: false },
    HiddenItem { id: 14505, name: "吹泡泡", category: "其它", island: false },
    HiddenItem { id: 14525, name: "新年龙灯", category: "节日·新年", island: false },
    HiddenItem { id: 14801, name: "龙雕像", category: "其它", island: false },
    HiddenItem { id: 14802, name: "小猫灌木", category: "其它", island: false },
    HiddenItem { id: 14805, name: "草球", category: "其它", island: false },
    HiddenItem { id: 14806, name: "缤纷花束", category: "其它", island: false },
    HiddenItem { id: 14809, name: "五彩纸风车", category: "其它", island: false },
    HiddenItem { id: 14810, name: "一剪梅屏风", category: "其它", island: false },
    HiddenItem { id: 14901, name: "爱心天使", category: "充值解锁", island: false },
    HiddenItem { id: 14907, name: "鲜花拱门左", category: "其它", island: false },
    HiddenItem { id: 14908, name: "鲜花拱门右", category: "其它", island: false },
    HiddenItem { id: 14911, name: "摩尔火车厢2", category: "节日·儿童节", island: false },
    HiddenItem { id: 14926, name: "天鹅堡主城沙雕", category: "占卜屋套件", island: false },
    HiddenItem { id: 14927, name: "天鹅堡尖塔沙雕", category: "占卜屋套件", island: false },
    HiddenItem { id: 14928, name: "天鹅堡副楼沙雕", category: "占卜屋套件", island: false },
    HiddenItem { id: 14929, name: "天鹅堡高塔沙雕", category: "占卜屋套件", island: false },
    HiddenItem { id: 14933, name: "炫彩星星灯", category: "礼包专属", island: false },
    HiddenItem { id: 14937, name: "和风路灯", category: "岛上有售", island: false },
    HiddenItem { id: 14939, name: "泳圈围栏", category: "岛上有售", island: false },
    HiddenItem { id: 14946, name: "堆堆泳圈", category: "岛上有售", island: false },
    HiddenItem { id: 14953, name: "清凉小“足球”", category: "节日·世界杯", island: false },
    HiddenItem { id: 14956, name: "乐乐水塔", category: "充值解锁", island: false },
    HiddenItem { id: 14968, name: "洁白拱门左", category: "其它", island: false },
    HiddenItem { id: 14969, name: "洁白拱门右", category: "其它", island: false },
    HiddenItem { id: 14978, name: "冰淇淋屋", category: "礼包专属", island: false },
    HiddenItem { id: 14982, name: "西瓜尖围栏", category: "岛上有售", island: false },
    HiddenItem { id: 14983, name: "西瓜圆围栏", category: "岛上有售", island: false },
    HiddenItem { id: 16046, name: "古堡主塔", category: "占卜屋套件", island: false },
    HiddenItem { id: 16103, name: "东欧城堡左塔楼", category: "占卜屋套件", island: false },
    HiddenItem { id: 16131, name: "庆典水晶天鹅", category: "节日·周年庆", island: false },
    HiddenItem { id: 16132, name: "童话世界之树", category: "节日·周年庆", island: false },
    HiddenItem { id: 16133, name: "巫师摩尔", category: "节日·万圣", island: false },
    HiddenItem { id: 16137, name: "南瓜路灯", category: "节日·万圣", island: false },
    HiddenItem { id: 16138, name: "枯树", category: "节日·万圣", island: false },
    HiddenItem { id: 16139, name: "蝙蝠地毯", category: "节日·万圣", island: false },
    HiddenItem { id: 16141, name: "木乃伊指示牌", category: "节日·万圣", island: false },
    HiddenItem { id: 16142, name: "恶魔拱门左", category: "节日·万圣", island: false },
    HiddenItem { id: 16143, name: "恶魔拱门右", category: "节日·万圣", island: false },
    HiddenItem { id: 16144, name: "恶魔铁栅栏", category: "节日·万圣", island: false },
    HiddenItem { id: 16148, name: "麦田火鸡", category: "节日·丰收", island: false },
    HiddenItem { id: 16149, name: "稻草人(新)", category: "节日·丰收", island: false },
    HiddenItem { id: 16150, name: "薰衣草围栏", category: "节日·丰收", island: false },
    HiddenItem { id: 16151, name: "薰衣草", category: "节日·丰收", island: false },
    HiddenItem { id: 16152, name: "生梨路灯", category: "节日·丰收", island: false },
    HiddenItem { id: 16153, name: "葡萄路灯", category: "节日·丰收", island: false },
    HiddenItem { id: 16154, name: "柿子路灯", category: "节日·丰收", island: false },
    HiddenItem { id: 16155, name: "草莓路灯", category: "节日·丰收", island: false },
    HiddenItem { id: 16179, name: "冰雕圣诞树", category: "节日·圣诞", island: false },
    HiddenItem { id: 16180, name: "圣诞树", category: "节日·圣诞", island: false },
    HiddenItem { id: 16181, name: "奶油屋", category: "节日·圣诞", island: false },
    HiddenItem { id: 16182, name: "粉色雪花地毯", category: "节日·冬季", island: false },
    HiddenItem { id: 16183, name: "紫色雪花地毯", category: "节日·冬季", island: false },
    HiddenItem { id: 16184, name: "蓝色雪花地毯", category: "节日·冬季", island: false },
    HiddenItem { id: 16186, name: "圣诞福袋", category: "节日·圣诞", island: false },
    HiddenItem { id: 16187, name: "么么公主雪人", category: "节日·冬季", island: false },
    HiddenItem { id: 16188, name: "摩乐乐雪人", category: "节日·冬季", island: false },
    HiddenItem { id: 16189, name: "菩提大伯雪人", category: "节日·冬季", island: false },
    HiddenItem { id: 16190, name: "丫丽雪人", category: "节日·冬季", island: false },
    HiddenItem { id: 16191, name: "拐杖糖", category: "节日·圣诞", island: false },
    HiddenItem { id: 16192, name: "圣诞路灯", category: "节日·圣诞", island: false },
    HiddenItem { id: 16193, name: "圣诞派", category: "节日·圣诞", island: false },
    HiddenItem { id: 16194, name: "圣诞糖果", category: "节日·圣诞", island: false },
    HiddenItem { id: 16195, name: "圣诞布丁", category: "节日·圣诞", island: false },
    HiddenItem { id: 16196, name: "圣诞金杯", category: "活动奖杯", island: false },
    HiddenItem { id: 16197, name: "圣诞银杯", category: "活动奖杯", island: false },
    HiddenItem { id: 16198, name: "圣诞铜杯", category: "活动奖杯", island: false },
    HiddenItem { id: 16212, name: "茉莉公主皇宫副宫", category: "岛上有售", island: false },
    HiddenItem { id: 16214, name: "皇宫城墙拐角", category: "其它", island: false },
    HiddenItem { id: 16222, name: "马年花灯", category: "节日·新年", island: false },
    HiddenItem { id: 16223, name: "年兽花灯", category: "节日·新年", island: false },
    HiddenItem { id: 16224, name: "新年许愿树", category: "节日·新年", island: false },
    HiddenItem { id: 16225, name: "哈哈笑拨浪鼓", category: "节日·新年", island: false },
    HiddenItem { id: 16226, name: "呆呆萌拨浪鼓", category: "节日·新年", island: false },
    HiddenItem { id: 16227, name: "眯眯眼拨浪鼓", category: "节日·新年", island: false },
    HiddenItem { id: 16228, name: "大红灯笼", category: "节日·新年", island: false },
    HiddenItem { id: 16229, name: "福字地毯", category: "节日·新年", island: false },
    HiddenItem { id: 16313, name: "心愿板", category: "岛上有售", island: false },
    HiddenItem { id: 16314, name: "红红小玩偶", category: "岛上有售", island: false },
    HiddenItem { id: 16393, name: "嘉年华金杯", category: "活动奖杯", island: false },
    HiddenItem { id: 16394, name: "嘉年华银杯", category: "活动奖杯", island: false },
    HiddenItem { id: 16395, name: "嘉年华铜杯", category: "活动奖杯", island: false },
    HiddenItem { id: 16396, name: "悉尼歌剧院（主厅）", category: "节日·世界杯", island: false },
    HiddenItem { id: 16397, name: "悉尼歌剧院（副厅）", category: "节日·世界杯", island: false },
    HiddenItem { id: 16398, name: "法国凯旋门（左）", category: "节日·世界杯", island: false },
    HiddenItem { id: 16399, name: "法国凯旋门（右）", category: "节日·世界杯", island: false },
    HiddenItem { id: 16400, name: "西班牙斗牛场1", category: "节日·世界杯", island: false },
    HiddenItem { id: 16401, name: "西班牙斗牛场2", category: "节日·世界杯", island: false },
    HiddenItem { id: 16402, name: "西班牙斗牛场3", category: "节日·世界杯", island: false },
    HiddenItem { id: 16403, name: "西班牙斗牛场4", category: "节日·世界杯", island: false },
    HiddenItem { id: 16404, name: "巴西狂欢花车", category: "节日·世界杯", island: false },
    HiddenItem { id: 17003, name: "布丁", category: "季节食物", island: false },
    HiddenItem { id: 17005, name: "饼干", category: "季节食物", island: false },
    HiddenItem { id: 17011, name: "蛋糕", category: "季节食物", island: false },
    HiddenItem { id: 17014, name: "三明治", category: "季节食物", island: false },
    HiddenItem { id: 17015, name: "薯条", category: "季节食物", island: false },
    HiddenItem { id: 17018, name: "杨桃", category: "季节食物", island: false },
    HiddenItem { id: 17020, name: "木瓜", category: "季节食物", island: false },
    HiddenItem { id: 17106, name: "周年庆蜡烛", category: "节日·周年庆", island: false },
    HiddenItem { id: 17107, name: "周年庆蜡烛", category: "节日·周年庆", island: false },
    HiddenItem { id: 17108, name: "周年庆蜡烛", category: "节日·周年庆", island: false },
    HiddenItem { id: 18004, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18005, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18006, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18007, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18008, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18009, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18010, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18011, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18012, name: "特色庄园奖杯", category: "活动奖杯", island: false },
    HiddenItem { id: 18101, name: "兔子伞", category: "其它", island: false },
    HiddenItem { id: 18102, name: "兔子伞", category: "其它", island: false },
    HiddenItem { id: 18103, name: "兔子伞", category: "其它", island: false },
    HiddenItem { id: 18104, name: "粉笔椅", category: "其它", island: false },
    HiddenItem { id: 18105, name: "粉笔椅", category: "其它", island: false },
    HiddenItem { id: 18106, name: "粉笔椅", category: "其它", island: false },
    HiddenItem { id: 18107, name: "墨水瓶", category: "其它", island: false },
    HiddenItem { id: 18108, name: "墨水瓶", category: "其它", island: false },
    HiddenItem { id: 18109, name: "墨水瓶", category: "其它", island: false },
    HiddenItem { id: 18110, name: "路灯", category: "其它", island: false },
    HiddenItem { id: 18111, name: "路灯", category: "其它", island: false },
    HiddenItem { id: 18112, name: "猫风铃", category: "其它", island: false },
    HiddenItem { id: 18113, name: "小兔不倒翁", category: "其它", island: false },
    HiddenItem { id: 18114, name: "小猪不倒翁", category: "其它", island: false },
    HiddenItem { id: 18115, name: "小猫不倒翁", category: "其它", island: false },
    HiddenItem { id: 18116, name: "小熊不倒翁", category: "其它", island: false },
    HiddenItem { id: 22014, name: "雪花路灯", category: "节日·冬季", island: false },
    HiddenItem { id: 22028, name: "小灯笼", category: "节日·新年", island: false },
    HiddenItem { id: 22029, name: "大灯笼", category: "节日·新年", island: false },
    HiddenItem { id: 22030, name: "中国结", category: "节日·新年", island: false },
    HiddenItem { id: 14160, name: "特色路灯", category: "其它", island: true },
    HiddenItem { id: 14183, name: "松鼠", category: "礼包专属", island: true },
    HiddenItem { id: 16133, name: "巫师摩尔", category: "节日·万圣", island: true },
    HiddenItem { id: 16137, name: "南瓜路灯", category: "节日·万圣", island: true },
    HiddenItem { id: 16138, name: "枯树", category: "节日·万圣", island: true },
    HiddenItem { id: 16139, name: "蝙蝠地毯", category: "节日·万圣", island: true },
    HiddenItem { id: 16141, name: "木乃伊指示牌", category: "节日·万圣", island: true },
    HiddenItem { id: 16142, name: "恶魔拱门左", category: "节日·万圣", island: true },
    HiddenItem { id: 16143, name: "恶魔拱门右", category: "节日·万圣", island: true },
    HiddenItem { id: 16144, name: "恶魔铁栅栏", category: "节日·万圣", island: true },
    HiddenItem { id: 16148, name: "麦田火鸡", category: "节日·丰收", island: true },
    HiddenItem { id: 16149, name: "稻草人(新)", category: "节日·丰收", island: true },
    HiddenItem { id: 16150, name: "薰衣草围栏", category: "节日·丰收", island: true },
    HiddenItem { id: 16151, name: "薰衣草", category: "节日·丰收", island: true },
    HiddenItem { id: 16152, name: "生梨路灯", category: "节日·丰收", island: true },
    HiddenItem { id: 16153, name: "葡萄路灯", category: "节日·丰收", island: true },
    HiddenItem { id: 16154, name: "柿子路灯", category: "节日·丰收", island: true },
    HiddenItem { id: 16155, name: "草莓路灯", category: "节日·丰收", island: true },
    HiddenItem { id: 16179, name: "冰雕圣诞树", category: "节日·圣诞", island: true },
    HiddenItem { id: 16180, name: "圣诞树", category: "节日·圣诞", island: true },
    HiddenItem { id: 16182, name: "粉色雪花地毯", category: "节日·冬季", island: true },
    HiddenItem { id: 16183, name: "紫色雪花地毯", category: "节日·冬季", island: true },
    HiddenItem { id: 16184, name: "蓝色雪花地毯", category: "节日·冬季", island: true },
    HiddenItem { id: 16186, name: "圣诞福袋", category: "节日·圣诞", island: true },
    HiddenItem { id: 16187, name: "么么公主雪人", category: "节日·冬季", island: true },
    HiddenItem { id: 16188, name: "摩乐乐雪人", category: "节日·冬季", island: true },
    HiddenItem { id: 16189, name: "菩提大伯雪人", category: "节日·冬季", island: true },
    HiddenItem { id: 16190, name: "丫丽雪人", category: "节日·冬季", island: true },
    HiddenItem { id: 16191, name: "拐杖糖", category: "节日·圣诞", island: true },
    HiddenItem { id: 16192, name: "圣诞路灯", category: "节日·圣诞", island: true },
    HiddenItem { id: 16213, name: "皇宫城墙拐角", category: "其它", island: true },
    HiddenItem { id: 16214, name: "皇宫城墙拐角", category: "其它", island: true },
    HiddenItem { id: 16222, name: "马年花灯", category: "节日·新年", island: true },
    HiddenItem { id: 16223, name: "年兽花灯", category: "节日·新年", island: true },
    HiddenItem { id: 16224, name: "新年许愿树", category: "节日·新年", island: true },
    HiddenItem { id: 16225, name: "哈哈笑拨浪鼓", category: "节日·新年", island: true },
    HiddenItem { id: 16226, name: "呆呆萌拨浪鼓", category: "节日·新年", island: true },
    HiddenItem { id: 16227, name: "眯眯眼拨浪鼓", category: "节日·新年", island: true },
    HiddenItem { id: 16228, name: "大红灯笼", category: "节日·新年", island: true },
    HiddenItem { id: 16229, name: "福字地毯", category: "节日·新年", island: true },
    HiddenItem { id: 16396, name: "悉尼歌剧院（主厅）", category: "节日·世界杯", island: true },
    HiddenItem { id: 16397, name: "悉尼歌剧院（副厅）", category: "节日·世界杯", island: true },
    HiddenItem { id: 16398, name: "法国凯旋门（左）", category: "节日·世界杯", island: true },
    HiddenItem { id: 16399, name: "法国凯旋门（右）", category: "节日·世界杯", island: true },
    HiddenItem { id: 16400, name: "西班牙斗牛场1", category: "节日·世界杯", island: true },
    HiddenItem { id: 16401, name: "西班牙斗牛场2", category: "节日·世界杯", island: true },
    HiddenItem { id: 16402, name: "西班牙斗牛场3", category: "节日·世界杯", island: true },
    HiddenItem { id: 16403, name: "西班牙斗牛场4", category: "节日·世界杯", island: true },
    HiddenItem { id: 16404, name: "巴西狂欢花车", category: "节日·世界杯", island: true },
    HiddenItem { id: 22005, name: "心形灌木", category: "探险船奖励", island: true },
    HiddenItem { id: 22014, name: "雪花路灯", category: "节日·冬季", island: true },
    HiddenItem { id: 22028, name: "小灯笼", category: "节日·新年", island: true },
    HiddenItem { id: 22029, name: "大灯笼", category: "节日·新年", island: true },
    HiddenItem { id: 22030, name: "中国结", category: "节日·新年", island: true },
    HiddenItem { id: 32001, name: "南瓜车", category: "探险船奖励", island: true },
    HiddenItem { id: 32032, name: "幸运草盆景", category: "探险船奖励", island: true },
    HiddenItem { id: 32033, name: "幸运草伞", category: "探险船奖励", island: true },
];
/// 节日·万圣:原始候选 15 条,有美术且可上架 8 条。
const FEST_HALLOWEEN: &[u32] = &[16133, 16137, 16138, 16139, 16141, 16142, 16143, 16144];
/// 节日·丰收:原始候选 14 条,有美术且可上架 8 条。
const FEST_HARVEST: &[u32] = &[16148, 16149, 16150, 16151, 16152, 16153, 16154, 16155];
/// 节日·圣诞:原始候选 13 条,有美术且可上架 12 条。
const FEST_CHRISTMAS: &[u32] = &[14155, 14170, 14179, 16179, 16180, 16181, 16186, 16191, 16192, 16193, 16194, 16195];
/// 节日·冬季:原始候选 9 条,有美术且可上架 8 条。
const FEST_WINTER: &[u32] = &[16182, 16183, 16184, 16187, 16188, 16189, 16190, 22014];
/// 节日·新年:原始候选 21 条,有美术且可上架 12 条。
const FEST_NEWYEAR: &[u32] = &[14525, 16222, 16223, 16224, 16225, 16226, 16227, 16228, 16229, 22028, 22029, 22030];
/// 节日·世界杯:原始候选 21 条,有美术且可上架 10 条。
const FEST_WORLDCUP: &[u32] = &[14953, 16396, 16397, 16398, 16399, 16400, 16401, 16402, 16403, 16404];
/// 节日·儿童节:原始候选 3 条,有美术且可上架 1 条。
const FEST_CHILDRENS: &[u32] = &[14911];
/// 节日·七夕:原始候选 3 条,有美术且可上架 0 条。
const FEST_QIXI: &[u32] = &[];
/// 节日·周年庆:原始候选 6 条,有美术且可上架 5 条。
const FEST_ANNIVERSARY: &[u32] = &[16131, 16132, 17106, 17107, 17108];
/// 节日·复活节:原始候选 1 条,有美术且可上架 0 条。
const FEST_EASTER: &[u32] = &[];
// ===== 生成表结束 =====

// [补完 2026-09-15] VIP 随贝壳购买累计升级的纯函数单测(档位表 / 门槛解析 / 等级推导)。
// 放在文件最末,避免测试模块之后还有条目(clippy items_after_test_module)。
#[cfg(test)]
mod vip_accumulate_tests {
    use super::*;

    #[test]
    fn shell_pack_uses_item_id_not_index() {
        assert_eq!(shell_pack(0), None);
        assert_eq!(shell_pack(1).map(|p| p.shells), Some(20));
        assert_eq!(shell_pack(7).map(|p| (p.shells, p.cny_yuan)), Some((3500, 648)));
        assert_eq!(shell_pack(8), None);
    }

    #[test]
    fn parse_thresholds_accepts_and_rejects() {
        assert_eq!(parse_vip_thresholds("6,100,500,2000"), Ok(vec![6, 100, 500, 2000]));
        assert_eq!(parse_vip_thresholds(" 10 ，20, "), Ok(vec![10, 20]));
        assert!(parse_vip_thresholds("").is_err());
        assert!(parse_vip_thresholds("5,5").is_err());
        assert!(parse_vip_thresholds("0,10").is_err());
        assert!(parse_vip_thresholds("abc").is_err());
        assert!(parse_vip_thresholds("300000000").is_err());
    }

    #[test]
    fn progress_is_monotone_and_caps_at_vip4() {
        let th = [60, 1000, 5000, 20000];
        assert_eq!(vip_progress(0, 0, &th), (0, 60));
        assert_eq!(vip_progress(0, 60, &th), (1, 1000));
        assert_eq!(vip_progress(0, 6480, &th), (3, 20000));
        assert_eq!(vip_progress(0, 25920, &th), (4, 0));
        // 只升不降:旧等级高于推导值时保持旧等级
        assert_eq!(vip_progress(3, 60, &th), (3, 20000));
        // 门槛表用完(环境变量只给 2 级)时下一级门槛为 0
        assert_eq!(vip_progress(0, 100, &[60, 90]), (2, 0));
    }
}
