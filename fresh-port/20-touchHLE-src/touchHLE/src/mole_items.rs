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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
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
/// [2026-09-16] 改 pub(crate):mole_activity 喂春节烟花回包前要判 tag 1 子节点是不是 FireworkLayer,复用这一份。
pub(crate) fn is_kind_of(env: &Environment, obj: id, want: &str) -> bool {
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

/// 当前"本地钟面秒"(unix 秒 + 本地时区偏移)。时区规则与引擎一致(默认 Asia/Shanghai,MOLE_TZ=host 跟随宿主),
/// 并含 mole_dev 时间旅行偏移,保证节日窗口/登录日界与游戏内看到的时间一致。
/// [2026-09-25 第五轮遗留 MISC-4] 取时由墙钟(host_now_unix_secs + time_offset_secs)改为 crate::mole_cheats::now_cf_secs():
/// 与离线 -[NewSceneTimer getCurrentServerTime] 臂、mole_activity 的 local_date/now_cf_u32 同一单调时钟(正常运行时等于墙钟,
/// 进程内宿主时间回拨时不倒退)。根因:F2-07 统一节日日历后,mole_activity 的 festival_today(废品站高价回收、春节烟花)与这里的
/// festival_active_mask(节日商店)查的是同一张表,两边必须同一天;只改那边会在回拨跨节日边界时错开一天。
/// now_cf_secs 已含时间旅行偏移,不能再加 time_offset_secs。进村连续登录 on_enter_village 在进程内回拨时不再走「回拨保持原记录」
/// 那一支(日界不倒退);跨重启单调时钟从墙钟重新起算,那一支照旧兜底。
fn local_wall_secs() -> i64 {
    let cf = crate::mole_cheats::now_cf_secs();
    let unix = if cf.is_finite() {
        cf.floor() as i64 + 978_307_200
    } else {
        crate::libc::time::host_now_unix_secs() + crate::libc::time::time_offset_secs()
    };
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

/// [2026-09-16] F2-07 统一节日日历:按日序号(1970-01-01 = 0,本地日期)判断节日键 `key` 当天是否在窗口内。
/// 只看日期,不看 MOLE_FESTIVAL,也不看菜单「节日商店」模式;认不出的键返回 false。
/// 以前 mole_activity 另有一套窗口(圣诞只算 12/20~12/31、春节只算除夕~元宵),与商店节日物上架对不上;
/// 现在废品站高价回收(christmas)与春节烟花(newyear)直接复用这张表。两套窗口原本都是移植者自拟(原版由服务器按活动下发),
/// 统一后高价回收从 12/10 起、烟花从初一前 15 天起生效。
pub(crate) fn festival_on_date(key: &str, day_index: i64) -> bool {
    FESTIVALS
        .iter()
        .position(|f| f.key == key)
        .is_some_and(|i| festival_on_day(i, day_index))
}

/// [2026-09-16] F2-07 MOLE_FESTIVAL 的解析结果(商店模式与活动模块共用同一份解析)。
enum FestEnv {
    ByDate,
    All,
    Off,
    /// 只开 FESTIVALS[下标] 这一个节日。
    Only(usize),
}

/// [2026-09-16] F2-07 解析 MOLE_FESTIVAL 原值,认不出返回 None(调用方按日期处理)。
/// 接受 date/auto、all/on/1、off/none/0、节日英文键或中文名,以及活动模块旧取值的别名 spring→newyear、xmas→christmas。
fn parse_festival_env(raw: &str) -> Option<FestEnv> {
    let v = raw.trim().to_ascii_lowercase();
    match v.as_str() {
        "" | "date" | "auto" => Some(FestEnv::ByDate),
        "all" | "on" | "1" => Some(FestEnv::All),
        "off" | "none" | "0" => Some(FestEnv::Off),
        other => {
            let key = match other {
                "spring" => "newyear",
                "xmas" => "christmas",
                k => k,
            };
            FESTIVALS
                .iter()
                .position(|f| f.key == key || f.cn == key)
                .map(FestEnv::Only)
        }
    }
}

/// [2026-09-16] F2-07 只解析 MOLE_FESTIVAL,不看菜单「节日商店」轮换模式(那个开关只管商店)。
/// 返回强制的节日键(如 "christmas"/"newyear"),或强制态 "all"/"off"/"date";未设置或认不出返回 None(= 按日期)。
/// 认不出时的「无法识别」日志由 fest_mode 打一次,这里不重复打。
pub(crate) fn festival_forced() -> Option<&'static str> {
    let raw = std::env::var("MOLE_FESTIVAL").ok()?;
    match parse_festival_env(&raw)? {
        FestEnv::ByDate => Some("date"),
        FestEnv::All => Some("all"),
        FestEnv::Off => Some("off"),
        FestEnv::Only(i) => Some(FESTIVALS[i].key),
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
    // [2026-09-16] F2-07 解析改走 parse_festival_env(与 festival_forced 同一份),新增别名 spring/xmas。
    let init = match std::env::var("MOLE_FESTIVAL") {
        Err(_) => FEST_BY_DATE,
        Ok(raw) => match parse_festival_env(&raw) {
            Some(FestEnv::ByDate) => FEST_BY_DATE,
            Some(FestEnv::All) => FEST_ALL,
            Some(FestEnv::Off) => FEST_OFF,
            Some(FestEnv::Only(i)) => FEST_FORCE_BASE + i as u8,
            None => {
                log!(
                    "[MOLEITEMS] MOLE_FESTIVAL={} 无法识别(可用 all/off/date、节日名,或别名 spring/xmas),按日期处理",
                    raw
                );
                FEST_BY_DATE
            }
        },
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
    /// [2026-10-04 第八轮 R8-A1] 首充大礼包待弹(随 vip.dat 落盘,见 FIRST_CHARGE_PENDING)。
    first_charge_pending: bool,
    /// [2026-10-04 第八轮 R8-A2] 充值解锁物待在主村重放落盘(见 RECHARGE_UNLOCK_PENDING)。
    recharge_unlock_pending: bool,
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
            first_charge_pending: false,
            recharge_unlock_pending: false,
        }
    }
}

static SIDE: Mutex<Side> = Mutex::new(Side::new());
static SIDE_BLOCK_LOGGED: AtomicBool = AtomicBool::new(false);

/// 注意:持锁期间绝不能发 msg_send(嵌套的 objc_msgSend 可能再次进入本模块钩子而死锁)。
fn side() -> MutexGuard<'static, Side> {
    SIDE.lock().unwrap_or_else(|e| e.into_inner())
}

/// [2026-09-16] F2-06 改 pub(crate):mole_activity.dat 升 v=2 后用同一个校验和算法。
pub(crate) fn fnv1a(bytes: &[u8]) -> u32 {
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
    // [2026-10-04 第八轮 R8-A1/A2] 两个充值待办,为 1 才写;旧版本读到未知键会忽略,新版本读旧档按 0。
    if s.first_charge_pending {
        body.push_str("first_charge_pending=1\n");
    }
    if s.recharge_unlock_pending {
        body.push_str("recharge_unlock_pending=1\n");
    }
    let sum = fnv1a(body.as_bytes());
    body.push_str(&format!("sum={:08x}\n", sum));
    body.into_bytes()
}

struct SideParsed {
    vip: Option<VipVals>,
    last_day: i64,
    streak: u32,
    online_ms: u64,
    first_charge_pending: bool,
    recharge_unlock_pending: bool,
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
        first_charge_pending: false,
        recharge_unlock_pending: false,
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
            "first_charge_pending" => p.first_charge_pending = v.trim() == "1",
            "recharge_unlock_pending" => p.recharge_unlock_pending = v.trim() == "1",
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
        // [2026-10-04 第八轮 R8-A1/A2] 上次没办完的充值待办(例如岛上充值后没回主村就退出)读回后照常受理。
        s.first_charge_pending = p.first_charge_pending;
        s.recharge_unlock_pending = p.recharge_unlock_pending;
        if p.first_charge_pending {
            FIRST_CHARGE_PENDING.store(true, O);
        }
        if p.recharge_unlock_pending {
            RECHARGE_UNLOCK_PENDING.store(true, O);
        }
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
    FIRST_CHARGE_PENDING.store(false, O);
    RECHARGE_UNLOCK_PENDING.store(false, O);
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
    // [2026-09-16] F2-05 时间旅行隔离:开发工具「时间旅行」的偏移只在内存里(重启回到现实),期间把「未来」的本地日写进 last_day,
    //   会让连续登录冻结到现实日期追上为止;往前拨超过 1 天还会把 streak 直接打回 1。
    //   所以偏移非 0 时,本次进村不改登录日/连续天数,这里也不落盘;在线计时照常开始、累计(切后台与 5 分钟节流时照旧落盘)。
    //   代价:旅行期间测出来的「连续登录」重启即丢,这是有意的。
    if crate::libc::time::time_offset_secs() != 0 {
        let secs = {
            let mut s = side();
            s.village_entered = true;
            if s.anchor.is_none() {
                s.anchor = Some(Instant::now());
            }
            online_flush(&mut s);
            online_total_secs(&s)
        };
        log!(
            "[MOLEITEMS] 时间旅行中(偏移 {} 秒):本次进村不记登录日/连续天数,不在这里写 {}(累计在线 {} 秒照常计)",
            crate::libc::time::time_offset_secs(),
            SIDE_FILE,
            secs
        );
        return;
    }
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

/// [2026-10-06 第九轮 R9-D1] 离线回环 1051/1050 回包用:(连续登录天数, 累计在线秒),与 stats_getter 的返回值同源。
/// 进村补发在 startGame: 臂(on_enter_village 已先记好当天)之后的受理点执行,这里只读侧档,不改、不落盘。
pub(crate) fn offline_login_stats(env: &mut Environment) -> (u32, u32) {
    side_ensure_loaded(env);
    let s = side();
    (s.streak, online_total_secs(&s).min(u32::MAX as u64) as u32)
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
/// [2026-10-03] 用户拍板「替服务器补发 VIP 信息」:没充过值(侧档没有 VIP 记录)的号以前三值恒 0,
/// -[VIPLayer showWithTarget:selector:] 在 0x37f0a6 `orrs r0, next, value` / 0x37f0aa beq 见两者都是 0 就走 0x37f26c:reset 后弹
/// GET_VIP_INFO_FAILED「获取 VIP 信息失败!请先检查网络状况」。原版进村每次都发 getVipInfo(startGame: 0x19ca0),服务器给没充值的玩家
/// 回 VIP0、累计 0、下一级门槛 = VIP1 所需累计额,面板显示「达到 VIP 1 您还需充值 N 元」与首充大礼包提示(0x37f39e)。
/// 这里照同一口径注入 VipVals{0, 0, VIP1 门槛}(vip_default_vals,门槛与假充值累计同一张 vip_thresholds 表,移植者自拟、非原版数据);
/// 注入时 VIP_BUSY 置位,setter 钩子不记账,不写侧档——侧档只记真实充值/修改器改过的值。不走 parseVipInfo,不涉及它的作弊警告(0x1c0c96)。
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
            let recorded = side().vip;
            let vals = Some(recorded.unwrap_or_else(vip_default_vals));
            VIP_BUSY.store(true, O);
            let local = local_vip_object(env);
            let is_local = local != nil && local.to_bits() == recv;
            if is_local {
                VIP_LOCAL.store(recv, O);
                if let Some(v) = vals {
                    vip_apply(env, local, v);
                    VIP_INJECTED.store(true, O);
                    if recorded.is_some() {
                        log!(
                            "[MOLEITEMS] VIP 读档:从 {} 写回 vipLevel={} vipValue={} vipValueOfNextLevel={}",
                            SIDE_FILE,
                            v.level,
                            v.value,
                            v.next
                        );
                    } else {
                        log!(
                            "[MOLEITEMS] VIP 读档:没有充值记录,照原版服务器口径下发 vipLevel=0 vipValue=0 vipValueOfNextLevel={}(VIP1 门槛,移植者自拟、非原版数据;不写 {})",
                            v.next,
                            SIDE_FILE
                        );
                    }
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

/// [2026-10-03] 没充过值的玩家「服务器」该下发的三值:VIP0、累计 0、下一级门槛 = VIP1 门槛(见 vip_hook 注释)。
fn vip_default_vals() -> VipVals {
    let (level, next) = vip_progress(0, 0, vip_thresholds());
    VipVals {
        level,
        value: 0,
        next,
    }
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
/// [2026-09-25 第五轮遗留 V] 1084 回包还有「分发」那一半(主村 0x239f6 / 岛 0x23e7c4:HUD VIP 徽章、VIP 成就、贝壳树重排),
///    由 on_shells_purchased 末尾照原版入口补发 getVipInfo(mole_activity::request_vip_info)、运行循环受理点执行。
/// [2026-10-03] 首充大礼包(用户拍板补上;以前有意省略)。
/// 原版进村 -[GameManager startGame:]+0xb38(0x19ca0)每次都发 getVipInfo,没充过值的玩家也会先收到一次 VIP 信息,
/// 本地旧 next 不为 0;第一次真充值后 parseVipInfo 在 0x1c0e34-0x1c0e48 判定「旧等级 [sp+4]=0、旧 VIP 值 [sp+8]=0、
/// 旧 next [sp+0x18]≠0、新 VIP 值 [sp+0x20]≠0」成立,调 [[WrapperManager sharedManager] initFirstChargeGifts],
/// 再 [FirstChargeGiftsLayer layerWithRewards:[wm firstChargeGiftsArray]] showWithTarget:NetworkManager selector:nil(0x1c0e64..0x1c0ec0)。
/// 所以原版每个玩家第一次充值都会弹首充礼包(-[WrapperManager initFirstChargeGifts]@0x262d58:702×2000、704×10、22022、22023)。
/// 现在没充值的号由 vip_hook 按服务器口径注入了 VIP1 门槛,「旧 next ≠ 0」与原版一样成立;这里只要旧等级、旧累计都是 0 而新累计 > 0,
/// 就置 FIRST_CHARGE_PENDING,不在购买按钮的调用栈上弹(原版是异步回包触发)。由运行循环受理点 first_charge_gift_poll 在主村
/// (curSceneId 1)、currentGameMode == 1 时照上面的顺序调用:-[FirstChargeGiftsLayer showWithTarget:selector:]@0x3803cc 自己在
/// 0x38040a 要求 currentGameMode == 1,否则直接返回不弹,所以等商店等面板关掉再弹与原版判据一致。
/// [2026-10-04 第八轮 R8-A1] 待弹标志改为随 vip.dat 落盘;用户拍板岛上也照原版当场弹(领取走 -[WrapperManager
/// onAddFirstChargeGiftToMap:] 0x262f9c 的岛分支,摆到岛上),进岛加载/离岛过场期间不弹。
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
    // [2026-10-03] 首充大礼包:与 parseVipInfo 0x1c0e34..0x1c0e48 同判据(旧 next ≠ 0 由 vip_default_vals 的服务器口径保证)。
    if old.level == 0 && old.value <= 0 && new.value > 0 {
        // [2026-10-04 第八轮 R8-A1] 先写进 Side,下面 vip_apply 触发的 setter 臂 side_save 会把「新累计 + 待弹」一次原子写盘,
        //   不再出现「累计已写、待弹只在内存」的窗口(以前在岛上首充后没回主村就退出,礼包永久丢失)。
        side().first_charge_pending = true;
        FIRST_CHARGE_PENDING.store(true, O);
        log!(
            "[MOLEITEMS] 首充:旧 VIP0 / 累计 0 → 累计 {}(0.1 元),照原版 parseVipInfo 判据应弹首充大礼包(待弹已随 vip.dat 落盘);在主村或已稳定在岛上、没有面板打开时弹出",
            new.value
        );
    }

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
/// [2026-09-25 第五轮遗留 V] 最后照原版 0x117dcc 补发一次 getVipInfo(只排 1084 回包分发,见 mole_activity::request_vip_info)。
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
    // [2026-10-04 第八轮 R8-A2] 充值解锁物的「回主村重放落盘」待办先进 Side;下面 vip_accumulate 里 setter 臂的 side_save
    //   会把它和新累计一次原子写盘,末尾再显式写一次兜住没经 setter 记账的情形(非充值档位、本地 VIP 对象还没建)。
    side_ensure_loaded(env);
    side().recharge_unlock_pending = true;
    RECHARGE_UNLOCK_PENDING.store(true, O);
    recharge_unlock_side_effects(env);
    vip_accumulate(env, item_id, shells, pack);
    side_save(env);
    restore_regs(env, saved);
    // [2026-09-25 第五轮遗留 V] 原版 -[InAppPurchaseManager onPurchaseSuccessful] 在 0x117da8 发 1083
    //   (sendCostMoneyInfoToServerWithUserId:andNumber:)之后,紧接 0x117dcc `[nm getVipInfo]`
    //   (-[GameData addAlreadyPurchaseVipgoldWithPurchaseInfo:] 0x7f284 也发一次)。1084 回包写三值那一半已由上面 vip_accumulate
    //   替代,这里补分发那一半(HUD VIP 徽章、VIP 成就、贝壳树重排):纯原子置排队标志,不发消息、不碰寄存器、不在 side() 锁内,
    //   由运行循环受理点 mole_activity::vip_info_poll 在本轮末尾执行。
    crate::mole_activity::request_vip_info(env, "假充值后补发 getVipInfo(原版 0x117dcc / 0x7f284)");
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
    // [2026-10-04 第八轮 R8-A2] 岛上 -[GameData saveToLocal]@0x7cae0 在 0x7cb3e `cmp r0,#0xa` / 0x7cb42 popeq 直接返回,
    //   解锁物只在内存;回村 loadFromLocal 对键 63 盘上有就覆盖(0x2007a),岛上补的会被旧表冲掉。岛上不再空调它,
    //   留着 RECHARGE_UNLOCK_PENDING,由 recharge_unlock_replay 回主村、地图加载完后重放并落盘。
    let on_island = crate::mole_cheats::island_session_active();
    if !on_island {
        let save_s = sel_of(env, "saveToLocal");
        let _: () = msg_send(env, (gd, save_s));
    }
    log!(
        "[MOLEITEMS] 充值副作用:gamedataFlag {:#x} → {:#x},解锁 16283 都教授/14974 克劳神父(乐乐水塔/织女鹊桥锁同时解除){}",
        flag,
        flag | 0x30,
        if on_island {
            ";岛上存不进主档,回主村地图加载完后重放落盘"
        } else {
            ""
        }
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
    // [2026-10-04 第八轮 R8-B1] 主村好友入口放开后,离线也能串门(好友村/丝尔特村时 GameManager.gameMode=0,而 curSceneId 仍是 1)。
    //   原版 -[GameData saveToLocal] 在 0x7cb14/0x7cb18 遇 gameMode 0/6 跳过,不会把 NPC 地图存进玩家档;这里直接调 saveMapData
    //   (它只判 curSceneId==1、对象数、m_isLoadMap),串门时会把别人的地图写进 map.dat。照原版口径只在 currentGameMode==1 时执行。
    let wm = shared(env, "WrapperManager", "sharedManager");
    if wm != nil {
        let mode_s = sel_of(env, "currentGameMode");
        let mode: i32 = msg_send(env, (wm, mode_s));
        if mode != 1 {
            return Err(format!(
                "请先回到自己的庄园、关闭其它面板再入仓库(currentGameMode={})",
                mode
            ));
        }
    }
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

/// [2026-10-08 第十三轮] 建造商店(主村与黄金岛共用 NewStyleStoreMainLayer 单例)是否开着。
/// 单例缓存在静态变量 0xb40ff4(+sharedInstance@0x3ae4e0);关闭走 -[NewStyleStoreMainLayer detach]@0x3afb78
/// removeFromParentAndCleanup:,所以「开着」= 单例存在且有父节点。不存在就不调 +sharedInstance,免得凭空建一个。
/// 开商店不改 gameMode(仍为 1),而原版 -[Porter attachObjectWithHouseLevel:data:] 的 isLogicLayersOpen 检查也不含商店
/// (商店只能从「建造」按钮进,那个按钮先问过 isLogicLayersOpen),原版买东西时先 detach 关商店再摆放。
/// 修改器绕过了这一步:商店开着时发物品/召唤,摆放会把 HUD 连同商店一起隐藏,但商店菜单仍在触摸分发器里按下即吞,
/// 之后摆放的建筑拖不动、✓ 点不到、地图也拖不动,只能重启。调用方须在运行循环上下文(可以发消息)。
pub fn store_open(env: &mut Environment) -> bool {
    let store: u32 = env.mem.read(crate::mem::ConstPtr::<u32>::from_bits(0xb40ff4));
    if store == 0 {
        return false;
    }
    let saved = save_regs(env);
    let parent_s = sel_of(env, "parent");
    let parent: id = msg_send(env, (id::from_bits(store), parent_s));
    restore_regs(env, saved);
    parent != nil
}

/// 把物品 id 直接放到当前场景地图上(主村/黄金岛各走原版放置路径)。
/// 不在可放置状态(gameMode≠1、菜单层为 nil、商店开着)时返回 Err 文案。内部自己快照/恢复 r0-r3。
pub fn place_item(env: &mut Environment, id: u32) -> Result<String, String> {
    if env.options.network_access {
        return Err("在线模式不提供直接放置(以私服数据为准)".to_string());
    }
    if store_open(env) {
        log!("[MOLEITEMS] 拒绝放置 {}:建造商店开着", id);
        return Err("建造商店开着:请先关闭商店再放置".to_string());
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
    // [2026-10-06 第十轮 R10-C2] 原版放下 gameMode 15 的奖励物时(-[EditMenuLayer onButtonOkSelected:]
    // 0x4f586~0x4f6b4)先看 GameData 这十个活动兑换标志,任一为真就当作「活动兑换领奖」去连服务器确认,
    // 离线 isConnected 为假 → 0x509a8 报错并 onCancelExchange 取消放置(还会重发一次兑换确认)。
    // 修改器走的也是 gameMode 15,所以标志没清时先拦下并说明,不去碰原版标志——能清它们的只有
    // resetObjectInfoUnaddedInMap(好友村回家、重新开始)和重启游戏。
    let gd = shared(env, "GameData", "sharedInstance");
    if gd != nil {
        let mut pending: Option<&str> = None;
        for (getter, positive_only) in [
            ("iceCreamExchangeIndex", true),
            ("foodPrintExchangeIndex", true),
            ("selectedTotoroGiftData", false),
            ("getShrekReward", false),
            ("getAnniversaryReward", false),
            ("getAutumnFinalReward", false),
            ("getHalloweenReward", false),
            ("aliceRewardCost", true),
            ("getXmasActivityReward", true),
            ("getPopularItemReward", false),
        ] {
            let sel = sel_of(env, getter);
            let v: i32 = msg_send(env, (gd, sel));
            let set = if positive_only {
                v > 0
            } else if getter == "selectedTotoroGiftData" {
                v != 0
            } else {
                (v & 0xff) != 0
            };
            if set {
                pending = Some(getter);
                break;
            }
        }
        if let Some(getter) = pending {
            log!("[MOLEITEMS] 主村放置拦下:活动兑换标志 {} 未清,原版放置会去连服务器确认而失败", getter);
            return Err(
                "刚做过活动兑换,原版要联网确认才能放下这件物品:去好友村逛一圈回家或重启游戏后再放置"
                    .to_string(),
            );
        }
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
                // [2026-10-06 第九轮 R9-C1b] 对应原版每次进村重新排 60 秒定时器(0x198be);只写原子量。
                PERIODIC_SAVE_NEXT_MS.store(process_ms() + PERIODIC_SAVE_INTERVAL_MS, Ordering::Relaxed);
                PERIODIC_SAVE_ON.store(true, Ordering::Relaxed);
            }
            // [2026-10-03] 负数摩尔豆检查:在线离线都排,只写一个原子(见 NEG_GOLD_CHECK_PENDING)。
            NEG_GOLD_CHECK_PENDING.store(true, Ordering::Relaxed);
            None
        }
        "UserVIPInfoData" => vip_hook(env, sel),
        "iMoleVillageAppDelegate" => app_lifecycle(env, sel),
        _ => None,
    }
}


// ============================================================================
// [2026-10-03] 负数摩尔豆夹回 0(用户拍板)
// ============================================================================

/// 旧版「全物品解锁」(getLockType4Object: 恒返回 0,连余额锁 3/4 一起放开)开着时,摩尔豆不够也能买下去:
/// -[UserInfoData addGold:]@0xbb1d8 只做 gold_ += delta(0xbb20c..0xbb210),不设下限,存档里就留下了负数摩尔豆。
/// 原版里摩尔豆不会为负(各扣款入口都先判余额);贝壳不受影响:-[UserInfoData addVipGold:]@0xbb418 自带下限
/// (0xbb474 相加结果 ≤ -1 就写 encryptInt:0)。岛上花的也是主档 UserInfoData.gold_(NewSceneUserInfoData 没有自己的余额)。
/// 每次进村(-[GameManager startGame:],在线离线都算;从岛回村那一路跑在 CCScheduler 帧栈上)只置这个标志,
/// 由运行循环受理点 neg_gold_check_poll 读 [[GameData sharedInstance] userInfoData] 的 gold,是负数就用原版 setGold:0
/// (纯赋值 0xbd590)写回,再照 addGold: 收尾(0xbb28a..0xbb29c)发 [[WrapperManager sharedManager] updateUserInfoView:2] 刷新 HUD;
/// 下一次存档自然落盘。正数一概不动;夹过一次之后存档里就是 0,以后不会再触发(解除购买门槛现在照原版判余额)。
static NEG_GOLD_CHECK_PENDING: AtomicBool = AtomicBool::new(false);

/// [2026-10-03] 首充大礼包待弹(见 vip_accumulate 注释);FIRST_CHARGE_NEXT_TRY_MS = 下次检查时刻(进程内毫秒),
/// 场景或面板不满足时约 0.5 秒再看一次,不必每帧发消息。
/// [2026-10-04 第八轮 R8-A1] 与 Side.first_charge_pending 同步、随 vip.dat 落盘:以前只在内存,在岛上首充后没回主村就退出,
///   VIP 累计却已写进 vip.dat,下次「旧累计 0」判据永不成立,礼包永久丢失。
static FIRST_CHARGE_PENDING: AtomicBool = AtomicBool::new(false);
static FIRST_CHARGE_NEXT_TRY_MS: AtomicU64 = AtomicU64::new(0);
/// [2026-10-04 第八轮 R8-A2] 充值解锁物(都教授 16283、克劳神父 14974、gamedataFlag|=0x30)待在主村重放落盘;
/// 与 Side.recharge_unlock_pending 同步、随 vip.dat 落盘。见 recharge_unlock_replay。
static RECHARGE_UNLOCK_PENDING: AtomicBool = AtomicBool::new(false);
static RECHARGE_UNLOCK_NEXT_TRY_MS: AtomicU64 = AtomicU64::new(0);

/// [2026-10-06 第九轮 R9-C1b] 离线补原版联网时每 60 秒一次的本地定时落盘。原版 -[GameManager startGame:] 0x198be..0x198ce
/// 排 scheduleSelector:sendGameData2Server: interval:60.0;-[GameManager sendGameData2Server:]@0x20f00 过了各门槛后
/// sendInfoToServer → -[GameData encodeLocalMapData]@0x85300 先 saveUserInfoData(0x85340)、saveMapData(0x85352) 再组包上传。
/// 离线卡在 0x20f7c 的 isReachable 门,这套每分钟的本地落盘整段没了,异常退出(崩溃、被系统杀)时丢失窗口比联网长。
/// 这里只补「本地落盘」那一半:进村(startGame: 臂,离线)置 ON 并把下次时刻排到 60 秒后,运行循环受理点到点后
/// 照原版门槛逐项判,满足就按原版顺序调两个存档方法;不组包、不上传。原版 sendGameData2Server: 是 CCScheduler 回调
/// (帧栈上),所以不在帧里做,只在受理点做。
static PERIODIC_SAVE_ON: AtomicBool = AtomicBool::new(false);
static PERIODIC_SAVE_NEXT_MS: AtomicU64 = AtomicU64::new(0);
static PERIODIC_SAVE_COUNT: AtomicU64 = AtomicU64::new(0);
const PERIODIC_SAVE_INTERVAL_MS: u64 = 60_000;

fn process_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 待办在排队且到了下次检查时刻。
fn due(pending: &AtomicBool, next_try_ms: &AtomicU64) -> bool {
    pending.load(Ordering::Relaxed) && process_ms() >= next_try_ms.load(Ordering::Relaxed)
}

/// ns_run_loop::run_run_loop 主线程每轮都调,平时只有三次原子读。
pub fn run_loop_pending() -> bool {
    // [2026-10-07 第十一轮 R11-F-4] 墙钟跳变检测每轮做一次(只读两个时钟),跳了就排一次昼夜对钟。
    if day_night_clock_jumped() {
        DAY_NIGHT_RESYNC_PENDING.store(true, Ordering::Relaxed);
        DAY_NIGHT_NEXT_TRY_MS.store(0, Ordering::Relaxed);
    }
    NEG_GOLD_CHECK_PENDING.load(Ordering::Relaxed)
        || due(&FIRST_CHARGE_PENDING, &FIRST_CHARGE_NEXT_TRY_MS)
        || due(&RECHARGE_UNLOCK_PENDING, &RECHARGE_UNLOCK_NEXT_TRY_MS)
        || due(&PERIODIC_SAVE_ON, &PERIODIC_SAVE_NEXT_MS)
        || due(&DAY_NIGHT_RESYNC_PENDING, &DAY_NIGHT_NEXT_TRY_MS)
}

/// 运行循环受理点(ns_run_loop,perform 相位之后):栈上没有游戏方法体,可以自由发宿主消息;不在 intercept 里,不碰寄存器。
/// HUD 刷新(updateGold 里要格式化数字串)会产生自动释放对象,包一层池当场 drain(perform 相位本身没有池)。
pub fn run_loop_poll(env: &mut Environment) {
    let neg_gold = NEG_GOLD_CHECK_PENDING.swap(false, Ordering::Relaxed);
    let gift = due(&FIRST_CHARGE_PENDING, &FIRST_CHARGE_NEXT_TRY_MS);
    let unlock = due(&RECHARGE_UNLOCK_PENDING, &RECHARGE_UNLOCK_NEXT_TRY_MS);
    let periodic = due(&PERIODIC_SAVE_ON, &PERIODIC_SAVE_NEXT_MS);
    let day_night = due(&DAY_NIGHT_RESYNC_PENDING, &DAY_NIGHT_NEXT_TRY_MS);
    if !neg_gold && !gift && !unlock && !periodic && !day_night {
        return;
    }
    let pool_cls = env.objc.get_known_class("NSAutoreleasePool", &mut env.mem);
    let new_s = sel_of(env, "new");
    let pool: id = msg_send(env, (pool_cls, new_s));
    if neg_gold {
        neg_gold_check(env);
    }
    if unlock {
        recharge_unlock_replay(env);
    }
    if gift {
        first_charge_gift_poll(env);
    }
    if periodic {
        periodic_local_save_poll(env);
    }
    if day_night {
        day_night_resync(env);
    }
    let drain_s = sel_of(env, "drain");
    let _: () = msg_send(env, (pool, drain_s));
}

/// [2026-10-07 第十一轮 R11-F-4] 昼夜要重新对钟(见 day_night_resync)。不在主村时每 2 秒再试一次。
static DAY_NIGHT_RESYNC_PENDING: AtomicBool = AtomicBool::new(false);
static DAY_NIGHT_NEXT_TRY_MS: AtomicU64 = AtomicU64::new(0);

/// [2026-10-07 第十一轮 R11-F-4] 自上一轮以来墙钟有没有跳变:时间旅行(time_jump_generation 变了),或宿主墙钟相对
/// 单调钟前跳/后退超过 2 分钟(电脑睡眠唤醒——macOS 的 Instant 走 CLOCK_UPTIME_RAW,睡眠期间不走;或改了系统时间)。
/// 每轮运行循环一次,只读两个时钟。
fn day_night_clock_jumped() -> bool {
    use std::cell::Cell;
    use std::time::{Duration, SystemTime};
    thread_local! {
        static LAST: Cell<Option<(SystemTime, Instant, u64)>> = const { Cell::new(None) };
    }
    let now_wall = SystemTime::now();
    let now_mono = Instant::now();
    let generation = crate::libc::time::time_jump_generation();
    let threshold = Duration::from_secs(120);
    LAST.with(|last| {
        let jumped = match last.get() {
            None => false,
            Some((wall, mono, gen)) => {
                let mono_delta = now_mono.duration_since(mono);
                gen != generation
                    || match now_wall.duration_since(wall) {
                        Ok(wall_delta) => wall_delta > mono_delta + threshold,
                        Err(back) => back.duration() > threshold,
                    }
            }
        };
        last.set(Some((now_wall, now_mono, generation)));
        jumped
    })
}

/// [2026-10-07 第十一轮 R11-F-4] 昼夜重新对钟。-[CommonEffectController innerupdate4DayNight:]@0x321f00 每次只把
/// secondsInTaday_ 加 1(按 1 秒排程,一帧最多触发一次,长停顿后只补 1 秒),按 NSCalendar 重新取钟点的
/// resetSecondsInToday 全二进制只有 applicationDidBecomeActive: 调(0x10da4/0x10eac)。真机睡眠/改时间都会经历
/// 失活→激活;桌面电脑睡眠唤醒不发,时间旅行只补发 applicationSignificantTimeChange:(游戏只 setNextDeltaTimeZero:),
/// 于是白天黑夜与真实钟点错开,要最小化再还原或重启才恢复。这里照原版 didBecomeActive 的做法补调
/// resetSecondsInToday 与 checkIsNightComing(重复排程只会更新间隔),只在主村加载完成后做,否则留到之后。
/// 运行循环受理点,不在帧栈上。
fn day_night_resync(env: &mut Environment) {
    DAY_NIGHT_NEXT_TRY_MS.store(process_ms() + 2000, Ordering::Relaxed);
    let Some((scene, _)) = scene_and_mode(env) else {
        return;
    };
    if scene != 1 {
        return;
    }
    let am = shared(env, "ActorManager", "Instance");
    if am == nil {
        return;
    }
    let s = sel_of(env, "m_isLoadMap");
    let loading: u8 = msg_send(env, (am, s));
    if loading != 0 {
        return;
    }
    let cec = shared(env, "CommonEffectController", "sharedManager");
    DAY_NIGHT_RESYNC_PENDING.store(false, Ordering::Relaxed);
    if cec == nil {
        return;
    }
    let s = sel_of(env, "resetSecondsInToday");
    let _: () = msg_send(env, (cec, s));
    let s = sel_of(env, "checkIsNightComing");
    let _: () = msg_send(env, (cec, s));
    log!("[时间] 墙钟跳变(时间旅行或电脑睡眠唤醒):已照 applicationDidBecomeActive: 重新对昼夜钟点");
}

/// [2026-10-06 第九轮 R9-C1b] 定时本地落盘(见 PERIODIC_SAVE_ON)。先把下次时刻推后 60 秒,再照 -[GameManager sendGameData2Server:]
/// 的门槛顺序逐项判:[ActorManager Instance].m_isLoadMap == 0(0x20f42)、(离线没有 isReachable,改判「离线」)、
/// [GameManager gameMode] == 1(0x20f92)、villageLayer 有子节点(0x20fa6/0x20fd0)、[GameData mapdata] 非空(0x20ffe),
/// 另加 curSceneId == 1(岛上、好友村不做);再要求 CFAbsoluteTime 现在 − userInfoData.updateTime < 240
/// (0x2127a..0x21286,常量 0x21308,同一时钟含时间旅行偏移)。注意 -[GameData saveUserInfoData] 自己在 0x75562..0x75578
/// 把 updateTime 刷成当前时刻,所以只要最近 4 分钟内存过一次档(进村、切后台、各事件存档点、上一次定时落盘),之后就是
/// 每 60 秒存一次——原版联网在主村时同样如此(实测挂机 5 分钟每分钟一次);240 秒门只在长时间没存过档时拦一下。
/// 满足后照 -[GameData encodeLocalMapData] 的本地部分:mapdata 条数 ≥ 2(0x8532e)才 saveUserInfoData、saveMapData
/// (新号物件不足 14 件时 saveMapData: 0x76934 由原版自己跳过)。运行循环受理点、自动释放池内调用,不在帧栈上。
fn periodic_local_save_poll(env: &mut Environment) {
    PERIODIC_SAVE_NEXT_MS.store(process_ms() + PERIODIC_SAVE_INTERVAL_MS, Ordering::Relaxed);
    if env.options.network_access {
        PERIODIC_SAVE_ON.store(false, Ordering::Relaxed);
        return;
    }
    let Some((scene, _)) = scene_and_mode(env) else {
        return;
    };
    if scene != 1 {
        return;
    }
    let am = shared(env, "ActorManager", "Instance");
    if am == nil {
        return;
    }
    let s = sel_of(env, "m_isLoadMap");
    let loading: u8 = msg_send(env, (am, s));
    if loading != 0 {
        return;
    }
    let gm = shared(env, "GameManager", "sharedManager");
    if gm == nil {
        return;
    }
    let s = sel_of(env, "gameMode");
    let mode: i32 = msg_send(env, (gm, s));
    if mode != 1 {
        return;
    }
    let s = sel_of(env, "villageLayer");
    let village: id = msg_send(env, (gm, s));
    if village == nil {
        return;
    }
    let s = sel_of(env, "children");
    let children: id = msg_send(env, (village, s));
    if children == nil {
        return;
    }
    let count_s = sel_of(env, "count");
    let n: u32 = msg_send(env, (children, count_s));
    if n == 0 {
        return;
    }
    let gd = shared(env, "GameData", "sharedInstance");
    if gd == nil {
        return;
    }
    let s = sel_of(env, "mapdata");
    let mapdata: id = msg_send(env, (gd, s));
    if mapdata == nil {
        return;
    }
    let entries: u32 = msg_send(env, (mapdata, count_s));
    let s = sel_of(env, "userInfoData");
    let ui: id = msg_send(env, (gd, s));
    if ui == nil {
        return;
    }
    let s = sel_of(env, "updateTime");
    let updated: f64 = msg_send(env, (ui, s));
    let idle = crate::frameworks::core_foundation::time::cf_absolute_time_now() - updated;
    if idle >= 240.0 {
        return;
    }
    if entries < 2 {
        return;
    }
    let s = sel_of(env, "saveUserInfoData");
    let _: () = msg_send(env, (gd, s));
    let s = sel_of(env, "saveMapData");
    let _: () = msg_send(env, (gd, s));
    let k = PERIODIC_SAVE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if k == 1 || k % 10 == 0 {
        log!(
            "[存档] 离线定时落盘(原版 sendGameData2Server: 的本地部分)第 {} 次:距上次改数据 {:.0} 秒",
            k,
            idle
        );
    }
}

/// [SceneMannager curSceneId] 与 [WrapperManager currentGameMode](岛上读 NewGameManager.gameMode,-[WrapperManager
/// currentGameMode]@0x261518 按场景路由);单例拿不到返回 None。
fn scene_and_mode(env: &mut Environment) -> Option<(i32, i32)> {
    let sm = shared(env, "SceneMannager", "sharedManager");
    let wm = shared(env, "WrapperManager", "sharedManager");
    if sm == nil || wm == nil {
        return None;
    }
    let s = sel_of(env, "curSceneId");
    let scene: i32 = msg_send(env, (sm, s));
    let s = sel_of(env, "currentGameMode");
    let mode: i32 = msg_send(env, (wm, s));
    Some((scene, mode))
}

/// [2026-10-04 第八轮] 清掉 vip.dat 里的一个充值待办并写盘(save_blocked 时与 VIP 值一样不落盘)。
fn clear_side_pending(env: &mut Environment, first_charge: bool) {
    {
        let mut s = side();
        if first_charge {
            s.first_charge_pending = false;
        } else {
            s.recharge_unlock_pending = false;
        }
    }
    side_save(env);
}

/// [2026-10-03] 弹首充大礼包(照 parseVipInfo 0x1c0e4a..0x1c0ec0 的调用顺序)。条件不满足就约 0.5 秒后再看(标志保留)。
/// [2026-10-04 第八轮 R8-A1] 用户拍板照原版岛上也当场弹:原版 parseVipInfo 不分场景,礼包层 -[FirstChargeGiftsLayer
///   showWithTarget:selector:]@0x3803cc 只在 0x38040a 要求 currentGameMode==1(岛上读 NewGameManager.gameMode);领取
///   -[WrapperManager onAddFirstChargeGiftToMap:]@0x262f2c 在 0x262f9c 判 curSceneId==10 走岛菜单
///   -[NewSceneVillageMenuLayer onAddFirstChargeGiftToMap:]@0x25d098 摆到岛上。主村:不在岛会话、curSceneId 1;岛上:稳稳在岛上
///   (island_settled:不在进岛窗口/加载/离岛过场)、curSceneId 10;两边都要 currentGameMode 1。礼包层确实挂上([layer parent] 非 nil)
///   才清待办并写盘;currentGameMode≠1 时原版在 0x38040c 静默返回,不能据此清位。
fn first_charge_gift_poll(env: &mut Environment) {
    if env.options.network_access {
        // 在线时 VIP 三值与首充礼包都由服务器回包(parseVipInfo)负责。
        FIRST_CHARGE_PENDING.store(false, O);
        return;
    }
    FIRST_CHARGE_NEXT_TRY_MS.store(process_ms() + 500, O);
    let Some((scene, mode)) = scene_and_mode(env) else {
        return;
    };
    let main_ok = !crate::mole_cheats::island_session_active() && scene == 1 && mode == 1;
    let island_ok = crate::mole_cheats::island_settled() && scene == 10 && mode == 1;
    if !main_ok && !island_ok {
        return;
    }
    let wm = shared(env, "WrapperManager", "sharedManager");
    let layer_cls = env
        .objc
        .get_known_class("FirstChargeGiftsLayer", &mut env.mem);
    let nm = shared(env, "NetworkManager", "sharedInstance");
    if layer_cls == nil || wm == nil {
        FIRST_CHARGE_PENDING.store(false, O);
        clear_side_pending(env, true);
        log!("[MOLEITEMS] ⚠️ 首充大礼包:找不到 FirstChargeGiftsLayer 类或 WrapperManager,放弃");
        return;
    }
    let s = sel_of(env, "initFirstChargeGifts");
    let _: () = msg_send(env, (wm, s));
    let s = sel_of(env, "firstChargeGiftsArray");
    let gifts: id = msg_send(env, (wm, s));
    let s = sel_of(env, "layerWithRewards:");
    let layer: id = msg_send(env, (layer_cls, s, gifts));
    if layer == nil {
        // 礼物表都建不出来,重试也不会好:放弃并清待办,免得每 0.5 秒刷一次。
        FIRST_CHARGE_PENDING.store(false, O);
        clear_side_pending(env, true);
        log!("[MOLEITEMS] ⚠️ 首充大礼包:layerWithRewards: 返回 nil,没有弹出");
        return;
    }
    let s = sel_of(env, "showWithTarget:selector:");
    let null_sel: SEL = SEL::null();
    let _: () = msg_send(env, (layer, s, nm, null_sel));
    let s = sel_of(env, "parent");
    let parent: id = msg_send(env, (layer, s));
    if parent == nil {
        log_dbg!("[MOLEITEMS] 首充大礼包:礼包层没挂上(currentGameMode 可能刚变),0.5 秒后再试");
        return;
    }
    FIRST_CHARGE_PENDING.store(false, O);
    clear_side_pending(env, true);
    let count: u32 = if gifts == nil {
        0
    } else {
        let s = sel_of(env, "count");
        msg_send(env, (gifts, s))
    };
    log!(
        "[MOLEITEMS] 首充大礼包:照原版 parseVipInfo 顺序 initFirstChargeGifts → layerWithRewards:({} 件) → showWithTarget:NetworkManager selector:nil 已弹出({})",
        count,
        if scene == 10 { "黄金岛上当场弹" } else { "主村" }
    );
}

/// [2026-10-04 第八轮 R8-A2] 充值解锁物回主村重放落盘。原版 -[GameData addAlreadyPurchaseVipgoldWithPurchaseInfo:] 改内存后
/// saveToLocal(0x7f3dc/0x7f476),而 saveToLocal 在岛上 0x7cb3e 早退、-[WrapperManager unlockItem:]@0x2612e8 只改内存,回村
/// loadFromLocal 对键 13 按位或(0x1ee3c)、对键 63 盘上有就覆盖(0x2007a)。所以在不在岛会话、curSceneId 1、currentGameMode 1、
/// 主村地图加载完([[ActorManager Instance] m_isLoadMap]==0:键 63 已处理完,-[GameData saveMapData:] 0x768fa 不会因加载中静默跳过)
/// 时重放 recharge_unlock_side_effects(unlockItem: 内部 0x26137e 起枚举去重,gamedataFlag 按位或,都是幂等的)并 saveToLocal,
/// 然后清待办写盘。主村内充值当场已存过一次,这里再重放一次无害,也兜住充值时 gameMode≠1 致 saveToLocal 被跳过的情形。
fn recharge_unlock_replay(env: &mut Environment) {
    if env.options.network_access {
        RECHARGE_UNLOCK_PENDING.store(false, O);
        return;
    }
    RECHARGE_UNLOCK_NEXT_TRY_MS.store(process_ms() + 500, O);
    if crate::mole_cheats::island_session_active() {
        return;
    }
    let Some((scene, mode)) = scene_and_mode(env) else {
        return;
    };
    if scene != 1 || mode != 1 {
        return;
    }
    let am = shared(env, "ActorManager", "Instance");
    if am == nil {
        return;
    }
    let s = sel_of(env, "m_isLoadMap");
    let loading: bool = msg_send(env, (am, s));
    if loading {
        return;
    }
    recharge_unlock_side_effects(env);
    RECHARGE_UNLOCK_PENDING.store(false, O);
    clear_side_pending(env, false);
    log!("[MOLEITEMS] 充值解锁物:主村地图已加载完,照原版重放 unlockItem:/gamedataFlag 并 saveToLocal 落盘,待办已清");
}

fn neg_gold_check(env: &mut Environment) {
    let gd = shared(env, "GameData", "sharedInstance");
    if gd == nil {
        return;
    }
    let ui_s = sel_of(env, "userInfoData");
    let ui: id = msg_send(env, (gd, ui_s));
    if ui == nil {
        return;
    }
    let gold_s = sel_of(env, "gold");
    let gold: i32 = msg_send(env, (ui, gold_s));
    if gold >= 0 {
        return;
    }
    let set_s = sel_of(env, "setGold:");
    let _: () = msg_send(env, (ui, set_s, 0i32));
    let wm = shared(env, "WrapperManager", "sharedManager");
    if wm != nil {
        let upd_s = sel_of(env, "updateUserInfoView:");
        let _: () = msg_send(env, (wm, upd_s, 2i32));
    }
    log!(
        "[MOLEITEMS] 存档里的摩尔豆是负数({}),多半是旧版「全物品解锁」在钱不够时买下的;已按原版不会出现负数的规则夹回 0(下次存档落盘)",
        gold
    );
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

// [2026-09-16] F2-07 统一节日日历的纯函数单测(商店与活动模块共用 festival_on_date / parse_festival_env)。
#[cfg(test)]
mod festival_calendar_tests {
    use super::*;

    #[test]
    fn unified_calendar_dates() {
        // 12/15、1/3 在圣诞窗口;2027 年初一是 2/6,前 10 天(1/27)与当天都在新年窗口
        assert!(festival_on_date("christmas", days_from_civil(2026, 12, 15)));
        assert!(festival_on_date("christmas", days_from_civil(2027, 1, 3)));
        assert!(!festival_on_date("newyear", days_from_civil(2026, 12, 15)));
        assert!(festival_on_date("newyear", days_from_civil(2027, 1, 27)));
        assert!(festival_on_date("newyear", days_from_civil(2027, 2, 6)));
        assert!(!festival_on_date("christmas", days_from_civil(2027, 2, 6)));
        assert!(!festival_on_date(
            "no_such_festival",
            days_from_civil(2027, 2, 6)
        ));
    }

    #[test]
    fn christmas_and_newyear_windows_never_overlap() {
        // mole_activity::festival_today 按「先圣诞后春节」判定,依赖两个窗口不重叠
        for y in 2020..=2041i64 {
            let end = days_from_civil(y + 1, 1, 1);
            let mut d = days_from_civil(y, 1, 1);
            while d < end {
                assert!(!(festival_on_date("christmas", d) && festival_on_date("newyear", d)));
                d += 1;
            }
        }
    }

    #[test]
    fn festival_env_aliases() {
        assert!(
            matches!(parse_festival_env("spring"), Some(FestEnv::Only(i)) if FESTIVALS[i].key == "newyear")
        );
        assert!(
            matches!(parse_festival_env(" XMAS "), Some(FestEnv::Only(i)) if FESTIVALS[i].key == "christmas")
        );
        assert!(matches!(parse_festival_env("all"), Some(FestEnv::All)));
        assert!(matches!(parse_festival_env("none"), Some(FestEnv::Off)));
        assert!(matches!(parse_festival_env(""), Some(FestEnv::ByDate)));
        assert!(parse_festival_env("bogus").is_none());
    }
}
