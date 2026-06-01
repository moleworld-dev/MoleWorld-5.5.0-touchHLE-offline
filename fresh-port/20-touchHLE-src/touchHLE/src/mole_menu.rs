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

/// What a button does when tapped. Each variant maps to a real game call (or a
/// menu-internal action like switching page).
#[derive(Clone, Copy)]
pub enum Action {
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
}

struct Button {
    frame: CGRect,
    action: Action,
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
                ("经验 +1", GhostTL("onButtonXPPlus:", 1)),
                ("经验 +10", GhostTL("onButtonXPPlus:", 10)),
                ("经验 -1", GhostTL("onButtonXPMinus:", 1)),
                ("摩尔豆 +1", GhostTL("onButtonGoldPlus:", 1)),
                ("摩尔豆 +10", GhostTL("onButtonGoldPlus:", 10)),
                ("摩尔豆 -1", GhostTL("onButtonGoldMinus:", 1)),
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
        Page {
            title: "召唤NPC",
            buttons: vec![
                ("TestLayer 调试层", SummonClass("TestLayer", 99999)),
                ("超级贝壳树", SummonClass("SuperShellTree", 88888)),
                ("免费贝壳墙", SummonClass("ShowFreeShellsLayer", 88888)),
                ("水塔", SummonClass("WaterTower", 88888)),
                ("乌鸦祭司", SummonClass("CrowPriest", 88888)),
                ("圣诞树", SummonClass("ChrismasTreeView", 88888)),
                ("圣诞主活动(弃用)", SummonClass("XmasMainLayer", 88888)),
                ("彩蛋主面板(弃用)", SummonClass("EasterEggMainLayer", 88888)),
                ("村庄菜单层", SummonClass("VillageMenuLayer", 88888)),
                ("周年纪念(弃用)", SummonClass("AnniversaryMainLayer", 88888)),
                ("秋季活动(弃用)", SummonClass("AutumnMainLayer", 88888)),
                ("万圣节主活动(弃用)", SummonClass("HalloweenMainLayer", 88888)),
                ("Naram春活(弃用)", SummonClass("NaramSpringMainLayer", 88888)),
                ("新版商店", SummonClass("NewStyleStoreMainLayer", 88888)),
                ("促销主层", SummonClass("PromoteSalesMainLayer", 88888)),
                ("广告墙板(弃用)", SummonClass("ShowAdwallBoardLayer", 88888)),
                ("更多好友(弃用)", SummonClass("ShowMoreFriendsLayer", 88888)),
                ("活动规则层(弃用)", SummonClass("ShowActivityRuleLayer", 88888)),
            ],
        },
        // [离线弃用] All of these are server-driven activity layers; offline they
        // can't fetch their state/art so they won't render (some may freeze, like
        // Caribbean). Kept as summon-to-inspect tools but labelled (弃用).
        Page {
            title: "召唤活动",
            buttons: vec![
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
            ],
        },
        Page {
            title: "Mini/任务/重置",
            buttons: vec![
                ("Mini: 切水果", MiniGame(1)),
                ("Mini: 钓鱼", MiniGame(2)),
                ("Mini: 占卜(弃用)", MiniGame(3)),
                ("Mini: 挖矿", MiniGame(4)),
                ("Mini: 涂鸦", MiniGame(5)),
                ("Mini: 洗澡", MiniGame(6)),
                ("丝尔特(春)", EnterXiaoTuLv("xiaotulv_map", "xiaotulv_userinfo")),
                ("丝尔特(冬)", EnterXiaoTuLv("xiaotulv_winter_map", "xiaotulv_winter_userinfo")),
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
        Page {
            title: "开关",
            buttons: vec![
                ("购物免费", ToggleCheat("free_shop")),
                ("金币 x10", ToggleCheat("gold_x10")),
                ("经验 x10", ToggleCheat("xp_x10")),
                ("强制VIP", ToggleCheat("force_vip")),
                ("VIP等级", VipLevelInc),
                ("关反作弊检测", ToggleCheat("kill_anticheat")),
                ("作物瞬熟", ToggleCheat("instant_crop")),
                ("永不枯萎", ToggleCheat("no_wither")),
                ("冷却归零", ToggleCheat("no_cooldown")),
                ("建筑瞬完成", ToggleCheat("instant_build")),
            ],
        },
        Page {
            title: "解锁/成就/收获",
            buttons: vec![
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
        Page {
            title: "黄金岛",
            buttons: vec![
                ("修复黄金岛", ToggleCheat("fix_golden_island")),
                ("直达终点(弃用)", ToggleCheat("golden_win")),
                ("打开加勒比黄金岛(弃用)", OpenCaribbean),
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
        buttons.push(Button { frame, action });
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
            Action::GameDataReset(_) | Action::GameManagerCall(_) => {
                (label.to_string(), color(env, 0.55, 0.4, 0.18, 1.0))
            }
            Action::EnterXiaoTuLv(..) => (label.to_string(), color(env, 0.2, 0.5, 0.55, 1.0)),
            _ => (label.to_string(), color(env, 0.16, 0.45, 0.7, 1.0)),
        };
        add_label(env, container, frame, &display, bg, white);
        buttons.push(Button {
            frame,
            action: *action,
        });
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
    log!("[MOLEMENU] closed");
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
            .map(|b| b.action)
    });
    if let Some(action) = hit {
        run_action(env, action);
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
    }
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
