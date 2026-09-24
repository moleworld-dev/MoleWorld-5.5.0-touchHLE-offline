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
//!
//! [扫描修 2026-09-15] 新增「开发工具」(数值寄存器键盘 + mole_dev 工具)、「隐藏物品」(mole_items)两页,
//! 以及挂在召唤页下的「旧活动观赏」子页;「开发者 / 调试」页签固定在最右,页内按钮位置不变(无头测试按坐标点)。

use crate::frameworks::core_graphics::cg_affine_transform::CGAffineTransform;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::from_rust_string;
use crate::frameworks::foundation::NSInteger;
use crate::frameworks::uikit::ui_font::{UILineBreakMode, UILineBreakModeCharacterWrap};
use crate::mem::{MutVoidPtr, Ptr};
use crate::mole_dev::QuestFamily;
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
    /// 删本地存档文件(主档/岛档/vip.dat/mole_activity.dat)后立即退出进程 → 重开即为全新存档。
    /// [复核修 2026-09-15] 删完不退出的话,关窗时游戏会把内存里的旧档写回,详见 run_action。
    /// [2026-09-16] X4-01 有文件删不掉时已删的原样写回、不退出,只在 toast 里列出删不掉的文件。
    ResetLocalSave,
    /// [2026-09-16] G-02/G-06 只在底部 toast 显示说明、不做任何游戏调用:未实现或离线不可用的入口
    /// (丝尔特三键、超级贝壳树、广告墙板)。按钮删不删交用户拍板,先止损不再报假成功。
    Notice(&'static str),
    /// [2026-09-16] G-06 按物品 ID 走 mole_items::place_item 摆到当前地图(召唤页「水塔」「乌鸦祭司」)。
    PlaceItem(u32),
    /// `[[GameData userInfoData] <sel>:val]` then save.
    UserInfoSet(&'static str, i32),
    /// Set total + available workers, then save.
    SetWorkers(i32),
    /// Flip a toggle-style cheat (see `mole_cheats`) by its key.
    ToggleCheat(&'static str),
    /// 强制 VIP 等级在 1..=VIP_LEVEL_MAX(当前 4)间循环,并顺带打开 force_vip(标签与 toast 会说明,见 G-11)。
    VipLevelInc,
    /// Cycle the forced player level (0/10/.../100) — overrides curLevel.
    LevelInc,
    /// Set the avatar icon: `setAvatarIcon:` + `setIconIndex:`, then save.
    SetAvatar(i32),
    /// `[[GameData sharedInstance] <sel>:val]` then save (e.g. setRewardTickets:).
    GameDataSetInt(&'static str, i32),
    /// 一键收获全部:已成熟的地块走原版 -[WrapperManager harvestOnekey:] 真收获,生长中的只催熟,空地/枯萎地跳过,岛会话中拒绝。
    /// [2026-09-16] G-03 以前对每块地发 cropMatureHandler,只挂收获旗、不收获,详见 harvest_all。
    HarvestAll,
    /// Open the Golden Island (Caribbean) activity offline: enable the fix,
    /// build+set its data, create the layer and force `displayUI`.
    /// [扫描修 2026-09-15] 这是加勒比寻宝活动(≠可建筑黄金岛),菜单入口已移到「旧活动观赏」子页。
    OpenCaribbean,
    /// 一键进入 NewScene 可建筑黄金岛(scene id 10):arm 进岛(开功能/开窗/注入默认岛
    /// mapData)后直接 `[SceneMannager startNewSceneFrom:1 toScene:10]`。
    EnterIsland,
    /// 岛上一键回主村:调原版 `-[HolidayVillageLayer gobackMainVillage]`@0x23d15c(与点岛上飞机后确认框的回调
    /// 同一路径,0x23d458 把它作为 showWithTarget:selector: 的回调选择子)。
    /// [2026-09-24 第四轮 K14 N-D5-3] 不走 returnToMainVillage(断网专用,会 setConnectFirstInThisOpen:1);
    /// 退岛存盘在 mole_cheats 的 startNewSceneFrom 10→1 出口,不在 gobackMainVillage 上。
    ExitIsland,
    /// [2026-09-24 第四轮 K14 I4-04] 黄金岛探险船 GM:一键修好,走原版贝壳加速修船的同一出口 -[DiscoveryShip quickFixShip]。
    /// 仅离线且在岛上可用,见 ship_quick_fix。
    ShipQuickFix,
    /// [2026-09-24 第四轮 K14 I4-04] 黄金岛探险船 GM:立即返航,把 beginDiscoverTime_ 拨到出海时长之前,
    /// 由原版 innerUpdate: 自己结算返航与礼物。仅离线且在岛上可用,见 ship_return_now。
    ShipReturnNow,
    /// [扫描修 2026-09-15] 开发工具按钮:调用 mole_dev 的约定函数,DevResult 文案写底部 toast。
    Dev(DevTool),
    /// [扫描修 2026-09-15] 隐藏物品页按钮:调用 mole_items 的约定函数。
    Hidden(HiddenAct),
}

/// [扫描修 2026-09-15] 开发工具页的按钮种类(F7-2)。需要数值的工具统一读 mole_dev 的数值寄存器
/// (菜单数字键盘输入);菜单只调用 mole_dev 的约定函数,不碰它的内部状态。
#[derive(Clone, Copy)]
pub enum DevTool {
    /// 只显示寄存器当前值,点击无动作。
    RegShow,
    RegDigit(u8),
    RegBackspace,
    RegNegate,
    RegClear,
    /// 占位空格:不渲染、不可点,用来让键盘区对齐。
    Spacer,
    /// 对象计时快进 N 分钟(原版 TestLayer updateTime 语义,写绝对值不叠加)。
    TimeMinutes(i64),
    /// 任务链跳到寄存器里的任务号。
    Quest(QuestFamily),
    /// 播放寄存器里的剧情段。
    Story,
    UnlockInteraction,
    /// 天气种类 = 寄存器值。
    Weather,
    TimeScale(f32),
    Fps,
    MapGrid,
    /// 全局时钟前拨 N 小时(不可回退,二次确认)。
    TimeTravelHours(i64),
    SnapshotSave,
    /// 下次启动用快照覆盖存档(二次确认)。
    SnapshotRestore,
    /// 原版单例入口打开建设商店(替代会卡的「新版商店」召唤,F9-2)。
    BuildingStore,
    CameraCenter,
    Trace,
    /// [2026-09-24 第四轮 K4 I4-05] 岛档计时快进:分钟 = 寄存器值,主村离线执行、下次进岛生效(mole_dev::island_fast_forward_minutes)。
    IslandFastForward,
}

/// [扫描修 2026-09-15] 隐藏物品页的按钮种类(F1-1 / F1-5 / F4-1)。
#[derive(Clone, Copy)]
pub enum HiddenAct {
    ShopToggle,
    FestivalCycle,
    /// 切换点条目时的去向:放到地图 / 入仓库。
    ModeToggle,
    PrevPage,
    NextPage,
    PageInfo,
    /// 当前目录页上的第 N 格。
    Item(usize),
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
    // [扫描修 2026-09-15] toast 版本号:动作自己写了 toast 时,handle_touch 不再用「已执行」覆盖。
    static TOAST_GEN: Cell<u64> = const { Cell::new(0) };
    // [扫描修 2026-09-15] 开发工具不可回退动作(时间旅行/快照恢复)的二次确认待定态:0=无,其余=动作编码。
    static PENDING_DEV: Cell<u32> = const { Cell::new(0) };
    // [扫描修 2026-09-15] 隐藏物品目录:当前页号;点条目时 true=入仓库、false=放到地图。
    static CATALOG_PAGE: Cell<usize> = const { Cell::new(0) };
    static CATALOG_TO_STORAGE: Cell<bool> = const { Cell::new(false) };
}

/// [扫描修 2026-09-15] 页内按钮的排布方式。
#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// 旧布局:3 列,先把一列填满再换下一列(每列行数 = ceil(按钮数/3))。
    /// 「开发者 / 调试」页的无头测试坐标依赖它,勿改。
    ColumnFirst3,
    /// 新布局:cols 列,按行从左到右填(数字键盘、目录网格用)。
    RowFirst(usize),
}

struct Page {
    title: &'static str,
    buttons: Vec<(&'static str, Action)>,
    layout: Layout,
    /// 子页:不占页签,页签行高亮它的父页(如「旧活动观赏」挂在「召唤」下)。
    parent: Option<usize>,
}

// [扫描修 2026-09-15] 页下标常量。pages() 里各页的顺序必须与此一致(layout_selfcheck 会核对标题)。
/// 召唤页。
const PAGE_SUMMON: usize = 1;
/// 「开发者 / 调试」页:主控无头测试按坐标点它的页签和页内按钮,下标、按钮顺序与数量(19..=21)都别动。
const PAGE_DEV_DEBUG: usize = 4;
/// 「旧活动观赏」子页(从召唤页的入口按钮进入)。
const PAGE_OLD_ACTIVITY: usize = 7;
/// 隐藏物品目录每页条目数(3 列 × 12 行)。
const CATALOG_PER_PAGE: usize = 36;
/// [2026-09-16] G-02 丝尔特三键的说明 toast。
const XIAOTULV_TODO: &str = "丝尔特家园暂未实现(原版需切 NPC 地图)";
/// [2026-09-16] G-10 离线时「魔法密码任意过」的标签:MagicNumberView 只由 -[LoadingLayer onCommandReceived:] 收到 1018、
/// 且 [GameData magicPassword] 非空时创建(0x1313f6-0x131454),离线回环不回 1018,开关离线永远没有效果。
const MAGIC_BYPASS_OFFLINE_LABEL: &str = "魔法密码任意过(仅联机:私服回 1018 才出现)";
/// 按钮区设计宽度:页签、网格、toast 都按 1024×768 横屏设计坐标排版。
const DESIGN_W: CGFloat = 1024.0;

/// [2026-09-16] F2-04 菜单几何。原来容器写死 1024×768、center (384,512)、触摸 lx = 1024 - gy:--fill-screen 宽屏
/// (iOS 默认、桌面宽屏启动器)下 guest 逻辑屏是 768×1188 等,菜单偏到一侧,另一侧露出没有遮罩、点了没反应的竖条。
/// 现在逻辑尺寸与旋转复用系统弹框的 ui_alert_view::coord_map_and_transform(同时兼容 --landscape-left):
/// 遮罩铺满逻辑屏,按钮区仍按 1024 设计宽排版、整体水平偏移 ox 居中。Button.frame 一律存设计坐标,只在渲染时加 ox。
/// 4:3 横屏右时 logical=1024×768、ox=0、transform=(0,-1,1,0)、center=(384,512),与改动前逐字节一致。
#[derive(Clone, Copy)]
struct MenuGeometry {
    screen_w: CGFloat,
    screen_h: CGFloat,
    logical_w: CGFloat,
    logical_h: CGFloat,
    transform: CGAffineTransform,
    /// 设计宽 1024 的按钮区在逻辑屏里的水平偏移(宽屏 1188 时为 82)。
    ox: CGFloat,
}

fn menu_geometry(env: &Environment) -> MenuGeometry {
    let (map, t) = crate::frameworks::uikit::ui_view::ui_alert_view::coord_map_and_transform(env);
    // 旋转矩阵里 cos(π/2) 的浮点噪声经 round() 后可能是 -0.0;加 0.0 归一成 +0.0,
    // 让 4:3 下交给 setTransform: 的值与原来写死的 (0,-1,1,0) 逐位相同。
    let transform = CGAffineTransform {
        a: t.a + 0.0,
        b: t.b + 0.0,
        c: t.c + 0.0,
        d: t.d + 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    MenuGeometry {
        screen_w: map.screen_w,
        screen_h: map.screen_h,
        logical_w: map.logical_w,
        logical_h: map.logical_h,
        transform,
        ox: ((map.logical_w - DESIGN_W) / 2.0).max(0.0).floor(),
    }
}

/// guest 触摸点 → 设计坐标。容器视图变换是纯旋转(各项只有 0/±1),guest 偏移 = M·逻辑偏移,
/// M = [a c; b d];正交矩阵的逆是转置,所以逻辑偏移 dx = a·ux + b·uy、dy = c·ux + d·uy。
/// 4:3 横屏右:dx = -(gy-512)、dy = gx-384 → (1024 - gy, gx),与原公式一致;宽屏时再扣掉 ox。
fn guest_to_design(g: &MenuGeometry, gx: f32, gy: f32) -> (f32, f32) {
    // CGAffineTransform 是 repr(packed),先整体拷出再按值读字段,不对字段取引用。
    let t = g.transform;
    let (a, b, c, d) = (t.a, t.b, t.c, t.d);
    let ux = gx - g.screen_w / 2.0;
    let uy = gy - g.screen_h / 2.0;
    let dx = a * ux + b * uy;
    let dy = c * ux + d * uy;
    (g.logical_w / 2.0 + dx - g.ox, g.logical_h / 2.0 + dy)
}

fn pages() -> Vec<Page> {
    use Action::*;
    use DevTool as D;
    use HiddenAct as H;
    vec![
        // 0
        Page {
            title: "数值",
            layout: Layout::ColumnFirst3,
            parent: None,
            buttons: vec![
                // 点击式滑块:显示游戏实时值,点条某位置=按比例设值(等级 1-52)。配合下面 +/- 微调。
                ("等级", Slider(SliderKind::Level)),
                ("摩尔豆", Slider(SliderKind::Gold)),
                ("贝壳", Slider(SliderKind::VipGold)),
                ("工人", Slider(SliderKind::Workers)),
                ("房间", Slider(SliderKind::Rooms)),
                // [扫描修 2026-09-15] F6-1 文案按原版档值改准。onButtonXPPlus: 每次加
                // -[TestLayer getXPChangeValue]@0x145dec 的档值,按 curLevel 分档:<6→50、<11→500、<16→1500、
                // <21→1万、<41→5万、其余→50万;原文案「经验 +1/+10/+100」的量级是错的(实际是按 N 次)。
                ("经验 +档值×1(50~50万)", GhostTL("onButtonXPPlus:", 1)),
                ("经验 +档值×10", GhostTL("onButtonXPPlus:", 10)),
                ("经验 +档值×100", GhostTL("onButtonXPPlus:", 100)),
                // [扫描修 2026-09-15] 删掉「经验 -1/-10」:原版 -[TestLayer onButtonXPMinus:]@0x146668
                // 实为「账号 ID 归零」——[GameSettings setUserId:0]+saveSettings、
                // [userInfoData setUserId:0]+saveUserInfoData,不减经验。联机时会丢账号关联。
                // [扫描修 2026-09-15] getGoldChangeValue@0x145e90 恒为 1000;onButtonGoldMinus:@0x14676c
                // 余额不足 1000 时先 setGold:1000 再减 1000,即归零。
                ("摩尔豆 +1000", GhostTL("onButtonGoldPlus:", 1)),
                ("摩尔豆 +1万", GhostTL("onButtonGoldPlus:", 10)),
                ("摩尔豆 +10万", GhostTL("onButtonGoldPlus:", 100)),
                ("摩尔豆 -1000(不足归零)", GhostTL("onButtonGoldMinus:", 1)),
                ("摩尔豆 -1万(不足归零)", GhostTL("onButtonGoldMinus:", 10)),
                // [扫描修 2026-09-15] getVipGoldChangeValue@0x145e98 恒为 100(onButtonVipGoldPlus: 走 addVipGold:)。
                ("贝壳 +100", GhostTL("onButtonVipGoldPlus:", 1)),
                ("贝壳 +1000(档值×10)", GhostTL("onButtonVipGoldPlus:", 10)),
                ("贝壳 +1000(直接到账)", AddVipGold(1000)),
                ("食物 +1", GhostTL("onButtonFoodPlus:", 1)),
                ("奖励券 +1", GhostTL("onButtonTicketsPlus:", 1)),
                // [扫描修 2026-09-15] F6-1/F7-1:删掉 5 个空操作——
                //   「VIP值 +1」:onButtonVipValuePlus:@0x1469d0 只发服务器包,且 getVipValueChangeValue 恒 0,离线无效;
                //   「时间 +1」「任务进度 +1」「限时任务 +1」「VIP任务 +1」:只改幽灵 TestLayer 的 time_/questId_ 等 ivar,
                //   从不发 onButtonTimeTouched:/onButtonQuestTouched: 这类应用方法,游戏世界毫无变化。
                // 换成 mole_dev 的真实现:对象计时快进(原版 updateTime 语义,写绝对分钟数,不会重复叠加)与任务跳转
                // (任务号取「开发工具」页的数值寄存器)。
                ("对象计时快进 10分钟", Dev(D::TimeMinutes(10))),
                ("对象计时快进 1小时", Dev(D::TimeMinutes(60))),
                ("对象计时快进 1天", Dev(D::TimeMinutes(1440))),
                ("主线任务跳转", Dev(D::Quest(QuestFamily::Main))),
                ("限时任务跳转", Dev(D::Quest(QuestFamily::Time))),
                ("VIP任务跳转", Dev(D::Quest(QuestFamily::Vip))),
                // [2026-09-24 第四轮 K14 N-D2-4] 标签注明作用范围:两项都只写主村 [[GameData sharedInstance] userInfoData]。
                // 岛上工人走 NewSceneData.userInfoDataInNewScene(-[WrapperManager currentUserInfoData]@0x261828 在
                // curSceneId==10 时切过去),岛上点击在 run_action 里拒绝并提示。按钮个数与位置不变。
                ("工人数 = 20(仅主村)", SetWorkers(20)),
                ("房间数 = 20(仅主村)", UserInfoSet("setTotalRooms:", 20)),
            ],
        },
        // 1 召唤(NPC/功能层)。
        // [扫描修 2026-09-15] F10-9:原来 23 个标「弃用」的联网驱动活动层折叠进「旧活动观赏」子页(不删,离线怀旧观赏)。
        // F9-2:「新版商店(勿点!易卡)」换成原版单例入口。SummonClass 会 alloc 出一个和 +sharedInstance 并存的
        // 第二个 NewStyleStoreMainLayer(回调目标为 nil、没发顶层视图通知),这就是「易卡」的根因。
        // [2026-09-16] G-06 更正上面 09-15 的结论「只有 NewStyleStoreMainLayer 带 +sharedInstance」:当时只查了 sharedInstance
        // 这个名字,漏了 +shareInstance 命名的单例——ShowFreeShellsLayer@0x2552a8、ShowAdwallBoardLayer@0x4331c8 都是。
        // 现在 summon_class 对响应 +sharedInstance/+shareInstance/+sharedManager 的类一律拒绝召唤。
        // SuperShellTree/WaterTower/CrowPriest 是 Object 子类、没有 -init,alloc/init 落到 -[CCNode init] 只得到空节点;
        // 真构造器是 initWithTile:sprite:size:data:/initWithMapData:,原版只由 loadMapObjects:/Porter 建。
        Page {
            title: "召唤",
            layout: Layout::ColumnFirst3,
            parent: None,
            buttons: vec![
                ("超级贝壳树(主村无)", Notice("超级贝壳树:主村没有对应的物品,召唤出来只是空节点")),
                // 原版 -[ShowFreeShellsLayer open]@0x2569cc 的门:GameManager.gameMode==1;岛上场景没有 tag 0x11、主村场景没有
                // tag 0x1a 的子节点;!hasTopView;!isLogicLayersOpen。通过后 setIsInteractEnabled:NO、lockOthers、setVisible:YES。
                // 单例由 -[InGameScene init]+0x350 建好挂进场景,所以直接对单例发 open,不再 alloc 第二份。
                ("免费贝壳墙", SingletonCall("ShowFreeShellsLayer", "shareInstance", "open")),
                // property.dat:14956 乐乐水塔、14974 克劳神父,都是 type 14,描述写着需充值解锁。
                ("水塔(放到地图)", PlaceItem(14956)),
                ("乌鸦祭司(放到地图)", PlaceItem(14974)),
                ("圣诞树", SummonClass("ChrismasTreeView", 88888)),
                // 保留按钮(删不删交用户拍板),点击会被 summon_class 拒绝:它的 dealloc@0x5ed68 会 purge 商店/作物单例。
                ("村庄菜单层", SummonClass("VillageMenuLayer", 88888)),
                ("建设商店(原版单例入口)", Dev(D::BuildingStore)),
                ("促销主层", SummonClass("PromoteSalesMainLayer", 88888)),
                ("▶ 旧活动观赏(离线多为空壳)", SwitchPage(PAGE_OLD_ACTIVITY)),
            ],
        },
        // 2
        Page {
            title: "Mini/任务/重置",
            layout: Layout::ColumnFirst3,
            parent: None,
            buttons: vec![
                ("Mini: 切水果", MiniGame(1)),
                ("Mini: 拍虫子", MiniGame(2)),
                ("Mini: 挖矿石", MiniGame(3)),
                ("Mini: 敲木桩", MiniGame(4)),
                ("Mini: 钓鱼", MiniGame(5)),
                // [扫描修 2026-09-15] F9-3:-[MiniGameManager enterMiniGame:stage:]@0xf3fe8 共 8 个小游戏,补上 7 和 8。
                //   7 占卜屋:和建筑入口走同一条已打补丁的分支(依赖「修复占卜功能」,默认开);
                //   8 左左右右 = 黄金岛 18 级建筑「沙滩WC」的 WashRoomGame,图集自己加载;在主村召唤时奖励记进主村账本,
                //     不完全忠实但可接受。
                //   不加 6 涂鸦馆:图集缺失,大概率黑屏或空精灵帧。
                ("Mini: 占卜屋", MiniGame(7)),
                ("Mini: 左左右右(沙滩WC)", MiniGame(8)),
                // [2026-09-16] G-02 丝尔特三键先止损:原实现丢弃了 -[GameData loadMapdataFromResource:]@0x7e11c /
                // loadUserInfoFromResource:@0x7df8c 的返回值(两者只解档返回、不写 mapdata_),无参 saveMapData 存的是当前场景
                // 对象,reloadMapFromNewSceneData@0x24642c 在 nextSceneId_==0 时直接返回——什么都没做,「拷贝」还报成功。
                // 真做要照搬 -[FriendsVillageLayer showNPCMainVillageAgain]@0x10b3f0:loadMapdataFromResource:@"xiaotulv_map"
                // (冬季为 xiaotulv_winter_map)→ [GameManager unloadMap] → loadMapFromData:selector:mapData:forNPC:1@0x2099c,
                // 回家照 goToHomeVillage@0x108bbc;动手前须先核实 forNPC 期间不会把 NPC 地图写进玩家 map.dat。
                ("丝尔特(春)(未实现)", Notice(XIAOTULV_TODO)),
                ("丝尔特(冬)(未实现)", Notice(XIAOTULV_TODO)),
                ("拷贝丝尔特家园(未实现)", Notice(XIAOTULV_TODO)),
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
                // [2026-09-16] G-01 点击后在 handle_touch 里改道成 ResetLocalSave(二次确认 + 删全部存档 + 立即退出),
                // 不再发 resetUserGameData。按钮位置与本页按钮数不变。
                ("⚠️整库重置(删全部存档并退出)", GameDataReset("resetUserGameData")),
            ],
        },
        // 3 开关 + 解锁/成就/收获 合并(VIP等级/强制VIP/购物免费 已移到「开发者/调试」)。
        Page {
            title: "开关/解锁/成就",
            layout: Layout::ColumnFirst3,
            parent: None,
            buttons: vec![
                ("金币 x10", ToggleCheat("gold_x10")),
                ("经验 x10", ToggleCheat("xp_x10")),
                ("关反作弊检测", ToggleCheat("kill_anticheat")),
                ("作物瞬熟", ToggleCheat("instant_crop")),
                ("永不枯萎", ToggleCheat("no_wither")),
                // [2026-09-16] G-07 标签注明作用范围:冷却归零/建筑瞬完成/任务秒完成已补上黄金岛与日常、VIP 任务的同名方法;
                // 工人房间补满在岛上不做(全局拦岛上工人 getter 会把 99 写进岛档),只管主村。按钮位置与开关键名不变。
                ("冷却归零(主村+黄金岛)", ToggleCheat("no_cooldown")),
                ("建筑瞬完成(主村+黄金岛)", ToggleCheat("instant_build")),
                // [2026-09-24 第四轮 K14 N-D2-4] 配合 K13(I3-4):max_facility 删掉 totalRooms 臂、工人 getter 改按调用点白名单返 99,
                // 不再补房间,标签去掉「房间」。以前的旧逻辑已经经 encodeWithCoder: 写进 userinfo.dat 的 99 无法自动还原。开关键名不变。
                ("工人补满(仅主村)", ToggleCheat("max_facility")),
                ("产出×10(收菜)", ToggleCheat("harvest_mult")),
                ("任务秒完成免费(主村+黄金岛)", ToggleCheat("free_quest")),
                ("小游戏奖励满", ToggleCheat("minigame_reward")),
                ("海底寻宝必中稀有", ToggleCheat("seabed_best")),
                ("等级", LevelInc),
                ("全物品解锁", ToggleCheat("all_unlock")),
                // [2026-09-16] G-05 开关只让成就面板显示全亮,不再挡住真实成就判定,也不会发奖;标签照实说明。开关键名不变。
                ("成就面板全亮(仅显示,不发奖)", ToggleCheat("all_achieve")),
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
        // 4 破解功能 + 开发者/调试 + VIP + 黄金岛 + 购物免费 合并(保留"开发者/调试"名)。
        // [扫描修 2026-09-15] 本页受主控无头测试约束:第 9 项「一键进入黄金岛」、第 10 项「岛上一键回主村」必须留在
        // 第 2 列第 3、4 行(每列 7 行 ⇒ 按钮总数 19..=21,且前 11 项顺序不动)。layout_selfcheck 会在日志里核对。
        Page {
            title: "开发者 / 调试",
            layout: Layout::ColumnFirst3,
            parent: None,
            buttons: vec![
                // —— 破解功能(香草基底·默认关;开=往模拟内存写破解精确字节复刻,关=还原香草)——
                ("去越狱检测", ToggleCheat("kill_jailbreak")),
                ("修复占卜功能(默认开)", ToggleCheat("fix_divine")),
                // [扫描修 2026-09-15] F5-9 标签改准(原「节日村进入」):这组补丁落在 -[HolidayVillageLayer onEnter]@0x23938c
                // 的 isReachable/isConnected/disconnectByMultiLogin 三道门;HolidayVillageLayer 就是可建筑黄金岛的场景层,
                // 并没有独立的「节日村」。离线下与岛热点开关重复;联机时第三处能压掉岛上的挤号弹窗,所以保留。
                ("黄金岛网络门·跳过断网/挤号提示", ToggleCheat("enter_holiday")),
                // [扫描修 2026-09-15] F5-9 标签改准(原「商城免VIP等级」):补丁在 -[NewStyleStoreMainLayer purchaseCallback]
                // @0x3b228c,把 IAP 结果≠成功也当成功,和 VIP 等级无关;其唯一购买入口 onBuyVIPGold: 已被 SHELLHOOK 整段截走,
                // 基本跑不到。真正的 VIP 购买门在 getLockType4Object:,已由「强制VIP」「全物品解锁」覆盖。
                ("IAP回调强判成功(被贝壳钩子取代)", ToggleCheat("store_no_vip")),
                ("进新岛门(默认开·护黄金岛)", ToggleCheat("enter_newislands")),
                ("跳对象数据校验", ToggleCheat("skip_parse_check")),
                // —— VIP / 购物免费(从开关页移来)——
                ("强制VIP", ToggleCheat("force_vip")),
                ("VIP等级", VipLevelInc),
                ("购物免费", ToggleCheat("free_shop")),
                // —— 黄金岛(从黄金岛页移来)——
                ("▶ 一键进入黄金岛", EnterIsland),
                ("◀ 岛上一键回主村(存档)", ExitIsland),
                // [2026-09-24 第四轮 K14 I4-04] 探险船 GM 两项(仅离线且在岛上)。插在第 11、12 项,前 11 项顺序不动;
                // 本页按钮 19→21,每列仍 7 行,无头测试坐标 tap 178 519 / 220 517 仍落在进岛/回村上(layout_selfcheck 核对)。
                // 本页已到 21 个上限,再加按钮会变成每列 8 行、坐标漂移。
                ("探险船一键修好(离线GM)", ShipQuickFix),
                ("探险船立即返航(离线GM)", ShipReturnNow),
                ("可建筑黄金岛·热点开关", ToggleCheat("enable_newscene_island")),
                ("修复加勒比寻宝", ToggleCheat("fix_golden_island")),
                // [扫描修 2026-09-15] F10-9:原「直达终点(弃用)」「打开加勒比黄金岛(弃用)」移出本页——它们和「一键进入黄金岛」
                // 并列容易误点,名字又和可建筑黄金岛混淆。加勒比寻宝活动离线只能从菜单打开,所以不删,改名后放进「旧活动观赏」子页。
                // —— 调试工具 ——
                ("× 关闭召唤层(GM面板等)", CloseSummoned),
                ("GM面板 TestLayer", SummonClass("TestLayer", 99999)),
                ("黄金岛GM面板 NewSceneTestLayer", SummonClass("NewSceneTestLayer", 99999)),
                ("切到夜晚", SingletonCall("CommonEffectController", "sharedManager", "formDayToNight")),
                ("切回白天", SingletonCall("CommonEffectController", "sharedManager", "fromNightToDaybreak")),
                // [复核修 2026-09-15] 删完立即退出进程(防止游戏把内存里的旧档写回),文案同步说明。
                ("⚠️ 删本地存档并退出(重开全新)", ResetLocalSave),
            ],
        },
        // 5 [扫描修 2026-09-15] F7-2 开发工具:数值寄存器键盘 + 读寄存器的工具 + 各类开发开关。
        // 全部调用 mole_dev 的约定函数,DevResult 的 Ok/Err 文案显示在底部 toast。
        Page {
            title: "开发工具",
            layout: Layout::RowFirst(4),
            parent: None,
            buttons: vec![
                // 数字寄存器键盘:左列功能键,右三列数字。
                ("寄存器", Dev(D::RegShow)),
                ("1", Dev(D::RegDigit(1))),
                ("2", Dev(D::RegDigit(2))),
                ("3", Dev(D::RegDigit(3))),
                ("← 退格", Dev(D::RegBackspace)),
                ("4", Dev(D::RegDigit(4))),
                ("5", Dev(D::RegDigit(5))),
                ("6", Dev(D::RegDigit(6))),
                ("± 取负", Dev(D::RegNegate)),
                ("7", Dev(D::RegDigit(7))),
                ("8", Dev(D::RegDigit(8))),
                ("9", Dev(D::RegDigit(9))),
                ("清零", Dev(D::RegClear)),
                ("0", Dev(D::RegDigit(0))),
                ("", Dev(D::Spacer)),
                ("", Dev(D::Spacer)),
                // 读寄存器值的工具(标签实时显示将要用的数)
                ("主线任务跳转", Dev(D::Quest(QuestFamily::Main))),
                ("限时任务跳转", Dev(D::Quest(QuestFamily::Time))),
                ("VIP任务跳转", Dev(D::Quest(QuestFamily::Vip))),
                ("黄金岛任务跳转", Dev(D::Quest(QuestFamily::Island))),
                ("播放剧情", Dev(D::Story)),
                ("解锁交互", Dev(D::UnlockInteraction)),
                ("天气", Dev(D::Weather)),
                ("相机回中", Dev(D::CameraCenter)),
                ("倍速 ×0.5", Dev(D::TimeScale(0.5))),
                ("倍速 ×1", Dev(D::TimeScale(1.0))),
                ("倍速 ×2", Dev(D::TimeScale(2.0))),
                ("倍速 ×4", Dev(D::TimeScale(4.0))),
                ("FPS 显示(开关)", Dev(D::Fps)),
                ("地图格线(开关)", Dev(D::MapGrid)),
                ("选择子跟踪", Dev(D::Trace)),
                ("打开建设商店", Dev(D::BuildingStore)),
                ("时间旅行+1h(不可回退)", Dev(D::TimeTravelHours(1))),
                ("时间旅行+24h(不可回退)", Dev(D::TimeTravelHours(24))),
                ("存档快照:保存", Dev(D::SnapshotSave)),
                ("快照:下次启动恢复", Dev(D::SnapshotRestore)),
                // [2026-09-24 第四轮 K4 I4-05] 追加在末尾(第 11 行首格),不挪动前面任何按钮的坐标。
                ("岛档快进(分钟)", Dev(D::IslandFastForward)),
            ],
        },
        // 6 [扫描修 2026-09-15] F1-1/F1-5/F4-1 隐藏物品:进商店开关、节日商店模式、目录浏览(放到地图 / 入仓库)。
        // 全部调用 mole_items 的约定函数;目录条目格按 hidden_catalog() 分页,空格子不渲染。
        Page {
            title: "隐藏物品",
            layout: Layout::RowFirst(3),
            parent: None,
            buttons: {
                let mut b: Vec<(&'static str, Action)> = vec![
                    ("隐藏物品进商店", Hidden(H::ShopToggle)),
                    ("节日商店", Hidden(H::FestivalCycle)),
                    ("点条目", Hidden(H::ModeToggle)),
                    ("◀ 上一页", Hidden(H::PrevPage)),
                    ("目录页码", Hidden(H::PageInfo)),
                    ("下一页 ▶", Hidden(H::NextPage)),
                ];
                for slot in 0..CATALOG_PER_PAGE {
                    b.push(("", Hidden(H::Item(slot))));
                }
                b
            },
        },
        // 7 [扫描修 2026-09-15] F10-9 旧活动观赏子页(挂在召唤页下,不占页签)。
        // 这些活动层多为联网驱动,离线只能召唤出来观赏,多数是空壳;加勒比寻宝的两个入口也放在这里。
        Page {
            title: "旧活动观赏",
            layout: Layout::ColumnFirst3,
            parent: Some(PAGE_SUMMON),
            buttons: vec![
                ("◀ 返回召唤", SwitchPage(PAGE_SUMMON)),
                ("× 关闭召唤层", CloseSummoned),
                ("打开加勒比寻宝(观赏)", OpenCaribbean),
                ("加勒比寻宝·直达终点", ToggleCheat("golden_win")),
                ("圣诞主活动", SummonClass("XmasMainLayer", 88888)),
                ("彩蛋主面板", SummonClass("EasterEggMainLayer", 88888)),
                ("周年纪念", SummonClass("AnniversaryMainLayer", 88888)),
                ("秋季活动", SummonClass("AutumnMainLayer", 88888)),
                ("万圣节", SummonClass("HalloweenMainLayer", 88888)),
                ("Naram春活", SummonClass("NaramSpringMainLayer", 88888)),
                ("爱丽丝梦游", SummonClass("Activity_Alice_MainLayer", 88888)),
                ("史莱克", SummonClass("Activity_Shrek_BasePopLayer", 88888)),
                ("龙猫", SummonClass("Activity_Totoro_BasePopLayer", 88888)),
                ("冰激凌", SummonClass("Activity_IceCream_BasePopLayer", 88888)),
                ("火焰战争", SummonClass("Activity_FlameWars_MainLayer", 88888)),
                ("加勒比寻宝层(裸召唤·无数据)", SummonClass("CaribbeanMainLayer", 88888)),
                ("海底寻宝", SummonClass("SeabedSeekingTreasureMainLayer", 88888)),
                ("环游世界", SummonClass("AroundTheWorldMainLayer", 88888)),
                ("春天的诗", SummonClass("SpringPoemMainLayer", 88888)),
                ("放风筝", SummonClass("FlyKiteMainLayer", 88888)),
                ("清明青团", SummonClass("GreenRiceBallMainLayer", 88888)),
                ("开宝箱", SummonClass("OpenTreasureChestMainLayer", 88888)),
                ("世界杯竞猜", SummonClass("GuessWorldCupMainLayer", 88888)),
                ("冰夏", SummonClass("IceSummerMainLayer", 88888)),
                // [2026-09-16] G-06 ShowAdwallBoardLayer 是 +shareInstance@0x4331c8 单例,不能 alloc 第二份;而单例入口
                // -[ShowAdwallBoardLayer open]@0x43399c 又被 mole_cheats 的去广告钩子整个吞掉,离线点了也不会出现。只留说明。
                ("广告墙板(离线不可用)", Notice("广告墙板:原版展示入口 open 被去广告钩子拦截,离线不会出现")),
                ("更多好友", SummonClass("ShowMoreFriendsLayer", 88888)),
                ("活动规则层", SummonClass("ShowActivityRuleLayer", 88888)),
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
        build(env, true);
    }
}

fn color(env: &mut Environment, r: CGFloat, g: CGFloat, b: CGFloat, a: CGFloat) -> id {
    msg_class![env; UIColor colorWithRed:r green:g blue:b alpha:a]
}

fn add_label(env: &mut Environment, container: id, frame: CGRect, text: &str, bg: id, fg: id) {
    add_label_sized(env, container, frame, text, bg, fg, 0.0);
}

/// [扫描修 2026-09-15] 同 add_label,但可指定字号(font_size ≤ 0 表示用 UILabel 默认的 17 号)。
/// touchHLE 的 UILabel 单行文字不裁剪、居中后向两侧外溢,窄格子只能缩字号或缩文案。
fn add_label_sized(
    env: &mut Environment,
    container: id,
    frame: CGRect,
    text: &str,
    bg: id,
    fg: id,
    font_size: CGFloat,
) {
    let lbl: id = msg_class![env; UILabel alloc];
    let lbl: id = msg![env; lbl initWithFrame:frame];
    let t: id = from_rust_string(env, text.to_string());
    () = msg![env; lbl setText:t];
    // [扫描修 2026-09-15] setText: 内部会 copy,这里配对释放 from_rust_string 的 +1。
    // 原先每次重建菜单都漏一批字符串;数字键盘每按一次就重建一次,漏得更快。
    release(env, t);
    if font_size > 0.0 {
        let font: id = msg_class![env; UIFont systemFontOfSize:font_size];
        () = msg![env; lbl setFont:font];
    }
    () = msg![env; lbl setBackgroundColor:bg];
    () = msg![env; lbl setTextColor:fg];
    () = msg![env; lbl setTextAlignment:1i32]; // centered
    () = msg![env; container addSubview:lbl];
    release(env, lbl);
}

/// [2026-09-16] X4-02 折行 toast 的候选字号,从大到小取第一个放得下的。
const TOAST_WRAP_FONT_SIZES: [CGFloat; 3] = [15.0, 13.0, 11.0];

/// [2026-09-16] X4-02 底部 toast。一行放得下时照旧走 add_label(17 号单行,外观与改动前一致),放不下时改用多行标签。
/// 根因:touchHLE 的 UILabel 单行文字不裁剪、居中后向两侧溢出屏外(见 add_label_sized)。时间旅行确认补上活动中心说明、
/// 删档失败列出文件名之后都超过 992 宽,两头读不到。numberOfLines=0 时 UILabel drawRect 走
/// sizeWithFont:constrainedToSize:lineBreakMode: 与 drawInRect:withFont:lineBreakMode:alignment:,逐行居中;
/// 中文没有空格,按词折行(WordWrap)断不开,所以用 UILineBreakModeCharacterWrap 按字符断行。
/// 折行时框往下加高到 718..766:按钮最靠下的是「隐藏物品」页(6 个功能键 + 36 格目录,每行 3 个共 14 行),最后一行底边 715,
/// 屏幕底边 768;左右各内缩 8pt 给文字留边。三档字号都放不下时用最小一档(可能略超出框,不裁剪)。
fn add_toast(env: &mut Environment, container: id, frame: CGRect, text: &str, bg: id, fg: id) {
    let t: id = from_rust_string(env, text.to_string());
    let one_line_size: CGFloat = 17.0; // UILabel 默认字号
    let one_line_font: id = msg_class![env; UIFont systemFontOfSize:one_line_size];
    let one_line: CGSize = msg![env; t sizeWithFont:one_line_font];
    // 以原标签宽度判定:改动前能在 992 宽内放下的 toast 一律照旧单行,外观不变。
    if one_line.width <= frame.size.width {
        release(env, t);
        add_label(env, container, frame, text, bg, fg);
        return;
    }
    let inner_w = frame.size.width - 16.0;
    let wrap_frame = CGRect {
        origin: CGPoint {
            x: frame.origin.x + 8.0,
            y: frame.origin.y - 4.0,
        },
        size: CGSize {
            width: inner_w,
            height: frame.size.height + 10.0,
        },
    };
    let mode: UILineBreakMode = UILineBreakModeCharacterWrap;
    let limit = CGSize {
        width: wrap_frame.size.width,
        height: 10_000.0,
    };
    let mut font_size: CGFloat = TOAST_WRAP_FONT_SIZES[TOAST_WRAP_FONT_SIZES.len() - 1];
    for size in TOAST_WRAP_FONT_SIZES {
        let font: id = msg_class![env; UIFont systemFontOfSize:size];
        let wrapped: CGSize = msg![env; t sizeWithFont:font
                                        constrainedToSize:limit
                                            lineBreakMode:mode];
        if wrapped.height <= wrap_frame.size.height {
            font_size = size;
            break;
        }
    }
    let lbl: id = msg_class![env; UILabel alloc];
    let lbl: id = msg![env; lbl initWithFrame:wrap_frame];
    () = msg![env; lbl setText:t];
    // setText: 内部会 copy,这里配对释放 from_rust_string 的 +1(同 add_label_sized)。
    release(env, t);
    let font: id = msg_class![env; UIFont systemFontOfSize:font_size];
    () = msg![env; lbl setFont:font];
    let lines: NSInteger = 0; // 0 = 不限行数
    () = msg![env; lbl setNumberOfLines:lines];
    () = msg![env; lbl setLineBreakMode:mode];
    () = msg![env; lbl setBackgroundColor:bg];
    () = msg![env; lbl setTextColor:fg];
    () = msg![env; lbl setTextAlignment:1i32]; // centered
    () = msg![env; container addSubview:lbl];
    release(env, lbl);
}

fn build(env: &mut Environment, fade: bool) {
    let app: id = msg_class![env; UIApplication sharedApplication];
    let window: id = msg![env; app keyWindow];
    if window == nil {
        log!("[MOLEMENU] no key window yet; cannot open menu");
        return;
    }
    let all_pages = pages();
    let mut page_idx = CURRENT_PAGE.with(|c| c.get());
    if page_idx >= all_pages.len() {
        // [扫描修 2026-09-15] 防御:页表改短后残留的页号越界,回到第 0 页而不是下标 panic。
        page_idx = 0;
        CURRENT_PAGE.with(|c| c.set(0));
    }

    // [2026-09-16] F2-04 容器铺满 guest 逻辑屏(宽屏时宽于 1024),按钮区按设计坐标排版后整体右移 ox 居中。
    let geom = menu_geometry(env);
    let ox = geom.ox;
    let shifted = |r: CGRect| CGRect {
        origin: CGPoint {
            x: r.origin.x + ox,
            y: r.origin.y,
        },
        size: r.size,
    };
    let full = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: geom.logical_w,
            height: geom.logical_h,
        },
    };
    let container: id = msg_class![env; UIView alloc];
    let container: id = msg![env; container initWithFrame:full];
    let dim = color(env, 0.05, 0.05, 0.08, 0.82);
    () = msg![env; container setBackgroundColor:dim];

    let white = color(env, 1.0, 1.0, 1.0, 1.0);
    let mut buttons = Vec::new();

    // Top row: a Close button + one tab per page.
    // [扫描修 2026-09-15] 子页(parent=Some)不占页签,页签行高亮它的父页。「开发者 / 调试」固定排在最后一格:
    // 主控无头测试 tap 26 98(客户区坐标)换算成横屏逻辑坐标是 (926,26),必须落在它上面。
    // 页签宽 = (992 - (N-1)*8)/N,最后一格覆盖 [1008-宽, 1008];页签总数 N ≤ 11 时宽 ≥ 82,恒含 x=926。
    let mut tab_pages: Vec<usize> = (0..all_pages.len())
        .filter(|&p| all_pages[p].parent.is_none() && p != PAGE_DEV_DEBUG)
        .collect();
    tab_pages.push(PAGE_DEV_DEBUG);
    let highlight = all_pages[page_idx].parent.unwrap_or(page_idx);
    let tab_count = tab_pages.len() + 1;
    let tab_w = (1024.0 - 32.0 - (tab_count as f32 - 1.0) * 8.0) / tab_count as f32;
    // 页签变窄后改用 15 号字,防止中文标题溢出到相邻页签。
    let tab_font: CGFloat = if tab_w < 150.0 { 15.0 } else { 0.0 };
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
            ("× 关闭", Action::Close, color(env, 0.62, 0.18, 0.18, 1.0))
        } else {
            let p = tab_pages[i - 1];
            let selected = p == highlight;
            let bg = if selected {
                color(env, 1.0, 0.7, 0.2, 1.0)
            } else {
                color(env, 0.3, 0.32, 0.4, 1.0)
            };
            (all_pages[p].title, Action::SwitchPage(p), bg)
        };
        add_label_sized(env, container, shifted(frame), label, bg, white, tab_font);
        buttons.push(Button { frame, action, label });
    }

    // Body: current page's buttons in a grid.
    let page = &all_pages[page_idx];
    let n = page.buttons.len();
    let (left, top, hgap, bh, vgap) = (16.0f32, 64.0f32, 12.0f32, 40.0f32, 7.0f32);
    for (i, (label, action)) in page.buttons.iter().enumerate() {
        // [扫描修 2026-09-15] 占位格、以及当前目录页没有物品的条目格:不渲染、不可点。
        if matches!(action, Action::Dev(DevTool::Spacer)) {
            continue;
        }
        if let Action::Hidden(HiddenAct::Item(slot)) = action {
            if catalog_item_at(*slot).is_none() {
                continue;
            }
        }
        let (bx, by, col_w) = match page.layout {
            Layout::ColumnFirst3 => {
                // 旧布局(3 列、先填满一列再换列),几何与改动前完全一致。
                let col_w = 320.0f32;
                let per_col = n.div_ceil(3);
                let col = i / per_col;
                let row = i % per_col;
                (
                    left + col as f32 * (col_w + hgap),
                    top + row as f32 * (bh + vgap),
                    col_w,
                )
            }
            Layout::RowFirst(cols) => {
                let cols = cols.max(1);
                let col_w = (1024.0 - 2.0 * left - (cols as f32 - 1.0) * hgap) / cols as f32;
                let col = i % cols;
                let row = i / cols;
                (
                    left + col as f32 * (col_w + hgap),
                    top + row as f32 * (bh + vgap),
                    col_w,
                )
            }
        };
        let frame = CGRect {
            origin: CGPoint { x: bx, y: by },
            size: CGSize {
                width: col_w,
                height: bh,
            },
        };
        // 滑块格子:自定义渲染(轨道+填充+实时值),点击在 handle_touch 里按比例设值。
        if let Action::Slider(kind) = action {
            render_slider(env, container, shifted(frame), *kind, white);
            buttons.push(Button {
                frame,
                action: *action,
                label: *label,
            });
            continue;
        }
        // [2026-09-16] G-10 离线时魔法密码开关的标签更长,缩到 12 号字防止溢出到相邻列;其余按钮仍用默认字号。
        let magic_offline =
            matches!(action, Action::ToggleCheat("magic_bypass")) && !env.options.network_access;
        let (display, bg) = match action {
            Action::ToggleCheat(key) => {
                let on = crate::mole_cheats::is_on(key);
                let c = if on {
                    color(env, 0.2, 0.62, 0.28, 1.0)
                } else {
                    color(env, 0.45, 0.3, 0.32, 1.0)
                };
                let name: &str = if magic_offline {
                    MAGIC_BYPASS_OFFLINE_LABEL
                } else {
                    *label
                };
                (format!("{}: {}", name, if on { "开" } else { "关" }), c)
            }
            // [2026-09-16] G-11 原来恒绿、恒显示 VIP_LEVEL:强制 VIP 关着时这个等级根本不生效,颜色和数字都误导。
            Action::VipLevelInc => {
                if crate::mole_cheats::is_on("force_vip") {
                    (
                        format!("VIP等级 = VIP {} (点+)", crate::mole_cheats::vip_level()),
                        color(env, 0.2, 0.62, 0.28, 1.0),
                    )
                } else {
                    (
                        "VIP等级: 强制VIP 关".to_string(),
                        color(env, 0.45, 0.3, 0.32, 1.0),
                    )
                }
            }
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
            Action::SummonClass(..) | Action::PlaceItem(_) => {
                (label.to_string(), color(env, 0.5, 0.35, 0.65, 1.0))
            }
            Action::GameDataReset(_) | Action::GameManagerCall(_) | Action::ResetLocalSave => {
                (label.to_string(), color(env, 0.55, 0.4, 0.18, 1.0))
            }
            // [2026-09-16] 只出说明的入口用灰色,和能真正执行的按钮区分开。
            Action::Notice(_) => (label.to_string(), color(env, 0.34, 0.34, 0.36, 1.0)),
            // [扫描修 2026-09-15] 开发工具 / 隐藏物品页的动态标签(寄存器值、开关状态、目录条目)。
            Action::Dev(tool) => dev_display(env, label, *tool),
            Action::Hidden(h) => hidden_display(env, label, *h),
            _ => (label.to_string(), color(env, 0.16, 0.45, 0.7, 1.0)),
        };
        let font_size: CGFloat = if magic_offline { 12.0 } else { 0.0 };
        add_label_sized(env, container, shifted(frame), &display, bg, white, font_size);
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
        // [2026-09-16] X4-02 一行放不下的长提示(时间旅行确认、删档失败)改成折行,见 add_toast。
        add_toast(env, container, shifted(tframe), &toast, tbg, white);
    }

    layout_selfcheck(all_pages[PAGE_DEV_DEBUG].title, page_idx, &buttons);

    // Rotate + centre so the landscape layout shows upright.
    // [2026-09-16] F2-04 旋转与居中取自 menu_geometry(横屏右仍是 (0,-1,1,0) 与 (384,512))。
    let rot = geom.transform;
    () = msg![env; container setTransform:rot];
    let center = CGPoint {
        x: geom.screen_w / 2.0,
        y: geom.screen_h / 2.0,
    };
    () = msg![env; container setCenter:center];

    () = msg![env; window addSubview:container];
    retain(env, container);

    if fade {
        // Fade the menu in via the (now-restored) legacy UIView animation block.
        // Doubles as the live exercise of that code path: opacity 0 -> 1 over 0.2s.
        // [扫描修 2026-09-15] 只在打开菜单时淡入;按钮触发的重建不再淡入,否则数字键盘每按一下整个菜单都闪一次。
        () = msg![env; container setAlpha:0.0f32];
        let null_ctx: MutVoidPtr = Ptr::null();
        () = msg_class![env; UIView beginAnimations:nil context:null_ctx];
        let dur: f64 = 0.2;
        () = msg_class![env; UIView setAnimationDuration:dur];
        () = msg![env; container setAlpha:1.0f32];
        () = msg_class![env; UIView commitAnimations];
    }

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
    build(env, false);
}

fn teardown(env: &mut Environment) {
    remove_container(env);
    MENU.with(|m| m.borrow_mut().open = false);
    CURRENT_PAGE.with(|c| c.set(0));
    TOAST.with(|t| t.borrow_mut().clear());
    PENDING_RESET.with(|c| c.set(false));
    // [扫描修 2026-09-15] 关菜单时所有二次确认待定态一起清掉。
    PENDING_DEV.with(|c| c.set(0));
    log!("[MOLEMENU] closed");
}

/// 设置底部 toast 文本(下次 present/rebuild 时渲染)。
fn set_toast(s: String) {
    TOAST.with(|t| *t.borrow_mut() = s);
    // [扫描修 2026-09-15] 版本号 +1:handle_touch 据此判断动作是否自己写了反馈,避免用「已执行」覆盖。
    TOAST_GEN.with(|g| g.set(g.get().wrapping_add(1)));
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
    // [2026-09-16] F2-04 按 menu_geometry 换算并扣掉宽屏水平偏移 ox,得到与 Button.frame 相同的设计坐标。
    let geom = menu_geometry(env);
    let (lx, ly) = guest_to_design(&geom, gx, gy);
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
            let (name, cur, max, overridden) = slider_info(env, kind);
            set_toast(format!(
                "{} 当前 {}/{}(只读·用 +/- 改){}",
                name,
                cur,
                max,
                if overridden {
                    "(作弊覆盖:显示的是作弊开关给的值,存档里不是这个数)"
                } else {
                    ""
                }
            ));
            rebuild(env);
            return true;
        }
        // [2026-09-16] G-01 「⚠️整库重置」改道成与「删本地存档并退出」完全相同的二次确认 + 删档退出路径。
        // 根因:-[GameData resetUserGameData]@0x7de50 只删 map.dat(0x7dec8)/userinfo.dat(0x7df0e)、清 mapdata_、
        // 在内存里换一个空白 UserInfoData,不卸载场景也不退出;原版 -[OptionLayer onRestartYesRestart] 是先在 0x14f668
        // [GameManager unloadMap] 才到 0x14f68c 重置。菜单少了卸载,-[GameData saveMapData:] 又是从 ObjectManager
        // 的场景对象序列化(0x7690c),关窗 saveToLocal:1 就把旧庄园写回 → 等级金币清零、庄园照旧的混合档,
        // 岛档/vip.dat/mole_activity.dat 也都还在。误点一次就半毁存档,所以和删档一样要确认两次。
        let action = match action {
            Action::GameDataReset("resetUserGameData") => Action::ResetLocalSave,
            other => other,
        };
        // 二次确认类:第一次只提示,第二次才执行;点别的按钮则取消所有待确认。
        if matches!(action, Action::ResetLocalSave) {
            if !PENDING_RESET.with(|c| c.get()) {
                PENDING_RESET.with(|c| c.set(true));
                PENDING_DEV.with(|c| c.set(0));
                set_toast(format!(
                    "⚠️ 再点一次「{}」确认:删除全部本地存档后游戏立即退出,已安排的快照恢复也会一并取消",
                    label.trim_start_matches("⚠️").trim()
                ));
                rebuild(env);
                return true;
            }
            PENDING_RESET.with(|c| c.set(false)); // 已确认,下面真删
        } else if let Some((code, prompt)) = dev_confirm(action) {
            // [扫描修 2026-09-15] 开发工具里不可回退的动作(时间旅行、快照恢复)同样二次确认。
            if PENDING_DEV.with(|c| c.get()) != code {
                PENDING_DEV.with(|c| c.set(code));
                PENDING_RESET.with(|c| c.set(false));
                set_toast(prompt);
                rebuild(env);
                return true;
            }
            PENDING_DEV.with(|c| c.set(0)); // 已确认,下面真执行
        } else {
            PENDING_RESET.with(|c| c.set(false));
            PENDING_DEV.with(|c| c.set(0));
        }
        let toast_gen_before = TOAST_GEN.with(|g| g.get());
        run_action(env, action);
        let action_wrote_toast = TOAST_GEN.with(|g| g.get()) != toast_gen_before;
        // 底部 toast 反馈
        match action {
            // [扫描修 2026-09-15] 动作自己写了反馈(DevResult 文案、进岛失败原因等),不覆盖。
            _ if action_wrote_toast => {}
            Action::ToggleCheat(key) => {
                let on = crate::mole_cheats::is_on(key);
                // [2026-09-16] G-10 离线切魔法密码开关时说明它只在联机时有意义,开关本身照常翻转。
                let note = if key == "magic_bypass" && !env.options.network_access {
                    "(仅联机有效:离线不会出现魔法密码框,私服回 1018 才出现)"
                } else {
                    ""
                };
                set_toast(format!(
                    "「{}」已{}{}",
                    label,
                    if on { "开启" } else { "关闭" },
                    note
                ));
            }
            // [复核修 2026-09-15] run_action 删完就直接退出进程,正常走不到这里;留着分支免得落到「已执行」。
            // [2026-09-16] X4-01 删档失败时 run_action 自己写了失败 toast,走上面的 action_wrote_toast 分支,也到不了这里。
            Action::ResetLocalSave => set_toast("已删本地存档,正在退出游戏".to_string()),
            Action::Close | Action::SwitchPage(_) => {} // 导航不提示
            Action::EnterIsland | Action::ExitIsland => {} // 自带成功/失败提示,别覆盖
            // [扫描修 2026-09-15] 键盘输入与目录翻页:标签本身就是反馈,不刷「已执行」。
            Action::Dev(DevTool::RegShow)
            | Action::Dev(DevTool::RegDigit(_))
            | Action::Dev(DevTool::RegBackspace)
            | Action::Dev(DevTool::RegNegate)
            | Action::Dev(DevTool::RegClear)
            | Action::Dev(DevTool::Spacer)
            | Action::Hidden(HiddenAct::PrevPage)
            | Action::Hidden(HiddenAct::NextPage)
            | Action::Hidden(HiddenAct::PageInfo) => {}
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
            if selector == "resetUserGameData" {
                // [2026-09-16] G-01 兜底:handle_touch 已把它改道成 ResetLocalSave。这里绝不单独发 resetUserGameData
                // (不卸载场景、不退出,会得到半重置混合档,见 handle_touch 注释)。
                log!("[MOLEMENU] 拒绝单独发 resetUserGameData(应走删本地存档并退出)");
                return;
            }
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
            // [2026-09-16] F2-01 删档清单(主档/4 份岛档/vip.dat/mole_activity.dat 及其坏档备份)只在 save_reset.rs 维护一份;
            // 存档全部删掉之后 delete_local_saves 才撤销「下次启动恢复快照」标记,否则重开时快照被写回,删档被静默撤销。
            // 「⚠️整库重置」确认后也走这里(G-01)。
            // [2026-09-16] X4-01 有文件删不掉(chflags uchg、属主不对、Windows 上被杀毒/同步软件占着)时,delete_local_saves
            // 已把本轮删掉的文件原样写回并返回 Err。这时绝不能往下走:forget_sidecar 会禁写 vip.dat,exit(0) 后重开拿到的是
            // 「一部分旧档 + 一部分新档」的混合档。只写 toast 列出出问题的文件,游戏照常继续,处理后可以重试。
            let n = match crate::save_reset::delete_local_saves(env) {
                Ok(n) => n,
                Err(fail) => {
                    let text = reset_failure_toast(&fail);
                    log!("[MOLEMENU] 删本地存档未完成,不退出:{}", text);
                    set_toast(text);
                    return;
                }
            };
            // [复核修 2026-09-15] R5-1/R6-3 返修:删完【立即退出进程】,不走 ui_application::exit。
            // 根因:进程还活着时内存里仍是旧档——启动时 -[LoadingLayer loadResource] 在 0x12f3fa 就已 loadUserInfoData,
            // 而 loadUserInfoData@0x75704 发现文件不存在时在 0x7576e 直接跳到函数尾 0x75b3c,不清内存。
            // 正常关窗走 ui_application::exit,会先发 applicationWillResignActive:;原版在 gameMode 不为 0/6 且当前是
            // InGameScene 时于 0x1009e 调 [GameData saveToLocal:1] → 0x7cac4 saveUserInfoData、0x7cad8 saveMapData:,
            // 把旧 userinfo.dat/map.dat 写回;人在岛上时 mole_cheats 的退出落盘 island_flush 还会写回岛档。
            // 结果是旧档复活,而 vip.dat/mole_activity.dat 已删、forget_sidecar 又禁写 → 庄园/等级/金币是旧的,
            // VIP 等级与 VIP 值、连续登录、当天签到却清零(UserVIPInfoData 没有 encodeWithCoder,无从恢复)。
            // 直接 exit(0) 则失活/终止回调都不触发,saveToLocal:、side_save、island_flush 都不会写回。
            // 取舍:退出前只 synchronize 一次 NSUserDefaults,与原先正常关窗时一致,保住音量等偏好。本路径本来就不删偏好 plist;
            // 主档已删,loadUserInfoData 读不到文件就直接返回,不会去校验 isEncrypt,不会弹 HACK_USERINFO_DATA_ERROR。
            // forget_sidecar 留作兜底(万一以后去掉退出),只动宿主状态、不发消息;mole_activity 现读现写、没有内存缓存。
            crate::mole_items::forget_sidecar();
            log!(
                "[MOLEMENU] 已删本地存档 {} 个文件;立即退出进程(不让游戏把内存里的旧档写回),重开即为全新存档",
                n
            );
            let user_defaults: id = msg_class![env; NSUserDefaults standardUserDefaults];
            let _: bool = msg![env; user_defaults synchronize];
            std::process::exit(0);
        }
        Action::UserInfoSet(selector, val) => {
            // [2026-09-24 第四轮 K14 N-D2-4] 「房间数 = 20」只改主村 UserInfoData(user_info_data 固定取
            // [GameData sharedInstance].userInfoData),不在主村时拒绝,不写也不存。写了 toast,handle_touch 不会再刷「已执行」。
            if selector == "setTotalRooms:" {
                if let Some((cur, island)) = outside_main_village(env) {
                    log!(
                        "[MOLEMENU] 拒绝「房间数 = {}」:curSceneId={} 岛会话活跃={}",
                        val,
                        cur,
                        island
                    );
                    set_toast(if cur == 10 || island {
                        "房间只在主村:此按钮只改主村房间数,请回主村再用".to_string()
                    } else {
                        format!("此按钮只改主村房间数,请在主村使用(当前 curSceneId={},可能正在切换场景)", cur)
                    });
                    return;
                }
            }
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
            // [2026-09-24 第四轮 K14 N-D2-4] 岛上工人不归这个按钮管:岛上一律走 NewSceneUserInfoData
            // (-[ActorManager changeAvailableMolerForTask:] 0x9da30 岛分支读 curIdleWorkerCount/curTotalWorkersCount,
            // 抬头显示也读这两项),这里写的主村 totalWorkers/availableWorkers 要回主村后由 -[GameManager createIdleWorkers:]
            // 才生效,岛上什么都不变却会报「已执行」。也不能改成直接写岛上计数:岛上摩尔实体靠 initMoleActors:/addWorker:
            // 生成,只改数字会让计数和实体脱节。所以不在主村时拒绝,不写也不存,提示去摩尔公寓雇用。
            if let Some((cur, island)) = outside_main_village(env) {
                log!(
                    "[MOLEMENU] 拒绝「工人数 = {}」:curSceneId={} 岛会话活跃={}",
                    n,
                    cur,
                    island
                );
                set_toast(if cur == 10 || island {
                    "岛上工人请到摩尔公寓雇用;此按钮只改主村工人".to_string()
                } else {
                    format!("此按钮只改主村工人,请在主村使用(当前 curSceneId={},可能正在切换场景)", cur)
                });
                return;
            }
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
        Action::Notice(text) => {
            log!("[MOLEMENU] 说明入口:{}", text);
            set_toast(text.to_string());
        }
        Action::PlaceItem(item) => match crate::mole_items::place_item(env, item) {
            Ok(text) => {
                log!("[MOLEMENU] 召唤页放置物品 {} 成功:{}", item, text);
                set_toast(format!("{}(原版需充值解锁,放下后的交互未核实)", text));
            }
            Err(e) => {
                log!("[MOLEMENU] 召唤页放置物品 {} 失败:{}", item, e);
                set_toast(format!("放置物品 {} 失败:{}", item, e));
            }
        },
        Action::ToggleCheat(key) => {
            // [2026-09-16] G-08 离线不许关「可建筑黄金岛·热点开关」:ENABLE_NEWSCENE_ISLAND 关着时离线岛钩子
            // (网络门/解活锁/SUCC 调度)整块不跑,之后点村里飞机进岛卡死;island_arm_entry 只在菜单「一键进入黄金岛」里
            // 重新打开,飞机路径不会。开发者页按钮数须保持 19..=21,所以不删按钮,只拒绝关闭。
            if key == "enable_newscene_island"
                && !env.options.network_access
                && crate::mole_cheats::is_on(key)
            {
                log!("[MOLEMENU] 离线拒绝关闭 enable_newscene_island(离线进岛依赖它)");
                set_toast("离线进岛依赖此开关,不能关闭".to_string());
                rebuild(env);
                return;
            }
            crate::mole_cheats::toggle(key);
            // Rebuild so the on/off label refreshes immediately.
            rebuild(env);
        }
        Action::VipLevelInc => {
            // [2026-09-16] G-11 bump_vip_level 在 1..=4 间循环并顺带打开 force_vip;原来悄悄打开,这里在 toast 说清楚。
            let was_forced = crate::mole_cheats::is_on("force_vip");
            crate::mole_cheats::bump_vip_level();
            let lv = crate::mole_cheats::vip_level();
            set_toast(if was_forced {
                format!("强制VIP 等级 → VIP {}", lv)
            } else {
                format!("强制VIP 等级 → VIP {}(已同时开启强制VIP)", lv)
            });
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
        Action::ExitIsland => exit_island(env),
        // [2026-09-24 第四轮 K14 I4-04] 探险船 GM,两者都自己写 toast。
        Action::ShipQuickFix => ship_quick_fix(env),
        Action::ShipReturnNow => ship_return_now(env),
        // [扫描修 2026-09-15] 开发工具 / 隐藏物品页。
        Action::Dev(tool) => run_dev_tool(env, tool),
        Action::Hidden(h) => run_hidden(env, h),
        // 滑块(只读显示)在 handle_touch 里直接处理;此处占位满足穷尽匹配。
        Action::Slider(_) => {}
    }
}

/// 读某滑块种类的(显示名, 当前值, 上限, 是否被作弊覆盖)。当前值实时读游戏 UserInfoData。
/// [2026-09-16] G-11 宿主 msg_send 同样经过 objc_msgSend 的作弊钩子:FORCE_LEVEL 开着时 curLevel 返回强制等级,
/// max_facility 开着时 totalWorkers/totalRooms 恒返回 99。读法不改,只把「被覆盖」标出来,免得玩家以为存档已改。
/// [2026-09-24 第四轮 K14 N-D2-4] 配合 K13(I3-4)后 max_facility 不再覆盖宿主读到的工人/房间值,见下面 overridden。
fn slider_info(env: &mut Environment, kind: SliderKind) -> (&'static str, i64, i64, bool) {
    let (name, max): (&'static str, i64) = match kind {
        SliderKind::Level => ("等级", 52),
        SliderKind::Gold => ("摩尔豆", 9_999_999),
        SliderKind::VipGold => ("贝壳", 2_000_000),
        SliderKind::Workers => ("工人", 99),
        SliderKind::Rooms => ("房间", 99),
    };
    let overridden = match kind {
        SliderKind::Level => crate::mole_cheats::level() > 0,
        // [2026-09-24 第四轮 K14 N-D2-4] 配合 K13(I3-4):max_facility 只对 MAXFAC_GATE_LRS 里的人力门/HUD 调用点返回 99,
        // 宿主 msg_send 的返回地址不在白名单里,这里读到的就是存档真值;totalRooms 臂整条删了。所以工人/房间不再标
        // 「作弊覆盖」(否则开着开关时会误报)。与开关改名同一提交,K13 未合入时一起回退。
        SliderKind::Workers | SliderKind::Rooms => false,
        SliderKind::Gold | SliderKind::VipGold => false,
    };
    // [2026-09-24 第四轮 K14 N-D2-4] 不在主村(判定同「工人数 = 20」的门)时,「工人」改读岛档
    // [[NewSceneData sharedInstance] userInfoDataInNewScene](T@"NewSceneUserInfoData",getter@0x223cf4)的
    // curTotalWorkersCount(Ti,getter@0x3239c4 读 +8),与岛上抬头显示同源;max_facility 不拦岛上的 getter,
    // 所以不标作弊覆盖。只读显示,不写存档。「房间」岛上没有对应数值,照旧读主村并在名字里注明。
    if matches!(kind, SliderKind::Workers | SliderKind::Rooms) && outside_main_village(env).is_some() {
        if matches!(kind, SliderKind::Rooms) {
            let ui = user_info_data(env);
            if ui == nil {
                return ("房间(主村)", 0, max, overridden);
            }
            let s = sel(env, "totalRooms");
            let cur: i32 = msg_send(env, (ui, s));
            return ("房间(主村)", cur as i64, max, overridden);
        }
        let nsd = game_singleton(env, "NewSceneData", "sharedInstance");
        let island_ui: id = if nsd != nil {
            let s = sel(env, "userInfoDataInNewScene");
            msg_send(env, (nsd, s))
        } else {
            nil
        };
        if island_ui == nil {
            return ("岛上工人", 0, max, false);
        }
        let s = sel(env, "curTotalWorkersCount");
        let cur: i32 = msg_send(env, (island_ui, s));
        return ("岛上工人", cur as i64, max, false);
    }
    let ui = user_info_data(env);
    if ui == nil {
        return (name, 0, max, overridden);
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
    (name, cur as i64, max, overridden)
}

// [扫描修 2026-09-15] 删掉 slider_set:滑块早已改为只读显示,它标着 allow(dead_code) 且全仓零引用;
// SliderKind 仍被 slider_info/render_slider 使用,保留。

/// 在一个格子里渲染点击式滑块:深色轨道 + 亮色填充(宽=当前/上限) + 实时值文字。
fn render_slider(env: &mut Environment, container: id, frame: CGRect, kind: SliderKind, white: id) {
    let (name, cur, max, overridden) = slider_info(env, kind);
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
    let txt = format!(
        "{} {}/{} (只读){}",
        name,
        cur,
        max,
        if overridden { "(作弊覆盖)" } else { "" }
    );
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

/// [SceneMannager curSceneId]:1 = 主村,10 = 黄金岛,2 = 切场景过场/加载中;单例拿不到时返回 -1。
/// 取法与 mole_items::place_item_route 相同(+[SceneMannager sharedManager]@0x240cec 懒建,curSceneId@0x241730)。
fn cur_scene_id(env: &mut Environment) -> i32 {
    let sm = game_singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return -1;
    }
    let s = sel(env, "curSceneId");
    msg_send(env, (sm, s))
}

/// [2026-09-24 第四轮 K14 N-D2-4] 只改主村 UserInfoData 的按钮(工人数/房间数 = 20)与「工人」滑块共用的场景判定,
/// 口径同召唤 TestLayer(G-09):curSceneId 不是 1(10 = 黄金岛,2 = 切场景过场,-1 = 单例还没建)或岛会话仍活跃
/// (进出岛过场)都算不在主村。只看 island_session_active() 会漏掉在线进岛(在线时离线岛标志恒为 false)。
/// 返回 Some((curSceneId, 岛会话是否活跃)) = 不在主村;None = 在主村。会发宿主消息,只在菜单事件里调用。
fn outside_main_village(env: &mut Environment) -> Option<(i32, bool)> {
    let cur = cur_scene_id(env);
    let island = crate::mole_cheats::island_session_active();
    if cur != 1 || island {
        Some((cur, island))
    } else {
        None
    }
}

fn summon_class(env: &mut Environment, name: &str, z: i32) {
    let cls = env.objc.get_known_class(name, &mut env.mem);
    if cls == nil {
        log!("[MOLEMENU] class {} not found", name);
        set_toast(format!("召唤失败:找不到类 {}", name));
        return;
    }
    // [2026-09-16] G-06 防呆①:dealloc 会清全局单例的类。下面修掉 alloc 的 +1 泄漏后,关闭召唤层会真正 dealloc;
    // -[VillageMenuLayer dealloc]@0x5ed68 在 0x5edba/0x5edcc purge CropViewLayer 与 NewStyleStoreMainLayer 单例,
    // 关一次就把场景里那份正在用的商店/作物界面清掉。召唤列表其余类的 dealloc 已逐个核过:只清自己的
    // NetworkManager 代理、取消调度、移除触摸代理/通知或卸载自己的精灵帧,不 purge 全局单例。
    if name == "VillageMenuLayer" {
        log!("[MOLEMENU] 拒绝召唤 {}(dealloc 会 purge 商店/作物单例)", name);
        set_toast(format!(
            "拒绝召唤 {}:它销毁时会清掉商店/作物单例,关掉后场景里正在用的界面会坏",
            name
        ));
        return;
    }
    // [2026-09-16] G-06 防呆②:单例类不 alloc 第二份。第二份与场景里那份并存,回调目标/顶层视图通知都对不上
    // (新版商店「易卡」就是这个根因);ShowFreeShellsLayer/ShowAdwallBoardLayer 用的是 +shareInstance 命名。
    // 类对象的 isa 是元类,object_has_method_named 查到的是类方法。
    for shared in ["sharedInstance", "shareInstance", "sharedManager"] {
        if env.objc.object_has_method_named(&env.mem, cls, shared) {
            log!("[MOLEMENU] 拒绝召唤 {}(响应 +{},是单例)", name, shared);
            set_toast(format!(
                "拒绝召唤 {}:它是 +{} 单例,请用单例入口打开",
                name, shared
            ));
            return;
        }
    }
    // [2026-09-16] G-09 GM 面板场景门控。-[NewSceneTestLayer onButtonbuildValuePlus:]@0x330f10 加完建设值走
    // saveUserinfoBothInLocalAndRemote@0x21f42c,只写主村 userinfo.dat;岛档只在岛会话里由 mole_cheats 置脏落盘,
    // 进岛时 load_island_userinfo 又会用 island_userinfo.dat 覆盖建设值,所以在主村加的值进岛就没了。
    // TestLayer 的各项改的是主村数据,只在主村开放。
    match name {
        "NewSceneTestLayer" => {
            if cur_scene_id(env) != 10 || !crate::mole_cheats::island_session_active() {
                log!("[MOLEMENU] 拒绝召唤 NewSceneTestLayer:不在黄金岛上");
                set_toast("请在黄金岛上打开「黄金岛GM面板」(在主村加的建设值不会写进岛档)".to_string());
                return;
            }
        }
        "TestLayer" => {
            let cur = cur_scene_id(env);
            // [2026-09-16] 复审修:与 mole_items::place_item_route 同一口径,curSceneId==1 但岛会话仍活跃时是进出岛过场,
            // runningScene 可能正要被换掉,面板挂上去就跟着旧场景走了,同样拒绝。
            let island = crate::mole_cheats::island_session_active();
            if cur != 1 || island {
                log!(
                    "[MOLEMENU] 拒绝召唤 TestLayer:curSceneId={} 岛会话活跃={}",
                    cur,
                    island
                );
                set_toast("请在主村打开「GM面板 TestLayer」".to_string());
                return;
            }
        }
        _ => {}
    }
    // 先取运行中场景再 alloc,免得场景拿不到时 alloc 出来的对象无人释放。
    let director = game_singleton(env, "CCDirector", "sharedDirector");
    if director == nil {
        set_toast("召唤失败:CCDirector 还没初始化".to_string());
        return;
    }
    let rs = sel(env, "runningScene");
    let scene: id = msg_send(env, (director, rs));
    if scene == nil {
        set_toast("召唤失败:当前没有运行中的场景".to_string());
        return;
    }
    let alloc = sel(env, "alloc");
    let obj: id = msg_send(env, (cls, alloc));
    let init = sel(env, "init");
    let obj: id = msg_send(env, (obj, init));
    if obj == nil {
        log!("[MOLEMENU] {} alloc/init failed", name);
        set_toast(format!("召唤失败:{} alloc/init 返回 nil", name));
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
    // [2026-09-16] G-06 抵消 alloc/init 的 +1。原来从不释放:场景(addChild)和 LAST_SUMMONED 各持一份后
    // CloseSummoned 只 remove + release 一次,对象永远不会释放。现在关闭召唤层或它自己从父节点移除后会正常 dealloc。
    release(env, obj);
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

/// 一键收获全部。`ObjectManager.farms` 是游戏自己的地块数组(比 tweak 注入的 gFarmTable 干净)。
/// [2026-09-16] G-03 改成「已成熟的真收获、生长中的只催熟、空地和枯萎地跳过、岛会话中拒绝」。
/// 以前对每一项发 cropMatureHandler:-[Farm cropMatureHandler]@0x4a030 只写 cropStage_=4、[ActorManager releaseActor:]、
/// 挂 AlarmFlag 收获旗,没有任何发奖,也不看地块状态,空地和枯萎地一样挂旗;beginTime 没改,重进游戏又变回生长中。
/// 现在按 farmState_(+376)/cropStage_(+368) 分三类(偏移由 mole_cheats::farm_state_stage 从 ivar 槽现读):
///   · 已成熟(farmState_==4 且 cropStage_==4):走原版一键收获入口 -[WrapperManager harvestOnekey:]@0x38fb98。它判断
///     FlowerFarm/FruitFarm 之后一律 [farm harvest:YES](0x38fc04,YES=不逐块播收获音效)。-[Farm harvest:]@0x49a44 自己再判一次
///     cropStage_==4(0x49a60),然后做成就/任务判定、showOutGoldXP: 发金币经验、优惠券掉落,最后 [self reset];
///     -[Farm reset]@0x482b0 只清地块自身状态,不把地块移出 farms 数组,所以按下标遍历安全。
///   · 生长中(farmState_==4 且 cropStage_!=4):只调 mole_cheats::farm_instant_mature_at 把 beginTime 拨到刚过成熟点,
///     由原版 -[Farm innerupdate:] 下一拍自己走成熟流程挂旗(不直接发 cropMatureHandler),toast 提示稍后再点一次收获。
///   · 其它(farmState_!=4:空地、枯萎地等):跳过。
/// island_session_active() 为真,或 curSceneId 不等于 1(在线进岛、切场景过场)时直接拒绝:岛上没有农田,
/// 岛会话里 ObjectManager.farms 指向什么未核实,不去碰。
/// 只在菜单点击事件里执行(不在 drawScene/mainLoop 帧栈上);发消息前快照寄存器,结束后恢复 r0–r3。
fn harvest_all(env: &mut Environment) {
    if crate::mole_cheats::island_session_active() {
        log!("[MOLEMENU] 一键收获全部:岛会话中,拒绝");
        set_toast("「一键收获全部」只在主村可用,请先回主村".to_string());
        return;
    }
    let saved = *env.cpu.regs();
    // [2026-09-16] G-03 复审修:只看 island_session_active() 不够。那几个标志只由离线进岛钩子置位,在线模式
    // (--allow-network-access)下离线岛总闸被强制关闭,在线进岛走私服 1062 原版路径,标志恒为 false,
    // 岛上点这个按钮会对岛场景下的 ObjectManager.farms 逐个发 harvest:。与召唤 TestLayer 同一口径再判一次
    // curSceneId:不等于 1(10 = 黄金岛,2 = 切场景过场,-1 = 单例还没建)一律拒绝。cur_scene_id 会发宿主消息,
    // 放在快照之后,拒绝路径同样恢复 r0–r3。
    let cur = cur_scene_id(env);
    if cur != 1 {
        env.cpu.regs_mut()[0..4].copy_from_slice(&saved[0..4]);
        log!("[MOLEMENU] 一键收获全部:curSceneId={},不在主村,拒绝", cur);
        set_toast("「一键收获全部」只在主村可用,请先回主村".to_string());
        return;
    }
    let text = harvest_farms(env);
    env.cpu.regs_mut()[0..4].copy_from_slice(&saved[0..4]);
    set_toast(text);
}

/// [2026-09-16] G-03 harvest_all 的主体,返回给玩家看的 toast 文案。
fn harvest_farms(env: &mut Environment) -> String {
    let om = game_singleton(env, "ObjectManager", "sharedManager");
    if om == nil {
        log!("[MOLEMENU] ObjectManager == nil");
        return "一键收获全部:还没进村,找不到农田".to_string();
    }
    let farms_sel = sel(env, "farms");
    let farms: id = msg_send(env, (om, farms_sel));
    if farms == nil {
        log!("[MOLEMENU] ObjectManager.farms == nil");
        return "一键收获全部:还没进村,找不到农田".to_string();
    }
    let wm = game_singleton(env, "WrapperManager", "sharedManager");
    if wm == nil {
        log!("[MOLEMENU] WrapperManager == nil");
        return "一键收获全部:WrapperManager 还没初始化,什么都没做".to_string();
    }
    let count_sel = sel(env, "count");
    let count: u32 = msg_send(env, (farms, count_sel));
    let obj_at = sel(env, "objectAtIndex:");
    let harvest_onekey = sel(env, "harvestOnekey:");
    let mut harvested = 0u32;
    let mut ripening = 0u32;
    let mut skipped = 0u32;
    for i in 0..count {
        let farm: id = msg_send(env, (farms, obj_at, i));
        if farm == nil {
            skipped += 1;
            continue;
        }
        match crate::mole_cheats::farm_state_stage(env, farm.to_bits()) {
            Some((4, 4)) => {
                let _: () = msg_send(env, (wm, harvest_onekey, farm));
                harvested += 1;
            }
            Some((4, _)) => {
                if crate::mole_cheats::farm_instant_mature_at(env, farm.to_bits()) {
                    ripening += 1;
                } else {
                    skipped += 1;
                }
            }
            _ => skipped += 1,
        }
    }
    if harvested > 0 {
        save_user_info(env);
    }
    log!(
        "[MOLEMENU] 一键收获全部:共 {} 块地,收获 {},催熟 {},跳过 {}",
        count,
        harvested,
        ripening,
        skipped
    );
    if ripening > 0 {
        format!(
            "一键收获:收获 {} 块,催熟 {} 块(稍后再点一次收获),跳过 {} 块(空地/枯萎)",
            harvested, ripening, skipped
        )
    } else {
        format!(
            "一键收获:收获 {} 块,跳过 {} 块(空地/枯萎)",
            harvested, skipped
        )
    }
}

/// Open the Golden Island (加勒比寻宝) activity offline. Directly summoning the
/// layer (alloc/init/addChild) does NOT trigger its display — it's data-driven
/// and normally waits for a server callback that never arrives offline. So we:
/// (1) turn the fix on + build/store the local CaribbeanDiscoveringData so the
/// `caribbeanData` getter and the ivar both return valid data; (2) create the
/// layer and add it to the scene; (3) force `displayUI` to build the UI now.
fn open_caribbean(env: &mut Environment) {
    // [扫描修 2026-09-15] 改用稳定的 is_on/toggle 打开「修复加勒比寻宝」开关(fix_golden_island),
    // 不依赖 mole_cheats 里的专用开启函数,避免清理死代码时菜单这边编译失败。
    // [复核修 2026-09-15] 注释不再引用已删除的函数名。
    if !crate::mole_cheats::is_on("fix_golden_island") {
        crate::mole_cheats::toggle("fix_golden_island");
    }
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

/// 岛上一键回主村:找当前村庄层(岛上=HolidayVillageLayer),调原版 gobackMainVillage(和点飞机确认同路径:
/// -[HolidayVillageLayer checkSpecailZone:rect:] 在 0x23d456-0x23d468 弹 MessageBox 确认框,回调选择子就是它),
/// 由原版 startNewSceneFrom:10 toScene:1 回主村。不在岛上(无该方法)则只提示。
/// [2026-09-24 第四轮 K14 N-D5-3] 以前发的是 returnToMainVillage@0x23d6c4:它在 0x23d6e2-0x23d6f4 先
/// [[NetworkManager sharedInstance] setConnectFirstInThisOpen:1] 再调 gobackMainVillage,原版只在断网弹框关闭(0x23b6e4)
/// 与收包错误分支(0x23eaae)走它。在线模式回村后 startGame: 排的 sendGameData2Server: 在 0x21064 读到这个标志,就改发
/// getLocalUserAndMapInfo(1001)重拉,回包可能弹存档比对框或用云端覆盖本地;离线时 0x20f7c 的 isReachable 门先返回,不受影响。
/// 现在直接发 gobackMainVillage,不去动 connectFirstInThisOpen 本身(不走断网路径就是忠于原版)。
/// 退岛存盘在 mole_cheats 的 startNewSceneFrom 10→1 出口(island_flush),不在 gobackMainVillage 上(那里没有钩子臂)。
fn exit_island(env: &mut Environment) {
    let wm = game_singleton(env, "WrapperManager", "sharedManager");
    let village: id = if wm != nil {
        let s = sel(env, "currentVillageLayer");
        msg_send(env, (wm, s))
    } else {
        nil
    };
    // gobackMainVillage 与 returnToMainVillage 一样只在 HolidayVillageLayer 上实现,「在岛上」的判定语义不变。
    if village == nil || !env.objc.object_has_method_named(&env.mem, village, "gobackMainVillage") {
        log!("[MOLEMENU] currentVillageLayer/gobackMainVillage unavailable (need to be on island)");
        set_toast("回主村失败:你现在不在黄金岛上".to_string());
        return;
    }
    // ★[深扫修 2026-09-11] #19 岛上 gameMode 前置门(写法与 enter_island 的门①一致)。
    //   根因:岛上拿起建筑进入移动/放置态时 NewGameManager.gameMode=2/3(NewSceneMoveLayer onButtonMoveSelected:
    //   0x2b35b2/0x2b3602),9/11 是菜单瞬态。原版的离岛入口(点飞机 gobackMainVillage)只在浏览态(=1)可点,只有本菜单
    //   绕过 UI 直接发离岛方法(K14 N-D5-3 起为 gobackMainVillage,以前是 returnToMainVillage)。离岛加载 -[LoadingMainVillage updateLoading:] 在 0x2543b6 把岛上 gameMode 拷进
    //   主村 GameManager,0x254f48 见 ≠1 就跳过 [GameData loadFromLocal] → 主村不重读档、loadMapObjects/createIdleWorkers
    //   在内存旧值上再扣一轮工人(可扣成负数并被存盘),且主村 VillageLayer processTouch 读到 ≠1 直接不响应点击。
    //   修法:≠1 就拒绝并提示,不自动复位(resetGamemode 只管 9/11,管不了移动层的 2/3;代调 NewSceneMoveLayer detech
    //   还得保证 purge 单例等收尾,风险更高)。菜单动作在 UIKit 事件处理里执行,不在 drawScene 帧栈,可安全发消息。
    let ngm = game_singleton(env, "NewGameManager", "sharedManager");
    let ngmode: i32 = if ngm != nil {
        let s = sel(env, "gameMode");
        msg_send(env, (ngm, s))
    } else {
        -1
    };
    if ngmode != 1 {
        log!("[MOLEMENU] exit island refused: NewGameManager.gameMode={}", ngmode);
        set_toast(format!(
            "回主村失败:当前处于编辑/移动/放置状态(gameMode={}),请先退出编辑/移动状态再试",
            ngmode
        ));
        return;
    }
    // ★[审计修 2026-09-11] 真 gobackMainVillage 第一件事读 isChangeSceneButtonSelected,为真就早退(0x23d19c)。
    //   以前不查就发,日志报成功、人却留在岛上。先查,忙就明确提示。
    if scene_change_busy(env) {
        log!("[MOLEMENU] exit island refused: isChangeSceneButtonSelected=1");
        set_toast("回主村失败:场景切换进行中,稍后再试".to_string());
        return;
    }
    // [2026-09-24 第四轮 K14 N-D5-3] 无参数(v8@0:4);菜单动作在 UIKit 事件处理里执行,不在 drawScene 帧栈,可直接发。
    let s = sel(env, "gobackMainVillage");
    let _: () = msg_send(env, (village, s));
    log!("[MOLEMENU] exit island -> [village gobackMainVillage](同飞机确认路径)");
    teardown(env);
}

/// [2026-09-24 第四轮 K14 I4-04] 探险船 GM 的共用前置:仅离线(在线时船的进度由服务器同步,setModObjectToServer: 会真发包)、
/// 在岛上(curSceneId==10 且岛会话活跃),然后取 [[ObjectManager sharedManager] getDiscovership](@0x46a40,@8@0:4)。
/// getDiscovership 自己在 0x46a8e 判 curSceneId==10,遍历 ObjectManager.objects 找 objectId==34001(0x46b4e)
/// 且 isKindOfClass:DiscoveryShip(0x46b6a)的对象,找不到返回 nil。失败时返回给玩家看的原因。
/// 会发宿主消息,只在菜单点击事件里调用(不在 drawScene 帧栈,也不在 intercept 钩子里)。
fn island_discovery_ship(env: &mut Environment) -> Result<id, String> {
    if env.options.network_access {
        return Err("在线模式下船的进度由服务器同步,不能用 GM 操作".to_string());
    }
    let cur = cur_scene_id(env);
    if cur != 10 || !crate::mole_cheats::island_session_active() {
        return Err("请在黄金岛上使用".to_string());
    }
    let om = game_singleton(env, "ObjectManager", "sharedManager");
    if om == nil {
        return Err("ObjectManager 还没初始化".to_string());
    }
    let s = sel(env, "getDiscovership");
    let ship: id = msg_send(env, (om, s));
    if ship == nil {
        return Err("岛上没找到探险船".to_string());
    }
    Ok(ship)
}

/// [2026-09-24 第四轮 K14 I4-04] 探险船一键修好:shipState!=2(未修好)时发原版 -[DiscoveryShip quickFixShip]@0x362e20(v8@0:4)。
/// 它是原版贝壳加速修船的同一出口(-[DiscoveryShipView onChooseUse] 扣完贝壳后在 0x366aee 调它,本菜单不扣贝壳):
/// isFixing_=0、shipState=2(0x362e5c)、[[NewSceneQuest sharedInstance] checkAction:15 object:](修船任务进度)、
/// beginFixTime_=0、unschedule innerUpdate:、播放待命动画、canSail_=1、挂出海旗 sailFlag,最后 setModObjectToServer:
/// (离线由 mole_cheats 的回写钩子按 seqId 写回岛 mapData 并置脏落盘)。修船不占工人,不需要还工人。
/// shipState==2 时不发:quickFixShip 会无条件新建 sailFlag 覆盖旧的,已修好再发会叠出第二面旗。
fn ship_quick_fix(env: &mut Environment) {
    let ship = match island_discovery_ship(env) {
        Ok(ship) => ship,
        Err(e) => {
            log!("[MOLEMENU] 探险船一键修好:{}", e);
            set_toast(format!("探险船一键修好失败:{}", e));
            return;
        }
    };
    let s = sel(env, "shipState");
    let state: i32 = msg_send(env, (ship, s));
    if state == 2 {
        log!("[MOLEMENU] 探险船一键修好:shipState=2,已经修好");
        set_toast("探险船已经修好了(待出海或出海中),不需要再修".to_string());
        return;
    }
    let s = sel(env, "quickFixShip");
    let _: () = msg_send(env, (ship, s));
    let s = sel(env, "shipState");
    let after: i32 = msg_send(env, (ship, s));
    log!("[MOLEMENU] 探险船一键修好:shipState {} → {}(原版 quickFixShip)", state, after);
    set_toast(format!("探险船已修好(shipState {} → {}),点船即可出海", state, after));
}

/// [2026-09-24 第四轮 K14 I4-04] 探险船立即返航:只在出海中(isSailing,c8@0:4)时,把 beginDiscoverTime_ 写成
/// now − discoverTime_ − 1,由原版每秒一次的 innerUpdate:(出海时 -[DiscoveryShip onButtonDiscoverSelected] 在 0x362460
/// 以 1.0 秒间隔排定)自己结算:0x36279a-0x3627ee 算 now−begin ≥ discoverTime_ 后,经 checkIsShipInScreen 门
/// (船坞在屏幕内时原版会推迟)播放返航动画、清 isSailing_/beginDiscoverTime_、setModObjectToServer:、记 lastSailingTime,
/// 之后原版挂领奖旗、发礼物。这里不伪造礼物、不发消息改状态,只拨一个时间戳。
/// now 取 [[NewSceneTimer sharedInstance] getCurrentServerTime](L8@0:4,宿主消息同样经过 mole_cheats 对它的钩子,
/// 与原版 innerUpdate: 取的是同一个时钟)。偏移从 guest 的 _OBJC_IVAR 槽现读(兼容 touchHLE 非脆弱 ivar 修正写回):
/// re.py ivar DiscoveryShip 核得 discoverTime_ 槽 0xb07c24(静态 +396,L)、beginDiscoverTime_ 槽 0xb07c30(静态 +416,d)、
/// 末尾 ivar updateCount 槽 0xb07c58(静态 +452,i),实例大小 456。三者都不小于静态值、相对位置不变,否则放弃不写。
fn ship_return_now(env: &mut Environment) {
    let ship = match island_discovery_ship(env) {
        Ok(ship) => ship,
        Err(e) => {
            log!("[MOLEMENU] 探险船立即返航:{}", e);
            set_toast(format!("探险船立即返航失败:{}", e));
            return;
        }
    };
    let s = sel(env, "isSailing");
    let sailing: u8 = msg_send(env, (ship, s));
    if sailing == 0 {
        log!("[MOLEMENU] 探险船立即返航:船不在出海中");
        set_toast("探险船现在没有出海,不需要返航".to_string());
        return;
    }
    // 静态布局(objc_meta:instanceSize 456;discoverTime_ +396;beginDiscoverTime_ +416;最后一个 ivar updateCount +452,i)。
    const SLOT_DISCOVER_TIME: u32 = 0xb07c24;
    const SLOT_BEGIN_DISCOVER_TIME: u32 = 0xb07c30;
    const SLOT_UPDATE_COUNT: u32 = 0xb07c58;
    const STATIC_OFF_DISCOVER: u32 = 396;
    const STATIC_OFF_BEGIN: u32 = 416;
    const STATIC_OFF_UPDATE_COUNT: u32 = 452;
    let read_slot = |env: &Environment, slot: u32| -> u32 {
        env.mem.read(crate::mem::ConstPtr::<u32>::from_bits(slot))
    };
    let off_discover = read_slot(env, SLOT_DISCOVER_TIME);
    let off_begin = read_slot(env, SLOT_BEGIN_DISCOVER_TIME);
    let off_update = read_slot(env, SLOT_UPDATE_COUNT);
    // 非脆弱 ivar 修正只会把整个类的 ivar 统一往后挪,三者相对位置不变;beginDiscoverTime_ 后面还有本类的 updateCount,
    // 相对位置对得上就说明 beginDiscoverTime_ 的 8 字节整个落在实例内。任何一项对不上都不写。
    let layout_ok = off_discover >= STATIC_OFF_DISCOVER
        && off_begin >= STATIC_OFF_BEGIN
        && off_begin < 0x1000
        && off_begin.checked_sub(off_discover) == Some(STATIC_OFF_BEGIN - STATIC_OFF_DISCOVER)
        && off_update.checked_sub(off_begin) == Some(STATIC_OFF_UPDATE_COUNT - STATIC_OFF_BEGIN);
    if !layout_ok {
        log!(
            "[MOLEMENU] 探险船立即返航:ivar 偏移异常(discoverTime_={} beginDiscoverTime_={} updateCount={}),放弃",
            off_discover,
            off_begin,
            off_update
        );
        set_toast("探险船立即返航失败:船的内存布局和预期不符,没有改动".to_string());
        return;
    }
    let discover: u32 = env
        .mem
        .read(crate::mem::ConstPtr::<u32>::from_bits(ship.to_bits() + off_discover));
    let timer = game_singleton(env, "NewSceneTimer", "sharedInstance");
    if timer == nil {
        set_toast("探险船立即返航失败:NewSceneTimer 还没初始化".to_string());
        return;
    }
    let s = sel(env, "getCurrentServerTime");
    let now: u32 = msg_send(env, (timer, s));
    let target = now as f64 - discover as f64 - 1.0;
    // innerUpdate: 在 0x362740 见 beginDiscoverTime_<=0 就走修船分支,不结算出海;拨不出正数就不写。
    if target < 1.0 {
        log!(
            "[MOLEMENU] 探险船立即返航:now={} discoverTime_={},算出的起点 {} 不是正数,放弃",
            now,
            discover,
            target
        );
        set_toast("探险船立即返航失败:游戏时钟异常,没有改动".to_string());
        return;
    }
    let begin_ptr: crate::mem::MutPtr<f64> = Ptr::from_bits(ship.to_bits() + off_begin);
    let old: f64 = env.mem.read(begin_ptr);
    env.mem.write(begin_ptr, target);
    log!(
        "[MOLEMENU] 探险船立即返航:beginDiscoverTime_ {} → {}(now={} discoverTime_={}),交给原版 innerUpdate: 结算",
        old,
        target,
        now,
        discover
    );
    set_toast(
        "已把出海时间拨到期,原版每秒检查一次并结算返航(船在屏幕内时原版可能要等镜头移开),返航后点领奖旗领奖".to_string(),
    );
}

/// 一键进入 NewScene 可建筑黄金岛(scene id 10)。arm 进岛(开功能/开窗/预注入默认岛
/// mapData)后直接 `[SceneMannager startNewSceneFrom:1 toScene:10]`;网络门与 state2
/// 数据门由 mole_cheats 的 intercept 在进岛窗口内放行。跳过飞机过场(热点路径仍带)。
fn enter_island(env: &mut Environment) {
    let wm = game_singleton(env, "WrapperManager", "sharedManager");
    let village: id = if wm != nil {
        let s = sel(env, "currentVillageLayer");
        msg_send(env, (wm, s))
    } else {
        nil
    };
    if village == nil || !env.objc.object_has_method_named(&env.mem, village, "enterNewIslands") {
        log!("[MOLEMENU] currentVillageLayer/enterNewIslands unavailable (need to be in main village)");
        set_toast("进岛失败:需要在主村里操作".to_string());
        return;
    }
    let offline = !env.options.network_access;
    // ★[审计修 2026-09-11] 真 enterNewIslands(0x375b0)有两道前置门,不满足就静默 return;以前不查就发、日志照报成功。
    // 门① GameManager.gameMode ∈ {0,1,6}
    let gm = game_singleton(env, "GameManager", "sharedManager");
    let gmode: i32 = if gm != nil {
        let s = sel(env, "gameMode");
        msg_send(env, (gm, s))
    } else {
        -1
    };
    if !matches!(gmode, 0 | 1 | 6) {
        log!("[MOLEMENU] enter island refused: GameManager.gameMode={}", gmode);
        set_toast(format!("进岛失败:当前处于编辑/弹窗状态(gameMode={}),先退出再试", gmode));
        return;
    }
    // 门② SceneMannager.isChangeSceneButtonSelected == NO
    if scene_change_busy(env) {
        // ★[审查修 2026-09-11] 只在【离线】且不在任何岛会话里时,才把它当成"上次离线进岛半路失败的残留"复位
        //   (原版复位点在被我们吞掉的发包路径里)。在线模式下岛钩子全关、会话标志从不置位,此时标志为 1 就是一次
        //   真实的进岛请求正在等服务器回包;原版有自己的清零路径,复位只会让同一次进岛再发一遍请求。
        if !offline || crate::mole_cheats::island_session_active() {
            set_toast("进岛失败:场景切换进行中,稍后再试".to_string());
            return;
        }
        let sm = game_singleton(env, "SceneMannager", "sharedManager");
        if sm != nil {
            let s = sel(env, "setIsChangeSceneButtonSelected:");
            let _: () = msg_send(env, (sm, s, false));
        }
        log!("[MOLEMENU] isChangeSceneButtonSelected 卡在 1(上次离线进岛失败残留)→ 已复位");
    }
    crate::mole_cheats::island_arm_entry();
    let s = sel(env, "enterNewIslands");
    let _: () = msg_send(env, (village, s));
    // ★[审查修 2026-09-11] 受理判据:真方法过了两道前置门后会在 0x37692 置 isChangeSceneButtonSelected=1。离线(gate#1 吞包、
    //   SUCC 异步排队)与在线(要等 1001 回包、加载结束才由 switchToNewScene 清 0)都能读到 1。原来用 gate#1 命中当判据,
    //   在线模式岛钩子全关、永远命中不了 → 每次都误报失败且菜单不关。gate#1 只在离线时附带打日志。
    if scene_change_busy(env) {
        let gate1 = if offline && crate::mole_cheats::island_gate1_hit() { ",gate#1 已接管" } else { "" };
        log!("[MOLEMENU] enter island -> [village enterNewIslands] accepted{}", gate1);
        teardown(env);
    } else {
        log!("[MOLEMENU] enter island -> [village enterNewIslands] REJECTED (isChangeSceneButtonSelected stayed 0)");
        set_toast("进岛失败:游戏拒绝了进岛请求(详见日志)".to_string());
    }
}

/// [SceneMannager isChangeSceneButtonSelected]:场景切换进行中标志。进岛/离岛真方法开头都读它,为真即早退。
fn scene_change_busy(env: &mut Environment) -> bool {
    let sm = game_singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return false;
    }
    let s = sel(env, "isChangeSceneButtonSelected");
    let v: u8 = msg_send(env, (sm, s));
    v != 0
}

// ============================================================================
// [扫描修 2026-09-15] 开发工具页 / 隐藏物品页 / 布局自检。菜单只调用 mole_dev、mole_items 的公开函数,
// 不直接碰它们的内部状态;所有动作都在 UIKit 事件分派里执行,不在 drawScene 帧栈上。
// ============================================================================

fn quest_family_name(f: QuestFamily) -> &'static str {
    match f {
        QuestFamily::Main => "主线",
        QuestFamily::Time => "限时",
        QuestFamily::Vip => "VIP",
        QuestFamily::Island => "黄金岛",
    }
}

/// 执行开发工具按钮,把 DevResult 的 Ok/Err 文案写进底部 toast。
fn run_dev_tool(env: &mut Environment, tool: DevTool) {
    use crate::mole_dev as dev;
    let reg = dev::register_value();
    let (what, result): (String, dev::DevResult) = match tool {
        DevTool::RegShow | DevTool::Spacer => return,
        DevTool::RegDigit(d) => {
            dev::register_push_digit(d);
            return;
        }
        DevTool::RegBackspace => {
            dev::register_backspace();
            return;
        }
        DevTool::RegNegate => {
            dev::register_negate();
            return;
        }
        DevTool::RegClear => {
            dev::register_clear();
            return;
        }
        DevTool::TimeMinutes(m) => (
            format!("对象计时快进 {} 分钟", m),
            dev::apply_time_minutes(env, m),
        ),
        DevTool::Quest(f) => (
            format!("{}任务跳到 #{}", quest_family_name(f), reg),
            dev::quest_jump(env, f, reg),
        ),
        DevTool::Story => (format!("播放剧情 #{}", reg), dev::story_play(env, reg)),
        DevTool::UnlockInteraction => ("解锁交互".to_string(), dev::unlock_interaction(env)),
        DevTool::Weather => (format!("天气 #{}", reg), dev::weather(env, reg)),
        DevTool::TimeScale(s) => (format!("倍速 ×{}", s), dev::set_time_scale(env, s)),
        DevTool::Fps => ("FPS 显示".to_string(), dev::toggle_fps(env)),
        DevTool::MapGrid => ("地图格线".to_string(), dev::toggle_map_grid(env)),
        DevTool::TimeTravelHours(h) => (
            format!("时间旅行 +{} 小时", h),
            dev::time_travel_hours(env, h),
        ),
        DevTool::SnapshotSave => ("存档快照保存".to_string(), dev::snapshot_save(env)),
        DevTool::SnapshotRestore => (
            "快照下次启动恢复".to_string(),
            dev::snapshot_restore_on_next_launch(env),
        ),
        DevTool::BuildingStore => ("打开建设商店".to_string(), dev::open_building_store(env)),
        DevTool::CameraCenter => ("相机回中".to_string(), dev::camera_center(env)),
        DevTool::Trace => ("选择子跟踪".to_string(), dev::toggle_trace()),
        DevTool::IslandFastForward => (
            format!("岛档快进 {} 分钟", reg),
            dev::island_fast_forward_minutes(env, reg),
        ),
    };
    match result {
        Ok(text) => {
            log!("[MOLEMENU] 开发工具「{}」成功:{}", what, text);
            let mut shown = if text.is_empty() {
                format!("「{}」完成", what)
            } else {
                text
            };
            if matches!(tool, DevTool::Quest(QuestFamily::Time)) {
                // [2026-09-16] A2-05 更正 F7-1 复核的「简体下有语言门」:-[TimeQuest activate:]@0x1d88b4 在 0x1d88ee
                // 先查 currentUserLanguange:1(zh-Hans),为真直接放行;-[TimeQuest setCanActivate:] 只在 en 下清零。
                // 真正的条件是 GameManager.gameMode 不为 0/6(0x1d892a/0x1d893e)与 checkCanActivate(等级≥needLevel 等)。
                // [2026-09-16] 复审修:去掉重复的「限时任务」前缀。连同 quest_jump 的正文整句约 1040pt,超出 992 宽的 toast
                // (UILabel 单行不裁剪,居中后两侧溢出屏外),缩短后能放下。
                shown.push_str("(需不在 gameMode 0/6、等级≥needLevel,由雅丽激活)");
            }
            if matches!(tool, DevTool::Quest(QuestFamily::Island)) {
                // [2026-09-24 第四轮 K14 I4-4] 黄金岛跳转也补发了 [[NewSceneQuest sharedInstance] activate:0](见 mole_dev::quest_jump)。
                // -[NewSceneQuest activate:]@0x328190 的门:NewGameManager.gameMode 不为 0(0x3281d0)/6(0x3281e6);
                // checkCanActivate 里岛等级≥needLevel(0x32841e)。等级不够时置不上 canActivate 是原版行为。
                // toast 超宽会自动折行(add_toast),不必再缩短 quest_jump 的正文。
                shown.push_str("(需不在 gameMode 0/6、岛等级≥needLevel;布兰头顶出感叹号后点击接任务)");
            }
            set_toast(shown);
        }
        Err(e) => {
            log!("[MOLEMENU] 开发工具「{}」失败:{}", what, e);
            set_toast(format!("「{}」失败:{}", what, e));
        }
    }
}

/// 开发工具按钮的显示文案与底色。
fn dev_display(env: &mut Environment, label: &str, tool: DevTool) -> (String, id) {
    let reg = crate::mole_dev::register_value();
    match tool {
        DevTool::RegShow => (format!("寄存器 = {}", reg), color(env, 0.1, 0.12, 0.16, 1.0)),
        DevTool::RegDigit(_) => (label.to_string(), color(env, 0.3, 0.32, 0.4, 1.0)),
        DevTool::RegBackspace | DevTool::RegNegate | DevTool::RegClear => {
            (label.to_string(), color(env, 0.42, 0.36, 0.22, 1.0))
        }
        DevTool::Spacer => (String::new(), color(env, 0.0, 0.0, 0.0, 0.0)),
        DevTool::Quest(_) | DevTool::Story | DevTool::Weather | DevTool::IslandFastForward => {
            (format!("{} #{}", label, reg), color(env, 0.16, 0.45, 0.7, 1.0))
        }
        DevTool::Trace => {
            let on = crate::mole_dev::trace_on();
            let c = if on {
                color(env, 0.2, 0.62, 0.28, 1.0)
            } else {
                color(env, 0.45, 0.3, 0.32, 1.0)
            };
            (format!("{}: {}", label, if on { "开" } else { "关" }), c)
        }
        // 不可回退的动作用警示色。
        DevTool::TimeTravelHours(_) | DevTool::SnapshotRestore => {
            (label.to_string(), color(env, 0.6, 0.25, 0.2, 1.0))
        }
        _ => (label.to_string(), color(env, 0.16, 0.45, 0.7, 1.0)),
    }
}

/// [2026-09-16] X4-01 删档失败的 toast 文案。已删的都写回时说清「存档保持原样、游戏不退出」;
/// 写回也失败时如实列出已丢失的文件,让玩家去看日志 [RESET]。
fn reset_failure_toast(fail: &crate::save_reset::ResetFailure) -> String {
    let problems = fail.problems.join("、");
    if fail.not_restored.is_empty() {
        format!(
            "删档未完成,存档保持原样,游戏不退出:{}。多半是文件只读、被锁定或被其他程序占用,处理后再删一次",
            problems
        )
    } else {
        format!(
            "⚠️ 删档中止({}),且 {} 写回失败、已丢失;游戏不退出,详情见日志 [RESET]",
            problems,
            fail.not_restored.join("、")
        )
    }
}

/// 需要二次确认的开发工具动作:返回(确认编码, 第一次点击时的提示)。编码非 0 且各动作互不相同。
fn dev_confirm(action: Action) -> Option<(u32, String)> {
    match action {
        // [2026-09-16] X4-02 确认文案补上活动中心的限制:旅行期间 mole_activity 侧档只写内存(F2-05),付费操作的扣款和发奖
        // 却照常写进主档,所以这些操作在旅行中被禁用(拦截在 mole_activity.rs);旅行中拍快照时,主档是旅行后的,
        // 活动档还是旅行前的。文案超过一行,底部 toast 会自动折行(add_toast)。
        // [2026-09-24 第四轮 K3 I7-01] 补黄金岛:旅行期间岛档一律不落盘(mole_cheats::island_flush 开头的落盘闸);每次进岛都从磁盘读岛档,离岛再进或重启后都回到旅行前。
        Action::Dev(DevTool::TimeTravelHours(h)) => Some((
            1000 + h.clamp(0, 1_000_000) as u32,
            format!(
                "⚠️ 时间旅行 +{} 小时不可回退(存档时间戳会跟着往前走)。旅行期间活动中心的付费操作(补签、刷新/挖贝、珍珠与脚印兑换)会被禁用,旅行中拍的快照里活动数据与主档不一致。黄金岛进度在旅行期间不保存,离岛再进或重启后都回到旅行前。再点一次确认",
                h
            ),
        )),
        Action::Dev(DevTool::SnapshotRestore) => Some((
            1,
            "⚠️ 下次启动会用快照覆盖当时的存档,再点一次「快照:下次启动恢复」确认".to_string(),
        )),
        _ => None,
    }
}

fn catalog_page_count() -> usize {
    crate::mole_items::hidden_catalog()
        .len()
        .div_ceil(CATALOG_PER_PAGE)
        .max(1)
}

/// 当前目录页(越界时夹到最后一页)。
fn catalog_current_page() -> usize {
    CATALOG_PAGE
        .with(|c| c.get())
        .min(catalog_page_count() - 1)
}

/// 当前目录页第 slot 格对应的物品;没有则 None。
fn catalog_item_at(slot: usize) -> Option<&'static crate::mole_items::HiddenItem> {
    crate::mole_items::hidden_catalog().get(catalog_current_page() * CATALOG_PER_PAGE + slot)
}

fn catalog_item_label(it: &crate::mole_items::HiddenItem) -> String {
    // 名字与类别截短,防止 320 宽的格子里文字外溢到相邻格。
    let name: String = it.name.chars().take(9).collect();
    let cat: String = it.category.chars().take(4).collect();
    let mut s = format!("{} {}", it.id, name);
    if !cat.is_empty() {
        s.push_str(&format!("[{}]", cat));
    }
    if it.island {
        s.push_str("·岛");
    }
    s
}

/// 隐藏物品页按钮的显示文案与底色。
fn hidden_display(env: &mut Environment, label: &str, h: HiddenAct) -> (String, id) {
    match h {
        HiddenAct::ShopToggle => {
            let on = crate::mole_items::hidden_shop_on();
            let c = if on {
                color(env, 0.2, 0.62, 0.28, 1.0)
            } else {
                color(env, 0.45, 0.3, 0.32, 1.0)
            };
            (format!("{}: {}", label, if on { "开" } else { "关" }), c)
        }
        HiddenAct::FestivalCycle => (
            format!("{}(点切换)", crate::mole_items::festival_shop_label()),
            color(env, 0.2, 0.5, 0.55, 1.0),
        ),
        HiddenAct::ModeToggle => {
            let to_storage = CATALOG_TO_STORAGE.with(|c| c.get());
            let text = if to_storage {
                "点条目 = 入仓库×1(点切换)"
            } else {
                "点条目 = 放到地图(点切换)"
            };
            (text.to_string(), color(env, 0.55, 0.4, 0.18, 1.0))
        }
        HiddenAct::PrevPage | HiddenAct::NextPage => {
            (label.to_string(), color(env, 0.3, 0.32, 0.4, 1.0))
        }
        HiddenAct::PageInfo => {
            let total = crate::mole_items::hidden_catalog().len();
            let text = if total == 0 {
                "目录为空".to_string()
            } else {
                format!(
                    "第 {}/{} 页 · 共 {} 件",
                    catalog_current_page() + 1,
                    catalog_page_count(),
                    total
                )
            };
            (text, color(env, 0.1, 0.12, 0.16, 1.0))
        }
        HiddenAct::Item(slot) => match catalog_item_at(slot) {
            Some(it) => {
                let c = if it.island {
                    color(env, 0.2, 0.5, 0.55, 1.0)
                } else {
                    color(env, 0.5, 0.35, 0.65, 1.0)
                };
                (catalog_item_label(it), c)
            }
            None => (String::new(), color(env, 0.0, 0.0, 0.0, 0.0)),
        },
    }
}

/// 执行隐藏物品页按钮。
fn run_hidden(env: &mut Environment, h: HiddenAct) {
    match h {
        HiddenAct::ShopToggle => {
            let on = crate::mole_items::toggle_hidden_shop();
            log!("[MOLEMENU] 隐藏物品进商店 -> {}", on);
            set_toast(format!(
                "隐藏物品进商店:{}(下次打开商店时生效)",
                if on { "开" } else { "关" }
            ));
        }
        HiddenAct::FestivalCycle => {
            let label = crate::mole_items::cycle_festival_mode();
            log!("[MOLEMENU] 节日商店模式 -> {}", label);
            set_toast(format!("{}(下次打开商店时生效)", label));
        }
        HiddenAct::ModeToggle => {
            let to_storage = !CATALOG_TO_STORAGE.with(|c| c.get());
            CATALOG_TO_STORAGE.with(|c| c.set(to_storage));
            set_toast(if to_storage {
                "点条目 = 入仓库 ×1(之后从原版仓库取出摆放)".to_string()
            } else {
                "点条目 = 直接放到当前场景地图".to_string()
            });
        }
        HiddenAct::PrevPage | HiddenAct::NextPage => {
            let count = catalog_page_count();
            let cur = catalog_current_page();
            let next = if matches!(h, HiddenAct::PrevPage) {
                if cur == 0 {
                    count - 1
                } else {
                    cur - 1
                }
            } else {
                (cur + 1) % count
            };
            CATALOG_PAGE.with(|c| c.set(next));
        }
        HiddenAct::PageInfo => {}
        HiddenAct::Item(slot) => {
            let it = match catalog_item_at(slot) {
                Some(it) => it,
                None => {
                    set_toast("这一格没有物品".to_string());
                    return;
                }
            };
            let to_storage = CATALOG_TO_STORAGE.with(|c| c.get());
            let (what, result) = if to_storage {
                ("入仓库", crate::mole_items::give_goods(env, it.id, 1))
            } else {
                ("放到地图", crate::mole_items::place_item(env, it.id))
            };
            match result {
                Ok(text) => {
                    log!("[MOLEMENU] 隐藏物品 {} {} {}成功:{}", it.id, it.name, what, text);
                    set_toast(format!("{} {}:{}", it.id, it.name, text));
                }
                Err(e) => {
                    log!("[MOLEMENU] 隐藏物品 {} {} {}失败:{}", it.id, it.name, what, e);
                    set_toast(format!("{} {} {}失败:{}", it.id, it.name, what, e));
                }
            }
        }
    }
}

/// 无头测试坐标自检。无头测试脚本用客户区坐标 tap 26 98 打开「开发者 / 调试」页签、
/// tap 178 519 点「一键进入黄金岛」、tap 220 517 点「岛上一键回主村」。换算与 handle_touch 相同:
/// 横屏逻辑 x = 1024 - gy, y = gx。布局一旦漂移(改了页表或按钮数),这里在日志里明确报警,
/// 而不是让测试莫名其妙地超时。只在不匹配时打日志。
/// [2026-09-16] F2-04 Button.frame 存的是 1024 设计坐标,这里按 4:3 横屏右换算核对设计布局,宽屏下同样成立;
/// 宽屏(--fill-screen)跑无头测试时实际要 tap 的 guest y 需加 ox(1188 宽为 +82),x 不变。
/// [2026-09-24 第四轮 K14 I4-04] 「开发者 / 调试」页加了两项探险船 GM(第 11、12 项),按钮 19→21,每列仍 7 行,
/// 上面三个坐标都不变,所以这里不用改;若以后超过 21 个会变成每列 8 行,下面的核对会在日志里报警。
fn layout_selfcheck(dev_title: &str, page_idx: usize, buttons: &[Button]) {
    let hit = |gx: f32, gy: f32| -> Option<Action> {
        let (lx, ly) = (1024.0 - gy, gx);
        buttons
            .iter()
            .find(|b| in_rect(lx, ly, b.frame))
            .map(|b| b.action)
    };
    if dev_title != "开发者 / 调试" {
        log!(
            "[MOLEMENU] 警告:PAGE_DEV_DEBUG 指向的页标题是「{}」,不是「开发者 / 调试」(页表顺序漂移)",
            dev_title
        );
    }
    if !matches!(hit(26.0, 98.0), Some(Action::SwitchPage(PAGE_DEV_DEBUG))) {
        log!("[MOLEMENU] 警告:无头测试坐标 tap 26 98 没落在「开发者 / 调试」页签上(布局漂移)");
    }
    if page_idx == PAGE_DEV_DEBUG {
        if !matches!(hit(178.0, 519.0), Some(Action::EnterIsland)) {
            log!("[MOLEMENU] 警告:无头测试坐标 tap 178 519 没落在「一键进入黄金岛」上(布局漂移)");
        }
        if !matches!(hit(220.0, 517.0), Some(Action::ExitIsland)) {
            log!("[MOLEMENU] 警告:无头测试坐标 tap 220 517 没落在「岛上一键回主村」上(布局漂移)");
        }
    }
}
