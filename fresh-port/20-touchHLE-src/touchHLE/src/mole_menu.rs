/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! MoleWorld offline port: built-in debug / cheat menu.
//!
//! A native-Rust re-implementation of the user's real-device Substrate tweak
//! menu (`moletweak/Tweak.xm`). The original is host ObjC injected into the
//! game; here the menu is rebuilt inside touchHLE using the guest UIKit
//! (UIView/UILabel) for rendering, and each button runs the *real* game ObjC
//! logic by sending messages so the effects are genuine game behaviour.
//!
//! Toggle with the **T** key. The menu is laid out in landscape-logical
//! (1024x768) coordinates; the container is rotated -90° + centred so it shows
//! upright on touchHLE's LandscapeRight display. It is organised into pages
//! (tabs along the top) to fit the full tweak feature set.

use crate::frameworks::core_graphics::cg_affine_transform::CGAffineTransform;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::from_rust_string;
use crate::mem::{MutVoidPtr, Ptr};
use crate::objc::{id, msg, msg_class, msg_send, nil, release, retain, SEL};
use crate::Environment;
use std::cell::{Cell, RefCell};

/// 点击式滑块控制的数值种类(读游戏实时值 + 点条按比例设值)。
#[derive(Clone, Copy, PartialEq)]
pub enum SliderKind {
    Level,
    Gold,
    VipGold,
    Workers,
    Rooms,
}

/// What a button does when tapped. Each variant maps to a real game call (or a
/// menu-internal action like switching page).
#[derive(Clone, Copy)]
pub enum Action {
    /// 点击式滑块:点格子内某 x 位置 = 把该数值设到 (x比例 × 上限)。
    Slider(SliderKind),
    Close,
    /// Switch to the given page index.
    SwitchPage(usize),
    /// `-[GameData addVipGoldForBuy:UIUpdate:]` — grant 贝壳 (shells).
    AddVipGold(i32),
    /// Call a `TestLayer` ± method on an un-parented "ghost" instance N times.
    GhostTL(&'static str, u32),
    /// `[[NSClassFromString(name) alloc] init]` added to the running scene at z.
    SummonClass(&'static str, i32),
    /// `-[MiniGameManager startMiniGame:playType:callbackTarget:select:]`.
    MiniGame(i32),
    /// `[GameData <sel>]` then `-[GameData saveUserInfoData]`.
    GameDataReset(&'static str),
    /// `[GameManager <sel>]` (e.g. addTreasureReward).
    GameManagerCall(&'static str),
    /// `[[<class> <shared>] <method>]` —— 在某单例上调无参方法(如昼夜切换)。
    SingletonCall(&'static str, &'static str, &'static str),
    /// 关闭最近召唤的层(GM 面板等):removeFromParentAndCleanup。
    CloseSummoned,
    /// 删本地存档文件(userinfo.dat/map.dat)→ 下次启动为全新存档。
    ResetLocalSave,
    /// 一键拷贝丝尔特(xiaotulv)家园地图到玩家自己存档(覆盖、不可逆;handle_touch 里二次确认)。
    CopyXiaoTuLvHome,
    /// `[[GameData userInfoData] <sel>:val]` then save.
    UserInfoSet(&'static str, i32),
    /// Set total + available workers, then save.
    SetWorkers(i32),
    /// Load a bundled demo map + userinfo resource and reload the scene.
    EnterXiaoTuLv(&'static str, &'static str),
    /// Flip a toggle-style cheat (see `mole_cheats`) by its key.
    ToggleCheat(&'static str),
    /// Cycle the forced VIP level (1..=15) and enable force_vip.
    VipLevelInc,
    /// Cycle the forced player level (0/10/.../100) — overrides curLevel.
    LevelInc,
    /// Set the avatar icon: `setAvatarIcon:` + `setIconIndex:`, then save.
    SetAvatar(i32),
    /// `[[GameData sharedInstance] <sel>:val]` then save (e.g. setRewardTickets:).
    GameDataSetInt(&'static str, i32),
    /// Mature + harvest every Farm via `ObjectManager.farms` -> cropMatureHandler.
    HarvestAll,
    /// Open the Golden Island (Caribbean) activity offline: enable the fix,
    /// build+set its data, create the layer and force `displayUI`.
    OpenCaribbean,
    /// 一键进入 NewScene 可建筑黄金岛(scene id 10):arm 进岛(开功能/开窗/注入默认岛
    /// mapData)后直接 `[SceneMannager startNewSceneFrom:1 toScene:10]`。
    EnterIsland,
}

struct Button {
    frame: CGRect,
    action: Action,
    label: &'static str,
}

struct MenuState {
    open: bool,
    container: id,
    buttons: Vec<Button>,
}

thread_local! {
    static MENU: RefCell<MenuState> = RefCell::new(MenuState {
        open: false,
        container: nil,
        buttons: Vec::new(),
    });
    static GHOST: Cell<id> = const { Cell::new(nil) };
    static CURRENT_PAGE: Cell<usize> = const { Cell::new(0) };
    // 最近一次 SummonClass 召唤的层(retain 持有),供「关闭召唤层」用。
    static LAST_SUMMONED: Cell<id> = const { Cell::new(nil) };
    // 底部 toast 文本(最近一次操作反馈);删本地存档的二次确认待定态。
    static TOAST: RefCell<String> = const { RefCell::new(String::new()) };
    static PENDING_RESET: Cell<bool> = const { Cell::new(false) };
    // 拷贝丝尔特家园的二次确认待定态。
    static PENDING_COPY: Cell<bool> = const { Cell::new(false) };
}

struct Page {
    title: &'static str,
    buttons: Vec<(&'static str, Action)>,
}

fn pages() -> Vec<Page> {
    use Action::*;
    vec![
        Page {
            title: "数值",
            buttons: vec![
                // 点击式滑块:显示游戏实时值,点条某位置=按比例设值(等级 1-52)。配合下面 +/- 微调。
                ("等级", Slider(SliderKind::Level)),
                ("摩尔豆", Slider(SliderKind::Gold)),
                ("贝壳", Slider(SliderKind::VipGold)),
                ("工人", Slider(SliderKind::Workers)),
                ("房间", Slider(SliderKind::Rooms)),
                ("经验 +1", GhostTL("onButtonXPPlus:", 1)),
                ("经验 +10", GhostTL("onButtonXPPlus:", 10)),
                ("经验 +100", GhostTL("onButtonXPPlus:", 100)),
                ("经验 -1", GhostTL("onButtonXPMinus:", 1)),
                ("经验 -10", GhostTL("onButtonXPMinus:", 10)),
                ("摩尔豆 +1", GhostTL("onButtonGoldPlus:", 1)),
                ("摩尔豆 +10", GhostTL("onButtonGoldPlus:", 10)),
                ("摩尔豆 +100", GhostTL("onButtonGoldPlus:", 100)),
                ("摩尔豆 -1", GhostTL("onButtonGoldMinus:", 1)),
                ("摩尔豆 -10", GhostTL("onButtonGoldMinus:", 10)),
                ("贝壳 +1", GhostTL("onButtonVipGoldPlus:", 1)),
                ("贝壳 +10", GhostTL("onButtonVipGoldPlus:", 10)),
                ("贝壳 +1000", AddVipGold(1000)),
                ("VIP值 +1", GhostTL("onButtonVipValuePlus:", 1)),
                ("食物 +1", GhostTL("onButtonFoodPlus:", 1)),
                ("奖励券 +1", GhostTL("onButtonTicketsPlus:", 1)),
                ("时间 +1", GhostTL("onButtonTimePlus:", 1)),
                ("任务进度 +1", GhostTL("onButtonQuestPlus:", 1)),
                ("限时任务 +1", GhostTL("onButtonTimeQuestPlus:", 1)),
                ("VIP任务 +1", GhostTL("onButtonVipQuestPlus:", 1)),
                ("工人数 = 20", SetWorkers(20)),
                ("房间数 = 20", UserInfoSet("setTotalRooms:", 20)),
            ],
        },
        // 召唤(NPC/功能层 + 活动层合并)。活动层多为联网驱动,离线只能召唤观赏(标 弃用)。
        Page {
            title: "召唤",
            buttons: vec![
                ("超级贝壳树", SummonClass("SuperShellTree", 88888)),
                ("免费贝壳墙", SummonClass("ShowFreeShellsLayer", 88888)),
                ("水塔", SummonClass("WaterTower", 88888)),
                ("乌鸦祭司", SummonClass("CrowPriest", 88888)),
                ("圣诞树", SummonClass("ChrismasTreeView", 88888)),
                ("村庄菜单层", SummonClass("VillageMenuLayer", 88888)),
                ("新版商店(勿点!易卡)", SummonClass("NewStyleStoreMainLayer", 88888)),
                ("促销主层", SummonClass("PromoteSalesMainLayer", 88888)),
                ("圣诞主活动(弃用)", SummonClass("XmasMainLayer", 88888)),
                ("彩蛋主面板(弃用)", SummonClass("EasterEggMainLayer", 88888)),
                ("周年纪念(弃用)", SummonClass("AnniversaryMainLayer", 88888)),
                ("秋季活动(弃用)", SummonClass("AutumnMainLayer", 88888)),
                ("万圣节(弃用)", SummonClass("HalloweenMainLayer", 88888)),
                ("Naram春活(弃用)", SummonClass("NaramSpringMainLayer", 88888)),
                ("爱丽丝梦游(弃用)", SummonClass("Activity_Alice_MainLayer", 88888)),
                ("史莱克(弃用)", SummonClass("Activity_Shrek_BasePopLayer", 88888)),
                ("龙猫(弃用)", SummonClass("Activity_Totoro_BasePopLayer", 88888)),
                ("冰激凌(弃用)", SummonClass("Activity_IceCream_BasePopLayer", 88888)),
                ("火焰战争(弃用)", SummonClass("Activity_FlameWars_MainLayer", 88888)),
                ("加勒比黄金岛(弃用)", SummonClass("CaribbeanMainLayer", 88888)),
                ("海底寻宝(弃用)", SummonClass("SeabedSeekingTreasureMainLayer", 88888)),
                ("环游世界(弃用)", SummonClass("AroundTheWorldMainLayer", 88888)),
                ("春天的诗(弃用)", SummonClass("SpringPoemMainLayer", 88888)),
                ("放风筝(弃用)", SummonClass("FlyKiteMainLayer", 88888)),
                ("清明青团(弃用)", SummonClass("GreenRiceBallMainLayer", 88888)),
                ("开宝箱(弃用)", SummonClass("OpenTreasureChestMainLayer", 88888)),
                ("世界杯竞猜(弃用)", SummonClass("GuessWorldCupMainLayer", 88888)),
                ("冰夏(弃用)", SummonClass("IceSummerMainLayer", 88888)),
                ("广告墙板(弃用)", SummonClass("ShowAdwallBoardLayer", 88888)),
                ("更多好友(弃用)", SummonClass("ShowMoreFriendsLayer", 88888)),
                ("活动规则层(弃用)", SummonClass("ShowActivityRuleLayer", 88888)),
            ],
        },
        Page {
            title: "Mini/任务/重置",
            buttons: vec![
                ("Mini: 切水果", MiniGame(1)),
                ("Mini: 拍虫子", MiniGame(2)),
                ("Mini: 挖矿石", MiniGame(3)),
                ("Mini: 敲木桩", MiniGame(4)),
                ("Mini: 钓鱼", MiniGame(5)),
                ("丝尔特(春)", EnterXiaoTuLv("xiaotulv_map", "xiaotulv_userinfo")),
                ("丝尔特(冬)", EnterXiaoTuLv("xiaotulv_winter_map", "xiaotulv_winter_userinfo")),
                ("⚠️拷贝丝尔特家园到自己(覆盖存档!)", CopyXiaoTuLvHome),
                ("给宝藏奖励", GameManagerCall("addTreasureReward")),
                ("给宝藏兔奖励", GameManagerCall("addTreasureRabbitReward")),
                ("强开剧情任务", GameManagerCall("activateStoryQuest")),
                ("重置每日任务", GameDataReset("resetUnfinishedDailyQuestDataInMap")),
                ("重置限时任务", GameDataReset("resetTimeQuestDataInMap")),
                ("重置VIP任务", GameDataReset("resetVipQuestDataInMap")),
                ("重置今日签到", GameDataReset("resetLastGetDailyRewardDay")),
                ("重置每日列表", GameDataReset("resetDailyQuestList")),
                ("重置宝箱数据", GameDataReset("resetTreasureChestData")),
                ("重置加勒比", GameDataReset("resetCaribbeanData")),
                ("⚠️整库重置", GameDataReset("resetUserGameData")),
            ],
        },
        // 开关 + 解锁/成就/收获 合并(VIP等级/强制VIP/购物免费 已移到「开发者/调试」)。
        Page {
            title: "开关/解锁/成就",
            buttons: vec![
                ("金币 x10", ToggleCheat("gold_x10")),
                ("经验 x10", ToggleCheat("xp_x10")),
                ("关反作弊检测", ToggleCheat("kill_anticheat")),
                ("作物瞬熟", ToggleCheat("instant_crop")),
                ("永不枯萎", ToggleCheat("no_wither")),
                ("冷却归零", ToggleCheat("no_cooldown")),
                ("建筑瞬完成", ToggleCheat("instant_build")),
                ("工人房间补满", ToggleCheat("max_facility")),
                ("产出×10(收菜)", ToggleCheat("harvest_mult")),
                ("任务秒完成免费", ToggleCheat("free_quest")),
                ("小游戏奖励满", ToggleCheat("minigame_reward")),
                ("海底寻宝必中稀有", ToggleCheat("seabed_best")),
                ("等级", LevelInc),
                ("全物品解锁", ToggleCheat("all_unlock")),
                ("全成就通过", ToggleCheat("all_achieve")),
                ("魔法密码任意过", ToggleCheat("magic_bypass")),
                ("头像 = 1", SetAvatar(1)),
                ("头像 = 10", SetAvatar(10)),
                ("头像 = 30", SetAvatar(30)),
                ("头像 = 61", SetAvatar(61)),
                ("奖励券 = 100", GameDataSetInt("setRewardTickets:", 100)),
                ("奖励券 = 500", GameDataSetInt("setRewardTickets:", 500)),
                ("一键收获全部", HarvestAll),
            ],
        },
        // 破解功能 + 开发者/调试 + VIP + 黄金岛 + 购物免费 合并(保留"开发者/调试"名)。
        Page {
            title: "开发者 / 调试",
            buttons: vec![
                // —— 破解功能(香草基底·默认关;开=往模拟内存写破解精确字节复刻,关=还原香草)——
                ("去越狱检测", ToggleCheat("kill_jailbreak")),
                ("修复占卜功能(默认开)", ToggleCheat("fix_divine")),
                ("节日村进入", ToggleCheat("enter_holiday")),
                ("商城免VIP等级", ToggleCheat("store_no_vip")),
                ("进新岛门(默认开·护黄金岛)", ToggleCheat("enter_newislands")),
                ("跳对象数据校验", ToggleCheat("skip_parse_check")),
                // —— VIP / 购物免费(从开关页移来)——
                ("强制VIP", ToggleCheat("force_vip")),
                ("VIP等级", VipLevelInc),
                ("购物免费", ToggleCheat("free_shop")),
                // —— 黄金岛(从黄金岛页移来)——
                ("▶ 一键进入黄金岛", EnterIsland),
                ("可建筑黄金岛·热点开关", ToggleCheat("enable_newscene_island")),
                ("修复加勒比寻宝", ToggleCheat("fix_golden_island")),
                ("直达终点(弃用)", ToggleCheat("golden_win")),
                ("打开加勒比黄金岛(弃用)", OpenCaribbean),
                // —— 调试工具 ——
                ("✖ 关闭召唤层(GM面板等)", CloseSummoned),
                ("GM面板 TestLayer", SummonClass("TestLayer", 99999)),
                ("黄金岛GM面板 NewSceneTestLayer", SummonClass("NewSceneTestLayer", 99999)),
                ("切到夜晚", SingletonCall("CommonEffectController", "sharedManager", "formDayToNight")),
                ("切回白天", SingletonCall("CommonEffectController", "sharedManager", "fromNightToDaybreak")),
                ("⚠️ 删本地存档(下次启动全新)", ResetLocalSave),
            ],
        },
    ]
}

pub fn is_open() -> bool {
    MENU.with(|m| m.borrow().open)
}

pub fn toggle(env: &mut Environment) {
    if is_open() {
        teardown(env);
    } else {
        CURRENT_PAGE.with(|c| c.set(0));
        build(env);
    }
}

fn color(env: &mut Environment, r: CGFloat, g: CGFloat, b: CGFloat, a: CGFloat) -> id {
    msg_class![env; UIColor colorWithRed:r green:g blue:b alpha:a]
}

fn add_label(env: &mut Environment, container: id, frame: CGRect, text: &str, bg: id, fg: id) {
    let lbl: id = msg_class![env; UILabel alloc];
    let lbl: id = msg![env; lbl initWithFrame:frame];
    let t: id = from_rust_string(env, text.to_string());
    () = msg![env; lbl setText:t];
    () = msg![env; lbl setBackgroundColor:bg];
    () = msg![env; lbl setTextColor:fg];
    () = msg![env; lbl setTextAlignment:1i32]; // centered
    () = msg![env; container addSubview:lbl];
    release(env, lbl);
}

fn build(env: &mut Environment) {
    let app: id = msg_class![env; UIApplication sharedApplication];
    let window: id = msg![env; app keyWindow];
    if window == nil {
        log!("[MOLEMENU] no key window yet; cannot open menu");
        return;
    }
    let page_idx = CURRENT_PAGE.with(|c| c.get());
    let all_pages = pages();

    let full = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 1024.0,
            height: 768.0,
        },
    };
    let container: id = msg_class![env; UIView alloc];
    let container: id = msg![env; container initWithFrame:full];
    let dim = color(env, 0.05, 0.05, 0.08, 0.82);
    () = msg![env; container setBackgroundColor:dim];

    let white = color(env, 1.0, 1.0, 1.0, 1.0);
    let mut buttons = Vec::new();

    // Top row: a Close button + one tab per page.
    let tab_count = all_pages.len() + 1;
    let tab_w = (1024.0 - 32.0 - (tab_count as f32 - 1.0) * 8.0) / tab_count as f32;
    for i in 0..tab_count {
        let tx = 16.0 + i as f32 * (tab_w + 8.0);
        let frame = CGRect {
            origin: CGPoint { x: tx, y: 14.0 },
            size: CGSize {
                width: tab_w,
                height: 40.0,
            },
        };
        let (label, action, bg) = if i == 0 {
            ("✕ 关闭", Action::Close, color(env, 0.62, 0.18, 0.18, 1.0))
        } else {
            let p = i - 1;
            let selected = p == page_idx;
            let bg = if selected {
                color(env, 1.0, 0.7, 0.2, 1.0)
            } else {
                color(env, 0.3, 0.32, 0.4, 1.0)
            };
            (all_pages[p].title, Action::SwitchPage(p), bg)
        };
        add_label(env, container, frame, label, bg, white);
        buttons.push(Button { frame, action, label });
    }

    // Body: current page's buttons in a 3-column grid.
    let page = &all_pages[page_idx];
    let cols = 3usize;
    let per_col = page.buttons.len().div_ceil(cols);
    let (left, top, col_w, hgap, bh, vgap) = (16.0f32, 64.0f32, 320.0f32, 12.0f32, 40.0f32, 7.0f32);
    for (i, (label, action)) in page.buttons.iter().enumerate() {
        let col = i / per_col;
        let row = i % per_col;
        let bx = left + col as f32 * (col_w + hgap);
        let by = top + row as f32 * (bh + vgap);
        let frame = CGRect {
            origin: CGPoint { x: bx, y: by },
            size: CGSize {
                width: col_w,
                height: bh,
            },
        };
        // 滑块格子:自定义渲染(轨道+填充+实时值),点击在 handle_touch 里按比例设值。
        if let Action::Slider(kind) = action {
            render_slider(env, container, frame, *kind, white);
            buttons.push(Button {
                frame,
                action: *action,
                label: *label,
            });
            continue;
        }
        let (display, bg) = match action {
            Action::ToggleCheat(key) => {
                let on = crate::mole_cheats::is_on(key);
                let c = if on {
                    color(env, 0.2, 0.62, 0.28, 1.0)
                } else {
                    color(env, 0.45, 0.3, 0.32, 1.0)
                };
                (format!("{}: {}", label, if on { "开" } else { "关" }), c)
            }
            Action::VipLevelInc => (
                format!("VIP等级={} (点+)", crate::mole_cheats::vip_level()),
                color(env, 0.2, 0.62, 0.28, 1.0),
            ),
            Action::LevelInc => {
                let lv = crate::mole_cheats::level();
                let c = if lv > 0 {
                    color(env, 0.2, 0.62, 0.28, 1.0)
                } else {
                    color(env, 0.45, 0.3, 0.32, 1.0)
                };
                (format!("等级={} (点+10)", lv), c)
            }
            Action::MiniGame(_) => (label.to_string(), color(env, 0.18, 0.5, 0.3, 1.0)),
            Action::SummonClass(..) => (label.to_string(), color(env, 0.5, 0.35, 0.65, 1.0)),
            Action::GameDataReset(_) | Action::GameManagerCall(_) | Action::ResetLocalSave => {
                (label.to_string(), color(env, 0.55, 0.4, 0.18, 1.0))
            }
            Action::EnterXiaoTuLv(..) => (label.to_string(), color(env, 0.2, 0.5, 0.55, 1.0)),
            _ => (label.to_string(), color(env, 0.16, 0.45, 0.7, 1.0)),
        };
        add_label(env, container, frame, &display, bg, white);
        buttons.push(Button {
            frame,
            action: *action,
            label: *label,
        });
    }

    // 底部 toast:最近一次操作反馈(已开启/已关闭、删档确认/完成、已执行)。
    let toast = TOAST.with(|t| t.borrow().clone());
    if !toast.is_empty() {
        let tframe = CGRect {
            origin: CGPoint { x: 16.0, y: 722.0 },
            size: CGSize {
                width: 992.0,
                height: 38.0,
            },
        };
        let tbg = color(env, 0.08, 0.09, 0.12, 0.96);
        add_label(env, container, tframe, &toast, tbg, white);
    }

    // Rotate -90° + centre so the landscape layout shows upright.
    let rot = CGAffineTransform {
        a: 0.0,
        b: -1.0,
        c: 1.0,
        d: 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    () = msg![env; container setTransform:rot];
    let center = CGPoint { x: 384.0, y: 512.0 };
    () = msg![env; container setCenter:center];

    () = msg![env; window addSubview:container];
    retain(env, container);

    // Fade the menu in via the (now-restored) legacy UIView animation block.
    // Doubles as the live exercise of that code path: opacity 0 -> 1 over 0.2s.
    () = msg![env; container setAlpha:0.0f32];
    let null_ctx: MutVoidPtr = Ptr::null();
    () = msg_class![env; UIView beginAnimations:nil context:null_ctx];
    let dur: f64 = 0.2;
    () = msg_class![env; UIView setAnimationDuration:dur];
    () = msg![env; container setAlpha:1.0f32];
    () = msg_class![env; UIView commitAnimations];

    MENU.with(|m| {
        let mut s = m.borrow_mut();
        s.open = true;
        s.container = container;
        s.buttons = buttons;
    });
    log!("[MOLEMENU] opened page {} ({})", page_idx, page.title);
}

fn remove_container(env: &mut Environment) {
    let container = MENU.with(|m| m.borrow().container);
    if container != nil {
        () = msg![env; container removeFromSuperview];
        release(env, container);
    }
    MENU.with(|m| {
        let mut s = m.borrow_mut();
        s.container = nil;
        s.buttons.clear();
    });
}

/// Tear down and re-lay-out the menu in place — used after an action changes
/// state (page switch, cheat toggle, level bump) so the labels refresh.
fn rebuild(env: &mut Environment) {
    remove_container(env);
    build(env);
}

fn teardown(env: &mut Environment) {
    remove_container(env);
    MENU.with(|m| m.borrow_mut().open = false);
    CURRENT_PAGE.with(|c| c.set(0));
    TOAST.with(|t| t.borrow_mut().clear());
    PENDING_RESET.with(|c| c.set(false));
    log!("[MOLEMENU] closed");
}

/// 设置底部 toast 文本(下次 present/rebuild 时渲染)。
fn set_toast(s: String) {
    TOAST.with(|t| *t.borrow_mut() = s);
}

fn in_rect(x: f32, y: f32, r: CGRect) -> bool {
    x >= r.origin.x
        && x <= r.origin.x + r.size.width
        && y >= r.origin.y
        && y <= r.origin.y + r.size.height
}

pub fn handle_touch(env: &mut Environment, gx: f32, gy: f32) -> bool {
    if !is_open() {
        return false;
    }
    // Guest (portrait 768x1024) -> landscape-logical (1024x768).
    let lx = 1024.0 - gy;
    let ly = gx;
    let hit = MENU.with(|m| {
        m.borrow()
            .buttons
            .iter()
            .find(|b| in_rect(lx, ly, b.frame))
            .map(|b| (b.action, b.label, b.frame))
    });
    if let Some((action, label, frame)) = hit {
        // 数值显示格(原"滑块"改为只读实时值显示):点击只回显当前值,改值用下面的 +/- 按钮。
        let _ = frame; // 不再用 tap 坐标(滑块已弃用)
        if let Action::Slider(kind) = action {
            let (name, cur, max) = slider_info(env, kind);
            set_toast(format!("{} 当前 {}/{}(只读·用 +/- 改)", name, cur, max));
            rebuild(env);
            return true;
        }
        // 二次确认类:第一次只提示,第二次才执行;点别的按钮则取消所有待确认。
        if matches!(action, Action::ResetLocalSave) {
            if !PENDING_RESET.with(|c| c.get()) {
                PENDING_RESET.with(|c| c.set(true));
                PENDING_COPY.with(|c| c.set(false));
                set_toast("⚠️ 再点一次「删本地存档」确认(将清空本地存档)".to_string());
                rebuild(env);
                return true;
            }
            PENDING_RESET.with(|c| c.set(false)); // 已确认,下面真删
        } else if matches!(action, Action::CopyXiaoTuLvHome) {
            if !PENDING_COPY.with(|c| c.get()) {
                PENDING_COPY.with(|c| c.set(true));
                PENDING_RESET.with(|c| c.set(false));
                set_toast(
                    "⚠️ 确认要这样吗?会覆盖你的存档、不可逆!再点一次「拷贝丝尔特家园」确认"
                        .to_string(),
                );
                rebuild(env);
                return true;
            }
            PENDING_COPY.with(|c| c.set(false)); // 已确认,下面真拷贝
        } else {
            PENDING_RESET.with(|c| c.set(false));
            PENDING_COPY.with(|c| c.set(false));
        }
        run_action(env, action);
        // 底部 toast 反馈
        match action {
            Action::ToggleCheat(key) => {
                let on = crate::mole_cheats::is_on(key);
                set_toast(format!("「{}」已{}", label, if on { "开启" } else { "关闭" }));
            }
            Action::ResetLocalSave => {
                set_toast("已删本地存档;退出游戏后即为全新存档".to_string())
            }
            Action::CopyXiaoTuLvHome => {
                set_toast("已拷贝丝尔特家园到你的存档(已重载)".to_string())
            }
            Action::Close | Action::SwitchPage(_) => {} // 导航不提示
            _ => set_toast(format!("「{}」已执行", label)),
        }
        // 刷新菜单以显示 toast(若仍打开;Close 已关闭则跳过)
        if MENU.with(|m| m.borrow().open) {
            rebuild(env);
        }
    }
    true
}

fn game_singleton(env: &mut Environment, class_name: &str, selector: &str) -> id {
    let class = env.objc.get_known_class(class_name, &mut env.mem);
    if class == nil {
        return nil;
    }
    let s = sel(env, selector);
    msg_send(env, (class, s))
}

fn sel(env: &mut Environment, name: &str) -> SEL {
    env.objc
        .register_host_selector(name.to_string(), &mut env.mem)
}

fn run_action(env: &mut Environment, action: Action) {
    match action {
        Action::Close => teardown(env),
        Action::SwitchPage(p) => {
            CURRENT_PAGE.with(|c| c.set(p));
            rebuild(env);
        }
        Action::AddVipGold(amount) => {
            let gd = game_singleton(env, "GameData", "sharedInstance");
            if gd == nil {
                return;
            }
            let add = sel(env, "addVipGoldForBuy:UIUpdate:");
            let do_update: bool = true;
            let _: () = msg_send(env, (gd, add, amount, do_update));
            log!("[MOLEMENU] +{} vip gold", amount);
        }
        Action::GhostTL(selector, repeat) => ghost_call(env, selector, repeat),
        Action::SummonClass(name, z) => summon_class(env, name, z),
        Action::MiniGame(id_) => mini_game(env, id_),
        Action::GameDataReset(selector) => {
            let gd = game_singleton(env, "GameData", "sharedInstance");
            if gd == nil {
                return;
            }
            let s = sel(env, selector);
            let _: () = msg_send(env, (gd, s));
            let save = sel(env, "saveUserInfoData");
            let _: () = msg_send(env, (gd, save));
            log!("[MOLEMENU] GameData {}", selector);
        }
        Action::GameManagerCall(selector) => {
            let gm = game_singleton(env, "GameManager", "sharedManager");
            if gm == nil {
                log!("[MOLEMENU] GameManager sharedManager == nil");
                return;
            }
            let s = sel(env, selector);
            let _: () = msg_send(env, (gm, s));
            log!("[MOLEMENU] GameManager {}", selector);
        }
        Action::SingletonCall(class, shared, method) => {
            let obj = game_singleton(env, class, shared);
            if obj == nil {
                log!("[MOLEMENU] {} {} == nil", class, shared);
                return;
            }
            let s = sel(env, method);
            let _: () = msg_send(env, (obj, s));
            log!("[MOLEMENU] {} {}", class, method);
        }
        Action::CloseSummoned => {
            let layer = LAST_SUMMONED.with(|c| c.get());
            if layer == nil {
                log!("[MOLEMENU] 没有可关闭的召唤层");
                return;
            }
            let s = sel(env, "removeFromParentAndCleanup:");
            let cleanup: bool = true;
            let _: () = msg_send(env, (layer, s, cleanup));
            release(env, layer); // 配对 summon 时的 retain
            LAST_SUMMONED.with(|c| c.set(nil));
            log!("[MOLEMENU] 已关闭召唤层");
        }
        Action::ResetLocalSave => {
            // 删本地存档文件(home/Documents 下);先取 owned 路径再 remove(避免借用冲突)。
            let docs = env.fs.home_directory().join("Documents");
            let mut n = 0;
            for f in ["userinfo.dat", "map.dat"] {
                let p = docs.join(f);
                if env.fs.remove(&p).is_ok() {
                    n += 1;
                }
            }
            log!(
                "[MOLEMENU] 已删本地存档 {} 个文件;建议立即退出游戏,下次启动即为全新存档",
                n
            );
        }
        Action::UserInfoSet(selector, val) => {
            let ui = user_info_data(env);
            if ui == nil {
                return;
            }
            let s = sel(env, selector);
            let _: () = msg_send(env, (ui, s, val));
            save_user_info(env);
            log!("[MOLEMENU] UserInfoData {} {}", selector, val);
        }
        Action::SetWorkers(n) => {
            let ui = user_info_data(env);
            if ui == nil {
                return;
            }
            let s1 = sel(env, "setTotalWorkers:");
            let _: () = msg_send(env, (ui, s1, n));
            let s2 = sel(env, "setAvailableWorkers:");
            let _: () = msg_send(env, (ui, s2, n));
            save_user_info(env);
            log!("[MOLEMENU] workers = {}", n);
        }
        Action::EnterXiaoTuLv(map_res, ui_res) => enter_xiaotulv(env, map_res, ui_res),
        Action::ToggleCheat(key) => {
            crate::mole_cheats::toggle(key);
            // Rebuild so the on/off label refreshes immediately.
            rebuild(env);
        }
        Action::VipLevelInc => {
            crate::mole_cheats::bump_vip_level();
            rebuild(env);
        }
        Action::LevelInc => {
            crate::mole_cheats::bump_level();
            rebuild(env);
        }
        Action::SetAvatar(id_) => {
            let ui = user_info_data(env);
            if ui == nil {
                return;
            }
            let s1 = sel(env, "setAvatarIcon:");
            let _: () = msg_send(env, (ui, s1, id_));
            let s2 = sel(env, "setIconIndex:");
            let _: () = msg_send(env, (ui, s2, id_));
            save_user_info(env);
            log!("[MOLEMENU] avatar = {}", id_);
        }
        Action::GameDataSetInt(selector, val) => {
            let gd = game_singleton(env, "GameData", "sharedInstance");
            if gd == nil {
                return;
            }
            let s = sel(env, selector);
            let _: () = msg_send(env, (gd, s, val));
            save_user_info(env);
            log!("[MOLEMENU] GameData {} {}", selector, val);
        }
        Action::HarvestAll => harvest_all(env),
        Action::OpenCaribbean => open_caribbean(env),
        Action::EnterIsland => enter_island(env),
        // 滑块(只读显示)在 handle_touch 里直接处理;此处占位满足穷尽匹配。
        Action::Slider(_) => {}
        // 拷贝丝尔特家园:加载 xiaotulv 地图 → saveMapData 落盘 → 重载场景(二次确认在 handle_touch)。
        Action::CopyXiaoTuLvHome => {
            let gd = game_singleton(env, "GameData", "sharedInstance");
            if gd == nil {
                return;
            }
            let lm = sel(env, "loadMapdataFromResource:");
            let map_str = from_rust_string(env, "xiaotulv_map".to_string());
            let _: () = msg_send(env, (gd, lm, map_str));
            let save = sel(env, "saveMapData");
            let _: () = msg_send(env, (gd, save));
            let ngm = game_singleton(env, "NewGameManager", "sharedManager");
            if ngm != nil {
                let r = sel(env, "reloadMapFromNewSceneData");
                let _: () = msg_send(env, (ngm, r));
            }
            log!("[MOLEMENU] 已拷贝丝尔特家园地图到玩家存档(saveMapData)");
        }
    }
}

/// 读某滑块种类的(显示名, 当前值, 上限)。当前值实时读游戏 UserInfoData。
fn slider_info(env: &mut Environment, kind: SliderKind) -> (&'static str, i64, i64) {
    let (name, max): (&'static str, i64) = match kind {
        SliderKind::Level => ("等级", 52),
        SliderKind::Gold => ("摩尔豆", 9_999_999),
        SliderKind::VipGold => ("贝壳", 2_000_000),
        SliderKind::Workers => ("工人", 99),
        SliderKind::Rooms => ("房间", 99),
    };
    let ui = user_info_data(env);
    if ui == nil {
        return (name, 0, max);
    }
    let getter = match kind {
        SliderKind::Level => "curLevel",
        SliderKind::Gold => "gold",
        SliderKind::VipGold => "vipGold",
        SliderKind::Workers => "totalWorkers",
        SliderKind::Rooms => "totalRooms",
    };
    let s = sel(env, getter);
    let cur: i32 = msg_send(env, (ui, s));
    (name, cur as i64, max)
}

/// 把某滑块种类的值设到绝对值(写 UserInfoData + 落盘)。
#[allow(dead_code)] // 滑块已改为只读实时值显示;此 setter 暂保留备用
fn slider_set(env: &mut Environment, kind: SliderKind, value: i64) {
    let ui = user_info_data(env);
    if ui == nil {
        return;
    }
    let v = value as i32;
    match kind {
        SliderKind::Level => {
            let s = sel(env, "setCurLevel:");
            let _: () = msg_send(env, (ui, s, v));
        }
        SliderKind::Gold => {
            let s = sel(env, "setGold:");
            let _: () = msg_send(env, (ui, s, v));
        }
        SliderKind::VipGold => {
            // 贝壳密文存储无简单 setter:用 addVipGoldForBuy:(目标-当前) 设到绝对值。
            let g = sel(env, "vipGold");
            let cur: i32 = msg_send(env, (ui, g));
            let add = sel(env, "addVipGoldForBuy:UIUpdate:");
            let upd: bool = true;
            let _: () = msg_send(env, (ui, add, v - cur, upd));
        }
        SliderKind::Workers => {
            let s1 = sel(env, "setTotalWorkers:");
            let _: () = msg_send(env, (ui, s1, v));
            let s2 = sel(env, "setAvailableWorkers:");
            let _: () = msg_send(env, (ui, s2, v));
        }
        SliderKind::Rooms => {
            let s = sel(env, "setTotalRooms:");
            let _: () = msg_send(env, (ui, s, v));
        }
    }
    save_user_info(env);
}

/// 在一个格子里渲染点击式滑块:深色轨道 + 亮色填充(宽=当前/上限) + 实时值文字。
fn render_slider(env: &mut Environment, container: id, frame: CGRect, kind: SliderKind, white: id) {
    let (name, cur, max) = slider_info(env, kind);
    let frac = if max > 0 {
        (cur as f32 / max as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let track_bg = color(env, 0.24, 0.26, 0.34, 1.0);
    add_label(env, container, frame, "", track_bg, white);
    if frac > 0.0 {
        let fill = CGRect {
            origin: frame.origin,
            size: CGSize {
                width: frame.size.width * frac,
                height: frame.size.height,
            },
        };
        let fill_bg = color(env, 0.2, 0.6, 0.42, 1.0);
        add_label(env, container, fill, "", fill_bg, white);
    }
    let clear = color(env, 0.0, 0.0, 0.0, 0.0);
    let txt = format!("{} {}/{} (只读)", name, cur, max);
    add_label(env, container, frame, &txt, clear, white);
}

fn user_info_data(env: &mut Environment) -> id {
    let gd = game_singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return nil;
    }
    let s = sel(env, "userInfoData");
    msg_send(env, (gd, s))
}

fn save_user_info(env: &mut Environment) {
    let gd = game_singleton(env, "GameData", "sharedInstance");
    if gd != nil {
        let save = sel(env, "saveUserInfoData");
        let _: () = msg_send(env, (gd, save));
    }
}

fn ghost_test_layer(env: &mut Environment) -> id {
    let existing = GHOST.with(|g| g.get());
    if existing != nil {
        return existing;
    }
    let cls = env.objc.get_known_class("TestLayer", &mut env.mem);
    if cls == nil {
        log!("[MOLEMENU] TestLayer class not found");
        return nil;
    }
    let alloc = sel(env, "alloc");
    let obj: id = msg_send(env, (cls, alloc));
    let init = sel(env, "init");
    let obj: id = msg_send(env, (obj, init));
    if obj != nil {
        retain(env, obj);
        GHOST.with(|g| g.set(obj));
    }
    obj
}

fn ghost_call(env: &mut Environment, selector: &str, repeat: u32) {
    let ghost = ghost_test_layer(env);
    if ghost == nil {
        return;
    }
    let s = sel(env, selector);
    for _ in 0..repeat {
        let _: () = msg_send(env, (ghost, s, nil));
    }
    log!("[MOLEMENU] ghost TestLayer {} x{}", selector, repeat);
}

fn summon_class(env: &mut Environment, name: &str, z: i32) {
    let cls = env.objc.get_known_class(name, &mut env.mem);
    if cls == nil {
        log!("[MOLEMENU] class {} not found", name);
        return;
    }
    let alloc = sel(env, "alloc");
    let obj: id = msg_send(env, (cls, alloc));
    let init = sel(env, "init");
    let obj: id = msg_send(env, (obj, init));
    if obj == nil {
        log!("[MOLEMENU] {} alloc/init failed", name);
        return;
    }
    let director = game_singleton(env, "CCDirector", "sharedDirector");
    if director == nil {
        return;
    }
    let rs = sel(env, "runningScene");
    let scene: id = msg_send(env, (director, rs));
    if scene == nil {
        return;
    }
    let add = sel(env, "addChild:z:");
    let _: () = msg_send(env, (scene, add, obj, z));
    // 追踪最近召唤层供「关闭召唤层」用:retain 持有防被回收;替换旧的先 release(避免泄漏)。
    let prev = LAST_SUMMONED.with(|c| c.get());
    if prev != nil {
        release(env, prev);
    }
    retain(env, obj);
    LAST_SUMMONED.with(|c| c.set(obj));
    log!("[MOLEMENU] summoned {} z={}", name, z);
}

fn mini_game(env: &mut Environment, id_: i32) {
    let mgr = game_singleton(env, "MiniGameManager", "shareInstance");
    if mgr == nil {
        log!("[MOLEMENU] MiniGameManager == nil");
        return;
    }
    let s = sel(env, "startMiniGame:playType:callbackTarget:select:");
    let play_type: i32 = 0;
    let target: id = nil;
    let select: u32 = 0; // NULL SEL
    let _: () = msg_send(env, (mgr, s, id_, play_type, target, select));
    log!("[MOLEMENU] startMiniGame {}", id_);
}

fn enter_xiaotulv(env: &mut Environment, map_res: &str, ui_res: &str) {
    let gd = game_singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return;
    }
    let lm = sel(env, "loadMapdataFromResource:");
    let map_str = from_rust_string(env, map_res.to_string());
    let _: () = msg_send(env, (gd, lm, map_str));
    let lu = sel(env, "loadUserInfoFromResource:");
    let ui_str = from_rust_string(env, ui_res.to_string());
    let _: () = msg_send(env, (gd, lu, ui_str));
    // Reload the scene from the freshly loaded data.
    let ngm = game_singleton(env, "NewGameManager", "sharedManager");
    if ngm != nil {
        let r = sel(env, "reloadMapFromNewSceneData");
        let _: () = msg_send(env, (ngm, r));
    }
    log!("[MOLEMENU] enter xiaotulv {} / {}", map_res, ui_res);
}

/// Mature + harvest every farm. `ObjectManager.farms` is the game's own farm
/// collection (cleaner than the tweak's injected gFarmTable). Each Farm gets
/// `cropMatureHandler` (the matured-crop event → reward). Safe if `farms` is
/// nil or not index-able (objectAtIndex: just no-ops to nil → skipped).
fn harvest_all(env: &mut Environment) {
    let om = game_singleton(env, "ObjectManager", "sharedManager");
    if om == nil {
        log!("[MOLEMENU] ObjectManager == nil");
        return;
    }
    let farms_sel = sel(env, "farms");
    let farms: id = msg_send(env, (om, farms_sel));
    if farms == nil {
        log!("[MOLEMENU] ObjectManager.farms == nil");
        return;
    }
    let count_sel = sel(env, "count");
    let count: u32 = msg_send(env, (farms, count_sel));
    let obj_at = sel(env, "objectAtIndex:");
    let handler = sel(env, "cropMatureHandler");
    let mut done = 0u32;
    for i in 0..count {
        let farm: id = msg_send(env, (farms, obj_at, i));
        if farm != nil {
            let _: () = msg_send(env, (farm, handler));
            done += 1;
        }
    }
    save_user_info(env);
    log!("[MOLEMENU] harvested {} / {} farms", done, count);
}

/// Open the Golden Island (加勒比寻宝) activity offline. Directly summoning the
/// layer (alloc/init/addChild) does NOT trigger its display — it's data-driven
/// and normally waits for a server callback that never arrives offline. So we:
/// (1) turn the fix on + build/store the local CaribbeanDiscoveringData so the
/// `caribbeanData` getter and the ivar both return valid data; (2) create the
/// layer and add it to the scene; (3) force `displayUI` to build the UI now.
fn open_caribbean(env: &mut Environment) {
    crate::mole_cheats::enable_golden_island();
    // Build the local data and store it in GameData (covers both getter-based
    // and direct-ivar readers in displayUI).
    let data = crate::mole_cheats::build_caribbean_data(env);
    let gd = game_singleton(env, "GameData", "sharedInstance");
    if gd != nil && data != nil {
        let set = sel(env, "setCaribbeanData:");
        let _: () = msg_send(env, (gd, set, data));
    }
    let cls = env.objc.get_known_class("CaribbeanMainLayer", &mut env.mem);
    if cls == nil {
        log!("[MOLEMENU] CaribbeanMainLayer not found");
        return;
    }
    let alloc = sel(env, "alloc");
    let obj: id = msg_send(env, (cls, alloc));
    let init = sel(env, "init");
    let layer: id = msg_send(env, (obj, init));
    if layer == nil {
        log!("[MOLEMENU] CaribbeanMainLayer init failed");
        return;
    }
    // Parent it first (showLayerWithTarget does NOT addChild — in-game the
    // ActionCenterLayer does that) so displayUI's content actually renders.
    let director = game_singleton(env, "CCDirector", "sharedDirector");
    if director != nil {
        let rs = sel(env, "runningScene");
        let scene: id = msg_send(env, (director, rs));
        if scene != nil {
            let add = sel(env, "addChild:z:");
            let z: i32 = 88888;
            let _: () = msg_send(env, (scene, add, layer, z));
        }
    }
    // Register THIS layer as the Caribbean network delegate so the offline
    // getCaribbeanStateInfo: hook drives displayUI on it.
    let nm = game_singleton(env, "NetworkManager", "sharedInstance");
    if nm != nil
        && env
            .objc
            .object_has_method_named(&env.mem, nm, "setDelegateCaribbeanActivity:")
    {
        let sd = sel(env, "setDelegateCaribbeanActivity:");
        let _: () = msg_send(env, (nm, sd, layer));
    }
    // Present it: checkNetWork (hooked -> YES) -> showLoadingLayer ->
    // getCaribbeanStateInfo: (hooked -> build data, hide the loading modal,
    // drive displayUI). Fall back to building + displayUI directly otherwise.
    if env
        .objc
        .object_has_method_named(&env.mem, layer, "showLayerWithTarget:selector:")
    {
        let show = sel(env, "showLayerWithTarget:selector:");
        let _: () = msg_send(env, (layer, show, nil, 0u32));
    } else {
        let data = crate::mole_cheats::build_caribbean_data(env);
        if gd != nil && data != nil {
            let set = sel(env, "setCaribbeanData:");
            let _: () = msg_send(env, (gd, set, data));
        }
        if env.objc.object_has_method_named(&env.mem, layer, "displayUI") {
            let d = sel(env, "displayUI");
            let _: () = msg_send(env, (layer, d));
        }
    }
    let children: id = {
        let cs = sel(env, "children");
        msg_send(env, (layer, cs))
    };
    let child_count: u32 = if children == nil {
        0
    } else {
        let cc = sel(env, "count");
        msg_send(env, (children, cc))
    };
    // Is that child a populated container (content built, just not visible) or
    // empty (displayUI gated most content out)?
    let grandkids: u32 = if child_count > 0 {
        let obj_at = sel(env, "objectAtIndex:");
        let first: id = msg_send(env, (children, obj_at, 0u32));
        if first == nil {
            0
        } else {
            let gcs = sel(env, "children");
            let gc: id = msg_send(env, (first, gcs));
            if gc == nil {
                0
            } else {
                let cc = sel(env, "count");
                msg_send(env, (gc, cc))
            }
        }
    } else {
        0
    };
    log!(
        "[MOLEMENU] opened caribbean (children={} grandchildren={})",
        child_count,
        grandkids
    );
    // Close the menu so the activity is visible.
    teardown(env);
}

/// 一键进入 NewScene 可建筑黄金岛(scene id 10)。arm 进岛(开功能/开窗/预注入默认岛
/// mapData)后直接 `[SceneMannager startNewSceneFrom:1 toScene:10]`;网络门与 state2
/// 数据门由 mole_cheats 的 intercept 在进岛窗口内放行。跳过飞机过场(热点路径仍带)。
fn enter_island(env: &mut Environment) {
    crate::mole_cheats::island_arm_entry(); // 开启功能,余下交给自然链 + intercept hook
    // 走游戏自然进岛链:取主村层 [[WrapperManager sharedManager] currentVillageLayer] 后
    // [village enterNewIslands] —— 由 mole_cheats 的 intercept 接管网络门/异步 SUCC/mapData
    // 注入/解加载活锁。绝不直调 startNewSceneFrom(绕过前置 → 网络门 bail → 上不了岛)。
    let wm = game_singleton(env, "WrapperManager", "sharedManager");
    let village: id = if wm != nil {
        let s = sel(env, "currentVillageLayer");
        msg_send(env, (wm, s))
    } else {
        nil
    };
    if village != nil && env.objc.object_has_method_named(&env.mem, village, "enterNewIslands") {
        let s = sel(env, "enterNewIslands");
        let _: () = msg_send(env, (village, s));
        log!("[MOLEMENU] enter island -> [village enterNewIslands]");
    } else {
        log!("[MOLEMENU] currentVillageLayer/enterNewIslands unavailable (need to be in main village)");
    }
    teardown(env);
}
