/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [扫描修 2026-09-15] 离线活动复活:活动中心总闸、等级礼包、每日签到、海底寻宝、活动公告、系统公告、
//! 节日烟花等。核心是「离线回环服务器」:离线时截下白名单命令的发包,本地按原协议组回包,
//! 喂给原版解析链。由 mole_cheats::intercept 统一调度。
//!
//! # 回环服务器怎么工作(全部经 re.py 核实)
//! - 游戏所有相关命令都走 `-[NetworkManager sendPacket:commandId:]`@0xe231c:r0=self、r2=NSData 请求体
//!   (可为 nil)、r3=命令号。离线时它先 packetsCount+1(0xe235e)、给包头发 setSendFlag:(0xe2362),再读
//!   `isReachable_`(+180)=0 就返回(0xe2372 → 0xe2770),等于空过。[复核修 2026-09-15] 更正:不是"第一条指令就返回",
//!   所以任何放行真 sendPacket 的路径都必须保证 r0 仍是 NetworkManager。
//! - 我们对白名单命令号(或需要参数的上层发包方法)在这里拦下,按私服 mole-protocol 的编码组一个完整包:
//!   24 字节头(6×小端 u32:packetLen、commandID、sendFlag、userID、errorID、deviceIDHash;errorID 恒 0)
//!   + body + 16 字节 md5(头 ++ body ++ 盐 byte_B3AE64)。与 `-[NetworkManager checkPacketDataSourceWithData:length:]`
//!   @0xebe28 的校验一致(它把尾 16 字节换成盐再 md5 比较)。
//! - 组好的包先放宿主队列,再 `performSelector:withObject:afterDelay:0` 调一个本模块自拦的选择子
//!   `moleActivityLoopback`,在运行循环安全点把包追加进 `NetworkManager.buffer_`(ivar +196,
//!   偏移从 guest 的 _OBJC_IVAR 槽 0xb043f0 现读,兼容非脆弱 ivar 修正),然后 msg_send
//!   `parseBufferWhenDidReadData`@0xebefc(parseData:header:pos: 的唯一调用者)。原版的解码、
//!   GameManager/各层 onCommandReceived: 分发、hideLoadingLayer 全部照原链路跑。
//! - `parseBufferWhenDidReadData` 每处理完一个包会 `changeStateTo:7 withMessage:@""`(0xec920)。离线网络
//!   状态机不该被回环改成"已收包",所以回环期间把这一次 state=7 吞掉。
//! - 它还会 `[UnreadPacketsDic_ removeObjectForKey:@"<sendFlag>"]`;离线真 sendPacket 在登记超时表之前就
//!   返回了,表里没有条目,sendFlag 填 0 即可,移除不存在的键是空操作。
//! - GameManager 是在 `-[GameManager startGame:]`+0x7a8 设成 delegateGameData 的;万一回环时它为空
//!   (比如从岛上回来的中间态),回环期间临时指回 GameManager,解析完恢复原值。
//!
//! # 本地数据
//! 签到/脚印兑换/海底寻宝/烟花去重等"服务器侧状态"存旁路文件 `mole_activity.dat`
//! (路径取 `-[GameData pathForDataFile:]`,与岛档同目录;`writeToFile:atomically:YES` 原子写)。
//! 坏档或缺字段一律按默认值处理,不崩溃。
//!
//! # 限时折扣 1049([补完 2026-09-15] F2-2)
//! 进村(-[GameManager startGame:])与回前台(applicationDidBecomeActive:)会发 1049。回环服务器按本地日期确定性地
//! 挑几件主村商店的纯贝壳商品打 7~8 折回包,经原版 parseDiscountList:pos:len: → addOneDiscountGood: 进
//! GameData.discountObjDataArr_,商店划线价/买得起判定/扣款全走原版。选品规则为移植者自拟,非原版数据;不落盘。
//! 原版回包后 GameManager 只弹赛尔号/中信跨游戏推广层(折扣面板 UI 在 5.5.0 已是死代码),离线吞掉这次分发。
//! MOLE_DISCOUNT=off 关闭(恢复原离线行为:无折扣)。

use crate::frameworks::foundation::ns_string;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, Ptr};
use crate::objc::{id, msg_send, nil, release, SEL};
use crate::Environment;
use digest::Digest;
use md5::Md5;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

const O: Ordering = Ordering::Relaxed;

// ───────────────────────────── 命令号(十进制,均经 re.py 核对发包点的 movw r3) ─────────────────────────────
/// 1058 getNoticeMessages(0x1cb330)→ parseNoticeMessages。
const CMD_NOTICE: u32 = 1058;
/// 1090 getDailySignExchangeInfo(0xeacec)→ parseDailySignInfo(脚印兑换表)。
const CMD_SIGN_EXCHANGE_LIST: u32 = 1090;
/// 1091 getActivityCenterInfo:target:(0xead66)→ parseActivityCenterInfo。
const CMD_ACTIVITY_CENTER: u32 = 1091;
/// 1092 getPurchaseInActivity:target:(0xeadd2)→ parsePurchaseInActivity。
const CMD_PURCHASE_IN_ACTIVITY: u32 = 1092;
/// 1112 getFireworkFlagFromServer(0x1cbc5e,复核更正:不是 1121)→ parseFireworkFlag。
const CMD_FIREWORK: u32 = 1112;
/// 1117 getdailySignDaysInfo(0xeb56c)→ parseDailySignDaysInfo。
const CMD_SIGN_DAYS: u32 = 1117;
/// 1119 getIsHasExchangedInfo(0xeb5a4)→ parseIsExchangedInfo。
const CMD_IS_EXCHANGED: u32 = 1119;
/// 1217 getOpenBoxActivitySwitchFlag(0x1ccc5c,活动预告页)。
const CMD_OPEN_BOX_SWITCH: u32 = 1217;
/// 1219 getSeabedSeekingTreasureActivityInfo(0x1cccd8)。
const CMD_SEABED_INFO: u32 = 1219;
/// 1220 seabedSeekingTreasureDigShellWith:shellType:pearlCount:(0x1ccd48)。
const CMD_SEABED_DIG: u32 = 1220;
/// 1221 seabedSeekingTreasureExchangeRewardWithPearlCount:(0x1ccd90)→ parseStatisticExchangePlayersCount。
const CMD_SEABED_EXCHANGE: u32 = 1221;
/// 1223 seabedSeekingTreasureRefreshShells(0x1cce10)。
const CMD_SEABED_REFRESH: u32 = 1223;
/// [补完 2026-09-15] F2-2 1049 getDiscountListFromServer(0x1cb160,0x1cb184 `movw r3, #0x419`)→ parseDiscountList:pos:len:
/// (parseData tbh 下标 49 → 0xe6896);GameManager onCommandReceived: 分发表 tbh@0x22ed8 下标 5 → 0x23592。
const CMD_DISCOUNT_LIST: u32 = 1049;

/// 包尾 md5 用的 16 字节盐(guest 数据段 byte_B3AE64;与私服 mole-protocol::SALT 相同)。
const SALT: [u8; 16] = [
    0x21, 0xee, 0x5e, 0x1d, 0x8b, 0xf7, 0x81, 0x57, 0x67, 0x54, 0xbe, 0x70, 0x93, 0x01, 0xff, 0xe9,
];

// ───────────────────────────── ivar 槽地址(re.py ivar 核实;偏移运行时从槽里现读) ─────────────────────────────
/// NetworkManager.buffer_(@"NSMutableData",编译期 +196)。
const SLOT_NM_BUFFER: u32 = 0xb043f0;
/// NetworkManager.packetHeader_(@"MVPacketHeader",编译期 +172)。
const SLOT_NM_PACKET_HEADER: u32 = 0xb043e4;
/// NetworkManager.delegateGameData(@,编译期 +24)。
const SLOT_NM_DELEGATE_GAMEDATA: u32 = 0xb04448;
/// MVPacketHeader.userID_(L,编译期 +16)。
const SLOT_HDR_USER_ID: u32 = 0xb0489c;
/// MVPacketHeader.deviceIDHash_(L,编译期 +24)。
const SLOT_HDR_DEVICE_HASH: u32 = 0xb048a0;
/// [补完 2026-09-15] MVPacketHeader.commandID_(L,编译期 +8)。parseData:header:pos: 把包头(r3,0xe5c5c 存 [sp,#0x34])
/// 原样作为 onCommandReceived: 的参数(0xe79e0)。
const SLOT_HDR_COMMAND_ID: u32 = 0xb04890;
/// [补完 2026-09-15] NetworkManager.state(编译期 +12;getDiscountListFromServer 0x1cb16c..0x1cb172 经 GOT 读此槽)。
const SLOT_NM_STATE: u32 = 0xb043d0;

// ───────────────────────────── 按调用点精确放行的网络门(blx 指令地址;LR = 地址+4,带 Thumb 位) ─────────────────────────────
/// -[UserInfoLayer onButtonActionFunctionsSelected:] 活动按钮 isReachable(LR 0x5a431)。
const SITE_UIL_REACHABLE: u32 = 0x5a42c;
/// 同上 isConnected(LR 0x5a451)。
const SITE_UIL_CONNECTED: u32 = 0x5a44c;
/// -[ActionLevelLayer takeLevelReward:] 领取等级礼包 isReachable(LR 0x396005)。
const SITE_ACTION_LEVEL_REACHABLE: u32 = 0x396000;
/// -[ActivityBulletinLayer showLayerWithTarget:selector:] isReachable(LR 0x3a8b73)。
const SITE_BULLETIN_REACHABLE: u32 = 0x3a8b6e;
/// 同上 isConnected(LR 0x3a8b91)。
const SITE_BULLETIN_CONNECTED: u32 = 0x3a8b8c;
/// -[DailySignLayer showWithParent:selector:] isReachable(LR 0x39715b)。
const SITE_SIGN_SHOW_REACHABLE: u32 = 0x397156;
/// 同上 isConnected(LR 0x39717d)。
const SITE_SIGN_SHOW_CONNECTED: u32 = 0x397178;
/// -[SealExchangeLayer initData] isReachable(LR 0x39aac7)。
const SITE_SEAL_INIT_REACHABLE: u32 = 0x39aac2;
/// 同上 isConnected(LR 0x39aae5)。
const SITE_SEAL_INIT_CONNECTED: u32 = 0x39aae0;
/// -[EditMenuLayer onButtonOkSelected:] 海底寻宝珍珠兑换放置确认 isReachable(LR 0x4eb1d)。
/// 门失败会弹"需要联网才能领取奖励哦!"并把刚放下的奖励删掉(0x4ebd4)。
const SITE_EDIT_SEABED_REACHABLE: u32 = 0x4eb18;
/// 同上 isConnected(LR 0x4eb3b)。
const SITE_EDIT_SEABED_CONNECTED: u32 = 0x4eb36;
/// -[SeabedSeekingTreasureMainLayer init] isReachable(LR 0x2c06c1)。两道门都过才会把自己登记成
/// NetworkManager.seabedSeekingTreasureActivityResponder;没登记时 1219 回包解析完没人接,
/// onCommandReceived: → displayUI 不跑,海底寻宝页一片空白(运行时追踪实锤)。
const SITE_SEABED_INIT_REACHABLE: u32 = 0x2c06bc;
/// 同上 isConnected(LR 0x2c06df)。
const SITE_SEABED_INIT_CONNECTED: u32 = 0x2c06da;

// ActionCenterLayer 的 ivar 槽(_OBJC_IVAR 地址,偏移从槽里读)。
/// layer_tag(+236):当前页号 1 预告 / 2 海底寻宝 / 3 邀请码 / 4 签到 / 5 活动公告 / 6 兑换码 / 7 等级礼包
/// (changeActionLayer: 分发 tbb@0x3943a6 解码)。
const SLOT_ACL_LAYER_TAG: u32 = 0xb080a0;
// [复核修 2026-09-15] leftItem(+244,向前翻/页号减小,tag 2)与 rightItem(+248,向后翻/页号增大,tag 1)的槽常量已删:
// 翻页方向改按原版的 [sender tag] 判定(0x393d62/0x393d88/0x393e28),见 skip_online_only_pages。
/// hideActivity_(+255)。
const SLOT_ACL_HIDE_ACTIVITY: u32 = 0xb08074;
/// [复核修 2026-09-15] CCNode.visible_(+40,c)与 CCNode.tag_(+216,i)的槽。-[CCNode visible]@0x2d45d8、
/// -[CCNode tag]@0x2d466c 都是直接读这两个 ivar 的平凡取值方法(CCMenuItem/CCMenuItemSprite 未覆写),读 ivar 与发消息等价。
const SLOT_CCNODE_VISIBLE: u32 = 0xb06ed4;
const SLOT_CCNODE_TAG: u32 = 0xb06ed8;

// [补完 2026-09-15] F2-2 限时折扣选品用到的 ivar 槽。re.py 核对取值方法均为平凡 ivar 读:-[GameData storeBuildingsArray]@0x8c0b8 /
// storeDecorationsArray@0x8c0c8、-[ObjectData objectId]@0x8dd30 / type@0x8dd80 / cost_gold@0x8dde0 / cost_vip_gold@0x8de00 /
// limit_count@0x8df40 / shop_type@0x8e0a0 / vip_level@0x8e1e4。偏移运行时从槽里现读。
/// GameData.storeBuildingsArray_(+624):shop_type 1 的分页数组(-[GameData parseObjectData:] 0x6f492 按 shop_sub_type 1..6 归页)。
const SLOT_GD_STORE_BUILDINGS: u32 = 0xb039a0;
/// GameData.storeDecorationsArray_(+628):shop_type 2 的分页数组(0x6f4e2)。
const SLOT_GD_STORE_DECORATIONS: u32 = 0xb039a4;
/// ObjectData.objectId_(i,+4)。
const SLOT_OBJ_ID: u32 = 0xb03c2c;
/// ObjectData.type_(C,+13)。
const SLOT_OBJ_TYPE: u32 = 0xb03c34;
/// ObjectData.cost_gold_(i,+24)。
const SLOT_OBJ_COST_GOLD: u32 = 0xb03c40;
/// ObjectData.cost_vip_gold_(i,+28)。
const SLOT_OBJ_COST_VIP_GOLD: u32 = 0xb03c44;
/// ObjectData.limit_count_(C,+68)。
const SLOT_OBJ_LIMIT_COUNT: u32 = 0xb03c6c;
/// ObjectData.shop_type_(C,+93)。
const SLOT_OBJ_SHOP_TYPE: u32 = 0xb03c98;
/// ObjectData.vip_level_(i,+132)。
const SLOT_OBJ_VIP_LEVEL: u32 = 0xb03cbc;

/// 旁路存档文件名(Documents 下,经 GameData pathForDataFile: 拼路径)。
const STATE_FILE: &str = "mole_activity.dat";

/// 系统公告的 updateTime(unix 秒,2026-09-15 00:00:00 UTC)。客户端只收 updateTime 大于已记录值的公告
/// (parseNoticeMessages 0x1bffc0),公告正文改版时把它调大即可让玩家再看到一次小星星提示。
const NOTICE_UPDATE_TIME: u32 = 1_789_430_400;

/// 回环待喂包队列。★锁绝不跨 msg_send 持有(msg_send → intercept 可能重入)。
static LOOPBACK_QUEUE: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
/// >0 表示正在回环解析(用于吞掉解析循环里的 changeStateTo:7)。
static LOOPBACK_DEPTH: AtomicU32 = AtomicU32::new(0);
/// xorshift 随机数状态(0 = 未播种)。
static RNG_STATE: AtomicU64 = AtomicU64::new(0);

// ═════════════════════════════════════════════ 对外接口 ═════════════════════════════════════════════

/// 本模块是否要拦截这个 (类, 选择子)。会被 OR 进 mole_cheats::intercept_wants,必须廉价(只做字符串比较)。
pub fn wants(class: &str, sel: &str) -> bool {
    match class {
        "NetworkManager" => matches!(
            sel,
            "sendPacket:commandId:"
                | "isReachable"
                | "isConnected"
                | "moleActivityLoopback"
                | "changeStateTo:withMessage:"
                | "sendSignDayToSure:isPatch:"
                | "getAllDaysReward"
                | "getFoodsExchangeToSure:"
                | "sendOldSignDataToServer:"
                | "seabedSeekingTreasureDigShellWith:shellType:pearlCount:"
                | "seabedSeekingTreasureExchangeRewardWithPearlCount:"
                | "seabedSeekingTreasureDigShellToGainMimiCoinWith:coinCount:"
                // [补完 2026-09-15] F2-2 限时折扣:state==4 时原版不发包的兜底
                | "getDiscountListFromServer"
        ),
        // [补完 2026-09-15] F2-2 回环喂 1049 时吞掉 GameManager 的推广弹窗分发;离线进村时补发 1049
        "GameManager" => sel == "onCommandReceived:" || sel == "startGame:",
        "UserInfoLayer" => sel == "checkActivityStatus",
        "ActionCenterLayer" => sel == "changeActionLayer:",
        "DailySignLayer" | "SealExchangeLayer" | "SeabedSeekingTreasureMainLayer" => sel == "checkNetWork",
        "GameData" => sel == "isHighPriceRecycleTime" || sel == "hasFireworkGift",
        _ => false,
    }
}

/// 前置拦截。None = 不归本模块管;Some(true) = 已吞掉调用(返回值寄存器已写好);
/// Some(false) = 做完副作用后放行真方法(若发过宿主 msg_send,返回前必须恢复 r0-r3)。
pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> Option<bool> {
    // [扫描修 2026-09-15] 回环自用的两个选择子要最先处理:performSelector 排进运行循环的
    // moleActivityLoopback 在任何状态下都必须接住(NetworkManager 并不实现它,放行会 unrecognized selector)。
    if class == "NetworkManager" {
        if sel == "moleActivityLoopback" {
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            if env.options.network_access || crate::mole_cheats::island_session_active() {
                // 在线/岛上不回环:丢弃队列(正常不会走到这里)。
                if let Ok(mut q) = LOOPBACK_QUEUE.lock() {
                    q.clear();
                }
            } else {
                run_loopback(env, nm);
            }
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        if sel == "changeStateTo:withMessage:" {
            if LOOPBACK_DEPTH.load(O) > 0 && env.cpu.regs()[2] == 7 {
                // 回环解析循环末尾的 changeStateTo:7(0xec920):离线状态机不应被改成"已收包",吞掉。
                env.cpu.regs_mut()[0] = 0;
                return Some(true);
            }
            return None;
        }
    }

    // 其余全部只在离线主村生效。
    if env.options.network_access || crate::mole_cheats::island_session_active() {
        return None;
    }

    match (class, sel) {
        // ── F3-1 / F9-1 活动中心总闸 ──
        ("UserInfoLayer", "checkActivityStatus") => {
            let uil: id = Ptr::from_bits(env.cpu.regs()[0]);
            open_action_center(env, uil);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("ActionCenterLayer", "changeActionLayer:") => {
            skip_online_only_pages(env);
            Some(false)
        }
        ("NetworkManager", "isReachable") => {
            if lr_is(env, SITE_UIL_REACHABLE)
                || lr_is(env, SITE_ACTION_LEVEL_REACHABLE)
                || lr_is(env, SITE_BULLETIN_REACHABLE)
                || lr_is(env, SITE_SIGN_SHOW_REACHABLE)
                || lr_is(env, SITE_SEAL_INIT_REACHABLE)
                || lr_is(env, SITE_EDIT_SEABED_REACHABLE)
                || lr_is(env, SITE_SEABED_INIT_REACHABLE)
            {
                env.cpu.regs_mut()[0] = 1;
                Some(true)
            } else {
                None
            }
        }
        ("NetworkManager", "isConnected") => {
            if lr_is(env, SITE_UIL_CONNECTED)
                || lr_is(env, SITE_BULLETIN_CONNECTED)
                || lr_is(env, SITE_SIGN_SHOW_CONNECTED)
                || lr_is(env, SITE_SEAL_INIT_CONNECTED)
                || lr_is(env, SITE_EDIT_SEABED_CONNECTED)
                || lr_is(env, SITE_SEABED_INIT_CONNECTED)
            {
                env.cpu.regs_mut()[0] = 1;
                Some(true)
            } else {
                None
            }
        }
        // 三个层自己的 checkNetWork(签到层 0x39a3fc / 脚印兑换层 0x39c7d4 / 海底寻宝层 0x2c28b8):
        // 原版在无网时弹"该功能需要联网"或 showNetWorkError 并返回 NO。离线由回环服务器代答,直接返回 YES。
        ("DailySignLayer", "checkNetWork")
        | ("SealExchangeLayer", "checkNetWork")
        | ("SeabedSeekingTreasureMainLayer", "checkNetWork") => {
            env.cpu.regs_mut()[0] = 1;
            Some(true)
        }

        // ── 回环服务器主入口 ──
        ("NetworkManager", "sendPacket:commandId:") => {
            // [复核修 2026-09-15] R4-1 兜底:handle_send_packet 返回 None(放行真 sendPacket)时若之前发过宿主消息,
            //   寄存器已被改写;真方法在判 isReachable_ 之前就写 self+204、给 self+172 发 setSendFlag:(0xe235e/0xe2362),
            //   self 错了必坏堆。这里统一快照,None 时恢复。
            let saved = save_regs(env);
            let nm: id = Ptr::from_bits(saved[0]);
            let body: id = Ptr::from_bits(saved[2]);
            let cmd = saved[3];
            let r = handle_send_packet(env, nm, body, cmd);
            if r.is_none() {
                restore_regs(env, saved);
            }
            r
        }

        // ── [补完 2026-09-15] F2-2 限时折扣 1049 ──
        ("GameManager", "onCommandReceived:") => {
            // 只管回环喂进来的 1049;其它命令、非回环一律放行。这里只读寄存器与 guest 内存,不发宿主消息,放行时寄存器未动。
            if LOOPBACK_DEPTH.load(O) == 0 {
                return None;
            }
            let header: id = Ptr::from_bits(env.cpu.regs()[2]);
            if read_ivar_u32(env, header, SLOT_HDR_COMMAND_ID) != Some(CMD_DISCOUNT_LIST) {
                return None;
            }
            // 1049 臂(0x23592..0x23d80)只做一件事:[[GameData sharedInstance] isOpenGreatRewardLayer] 为真 →
            // [[AutoPopZhongXinLayer shareInstance] open](0x235ec),否则 purge 后 [[DiscountInfoLayer sharedInstance] show]
            // (0x23d7e),两路都经 0x2265a 落到函数收尾 0x22efa(只有栈保护检查)。5.5.0 的 DiscountInfoLayer 已改成赛尔号推广层:
            // init@0x1eb234 只摆 seer_bg_back / seer_button_join→onButtonLinkToItunesSiteOfIseer / seer_button_off;折扣列表
            // UI(showDiscountObjects 只被无 selref 的 onButtonLeft/onButtonRight 调用,dTable 从不创建)是死代码。
            // 原版每收到一次 1049 回包就弹一次推广;离线不弹跨游戏推广(与 mole_cheats 吞 AutoPopZhongXinLayer open 同一口径)。
            // 折扣数据在此之前已由 parseDiscountList:pos:len: 写进 GameData,吞掉分发不影响商店价格。
            log!("[ACTIVITY] 限时折扣 cmd=1049:吞掉 GameManager 分发(原版此臂只弹赛尔号/中信推广层,折扣数据已入库)");
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("GameManager", "startGame:") => {
            // [补完 2026-09-15] F2-2 实测纠正:-[GameManager startGame:] 在 0x1992a 先查 [NetworkManager isConnected],
            // 为假就跳到 0x19e18,整段服务器同步(getFriendsInfo:/getGiftsFromServer/getAmendVIPGoldFromServer/
            // 0x19a56 getDiscountListFromServer)都不执行;回前台那处(0x10f38)又要求 InGameScene 且中信奖励类型≥2。
            // 所以离线主村原版永远不会发 1049。这里在进村时(前置,商店数组已由 load:type: 加载好)照原版同一个入口
            // 补发一次:[[NetworkManager sharedInstance] getDiscountListFromServer] → sendPacket:1049 → 回环应答;
            // 回包排到运行循环再喂,那时 startGame: 已设好 delegateGameData。发过宿主消息,放行前恢复 r0-r3。
            if !discount_disabled() {
                let saved = save_regs(env);
                let nm = singleton(env, "NetworkManager", "sharedInstance");
                if nm != nil {
                    let get_list = sel_named(env, "getDiscountListFromServer");
                    let _: () = msg_send(env, (nm, get_list));
                }
                restore_regs(env, saved);
            }
            None
        }
        ("NetworkManager", "getDiscountListFromServer") => {
            // 原版 0x1cb174:state==4 直接返回不发包;其它状态走 sendPacket:commandId:1049,由 handle_send_packet 应答。
            // 离线状态机按理到不了 4,这里只是兜底(state==4 时照样本地应答并记日志)。放行路径只读内存,寄存器未动。
            if discount_disabled() {
                return None;
            }
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            if read_ivar_u32(env, nm, SLOT_NM_STATE) != Some(4) {
                return None;
            }
            log!("[ACTIVITY] 限时折扣:NetworkManager.state==4,原版不会发 1049,本地兜底应答");
            let body = encode_discount_list(env);
            enqueue_reply(env, nm, CMD_DISCOUNT_LIST, body);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }

        // ── F3-3 / F5-4 每日签到(需要参数的上层发包方法) ──
        ("NetworkManager", "sendSignDayToSure:isPatch:") => {
            let day = env.cpu.regs()[2];
            let is_patch = (env.cpu.regs()[3] & 0xff) != 0;
            sign_day(env, day, is_patch);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("NetworkManager", "getAllDaysReward") => {
            sign_full_attendance_reward(env);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("NetworkManager", "getFoodsExchangeToSure:") => {
            let index = env.cpu.regs()[2];
            sign_exchange_confirm(env, index);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("NetworkManager", "sendOldSignDataToServer:") => {
            let info: id = Ptr::from_bits(env.cpu.regs()[2]);
            sign_migrate_legacy(env, info);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }

        // ── F3-4 海底寻宝 ──
        ("NetworkManager", "seabedSeekingTreasureDigShellWith:shellType:pearlCount:") => {
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            let pos = env.cpu.regs()[2];
            let shell_type = env.cpu.regs()[3];
            seabed_dig(env, nm, pos, shell_type);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("NetworkManager", "seabedSeekingTreasureExchangeRewardWithPearlCount:") => {
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            let cost = env.cpu.regs()[2];
            seabed_exchange(env, nm, cost);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("NetworkManager", "seabedSeekingTreasureDigShellToGainMimiCoinWith:coinCount:") => {
            // 1222「挖到米币→输米米号领取」是真钱通道,离线屏蔽(不发包、不给任何东西)。
            // 本模块生成的贝壳表里没有米币类型,正常不会走到这里;万一走到只记日志。
            log!(
                "[ACTIVITY] 屏蔽 1222 米币领取(离线无淘米账户,米币通道关闭) mimi={} coin={}",
                env.cpu.regs()[2],
                env.cpu.regs()[3]
            );
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }

        // ── F2-6 废品站高价回收(圣诞窗口) ──
        ("GameData", "isHighPriceRecycleTime") => {
            // 复核结论:不要去写 ivar(changeStateTo:8 会清零,0xe1042),改拦 getter;四个读取点
            // (JunkShopLayer init / NpcPrompt / OscarDialogueLayer / GameManager onCommandReceived:)自然走原版分支。
            // [复核修 2026-09-15] R4-2:festival_today → local_date 发过宿主消息 [NSTimeZone systemTimeZone],r0/r1 已被改写;
            //   非圣诞窗口放行真 getter@0x8b788(`ldrsb r0, [r0, r1]`,偏移取自槽 0xb038f0)之前必须恢复,否则它以
            //   NSTimeZone 对象为 self 读越界字节,废品站/奥斯卡对话随机进入高价回收分支。
            let saved = save_regs(env);
            let (fest, _) = festival_today(env);
            if fest == Festival::Xmas {
                env.cpu.regs_mut()[0] = 1;
                Some(true)
            } else {
                restore_regs(env, saved);
                None
            }
        }
        // ── F3-12 烟花礼物 ──
        ("GameData", "hasFireworkGift") => {
            // parseFireworkFlag 用同一个字节同时写 showFirework/hasFireworkGift,分不开;而礼物本身是
            // -[FireworkLayer fireWorkDone]@0x3e3668 再发 1071 由服务器补发的。离线没有可信的补发清单,
            // 不自造奖励,也不弹"看你的脚下,我们给你留下了神秘的礼物!"这种骗人的提示 → 离线恒返回 NO。
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        _ => None,
    }
}

// ═════════════════════════════════════════════ 小工具 ═════════════════════════════════════════════

fn sel_named(env: &mut Environment, name: &str) -> SEL {
    env.objc.register_host_selector(name.to_string(), &mut env.mem)
}

/// 调用方返回地址是否正好是某条 blx 指令之后(LR = blx 地址 + 4,比较前去掉 Thumb 位)。
fn lr_is(env: &Environment, blx_site: u32) -> bool {
    (env.cpu.regs()[14] & !1u32) == blx_site + 4
}

/// [复核修 2026-09-15] 快照 r0-r3:发过宿主 msg_send 之后又要放行真方法时,返回前用 restore_regs 恢复。
fn save_regs(env: &Environment) -> [u32; 4] {
    let r = env.cpu.regs();
    [r[0], r[1], r[2], r[3]]
}

fn restore_regs(env: &mut Environment, saved: [u32; 4]) {
    env.cpu.regs_mut()[0..4].copy_from_slice(&saved);
}

/// 从 guest 的 _OBJC_IVAR 槽读偏移,再读对象里该偏移处的 u32。偏移异常时返回 None。
fn read_ivar_u32(env: &Environment, obj: id, slot: u32) -> Option<u32> {
    if obj == nil {
        return None;
    }
    let off_ptr: ConstPtr<u32> = Ptr::from_bits(slot);
    let off: u32 = env.mem.read(off_ptr);
    if off == 0 || off > 0x1000 {
        return None;
    }
    let p: ConstPtr<u32> = Ptr::from_bits(obj.to_bits().wrapping_add(off));
    Some(env.mem.read(p))
}

/// 同 read_ivar_u32,读 1 字节(ObjC BOOL/char ivar)。
fn read_ivar_u8(env: &Environment, obj: id, slot: u32) -> Option<u8> {
    if obj == nil {
        return None;
    }
    let off_ptr: ConstPtr<u32> = Ptr::from_bits(slot);
    let off: u32 = env.mem.read(off_ptr);
    if off == 0 || off > 0x1000 {
        return None;
    }
    let p: ConstPtr<u8> = Ptr::from_bits(obj.to_bits().wrapping_add(off));
    Some(env.mem.read(p))
}

fn write_ivar_u32(env: &mut Environment, obj: id, slot: u32, value: u32) -> bool {
    if obj == nil {
        return false;
    }
    let off_ptr: ConstPtr<u32> = Ptr::from_bits(slot);
    let off: u32 = env.mem.read(off_ptr);
    if off == 0 || off > 0x1000 {
        return false;
    }
    let p: MutPtr<u32> = Ptr::from_bits(obj.to_bits().wrapping_add(off));
    env.mem.write(p, value);
    true
}

/// NSData → 宿主字节(nil/空返回空 Vec)。
fn nsdata_bytes(env: &mut Environment, data: id) -> Vec<u8> {
    if data == nil {
        return Vec::new();
    }
    let len_sel = sel_named(env, "length");
    let len: GuestUSize = msg_send(env, (data, len_sel));
    if len == 0 {
        return Vec::new();
    }
    let bytes_sel = sel_named(env, "bytes");
    // 宿主实现的 -bytes 返回 ConstVoidPtr,类型必须精确匹配。
    let ptr: ConstVoidPtr = msg_send(env, (data, bytes_sel));
    if ptr.is_null() {
        return Vec::new();
    }
    env.mem.bytes_at(ptr.cast(), len).to_vec()
}

/// `[[Class sharedInstance/sharedManager] ...]` 取单例。
fn singleton(env: &mut Environment, class_name: &str, sel_name: &str) -> id {
    let cls = env.objc.get_known_class(class_name, &mut env.mem);
    if cls == nil {
        return nil;
    }
    let s = sel_named(env, sel_name);
    msg_send(env, (cls, s))
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

/// 长度前缀 UTF-8 串,按字符边界截到 max 字节以内(照抄私服 activity.rs put_lp_str)。
fn put_lp_str(b: &mut Vec<u8>, s: &str, max: usize) {
    let bytes = s.as_bytes();
    let mut n = bytes.len().min(max);
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    put_u32(b, n as u32);
    b.extend_from_slice(&bytes[..n]);
}

/// 组完整包:[24B 头][body][16B md5(头 ++ body ++ 盐)](照抄私服 mole-protocol::encode_packet)。
fn build_packet(cmd: u32, user_id: u32, device_hash: u32, body: &[u8]) -> Vec<u8> {
    let packet_len = (24 + body.len() + 16) as u32;
    let mut out = Vec::with_capacity(packet_len as usize);
    // 头字段顺序:packetLen, commandID, sendFlag, userID, errorID(@0x10), deviceIDHash
    for v in [packet_len, cmd, 0u32, user_id, 0u32, device_hash] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(body);
    let mut h = Md5::new();
    h.update(&out);
    h.update(SALT);
    let digest = h.finalize();
    out.extend_from_slice(digest.as_slice());
    out
}

/// 回环包入队并排一次运行循环回调。
fn enqueue_reply(env: &mut Environment, nm: id, cmd: u32, body: Vec<u8>) {
    if nm == nil {
        log!("[ACTIVITY] 回环放弃 cmd={}:NetworkManager 为 nil", cmd);
        return;
    }
    // 头里的 userID/deviceIDHash 抄 NetworkManager.packetHeader_ 当前值(和真服务器回显一致);直接读 ivar,
    // 不对 MVPacketHeader 发消息,避免碰到别的模块的钩子。
    let header: id = match read_ivar_u32(env, nm, SLOT_NM_PACKET_HEADER) {
        Some(bits) => Ptr::from_bits(bits),
        None => nil,
    };
    let user_id = read_ivar_u32(env, header, SLOT_HDR_USER_ID).unwrap_or(0);
    let device_hash = read_ivar_u32(env, header, SLOT_HDR_DEVICE_HASH).unwrap_or(0);
    let pkt = build_packet(cmd, user_id, device_hash, &body);
    log!(
        "[ACTIVITY] 回环入队 cmd={} body_len={} packet_len={}",
        cmd,
        body.len(),
        pkt.len()
    );
    if let Ok(mut q) = LOOPBACK_QUEUE.lock() {
        q.push(pkt);
    } else {
        return;
    }
    // 不在发包的调用栈里同步解析(那样回包处理会早于调用方后续的 showLoadingLayer 等),
    // 排到运行循环的 perform 相位再喂,时序与真网络回包一致。
    let perform = sel_named(env, "performSelector:withObject:afterDelay:");
    let tick = sel_named(env, "moleActivityLoopback");
    let _: () = msg_send(env, (nm, perform, tick, nil, 0.0f64));
}

/// 运行循环安全点:把队列里的包追加进 buffer_ 并调原版解析。
fn run_loopback(env: &mut Environment, nm: id) {
    let packets: Vec<Vec<u8>> = match LOOPBACK_QUEUE.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    if packets.is_empty() || nm == nil {
        return;
    }
    let buffer: id = match read_ivar_u32(env, nm, SLOT_NM_BUFFER) {
        Some(bits) => Ptr::from_bits(bits),
        None => nil,
    };
    if buffer == nil {
        log!(
            "[ACTIVITY] 回环放弃:NetworkManager.buffer_ 为 nil(丢弃 {} 个包)",
            packets.len()
        );
        return;
    }
    let append = sel_named(env, "appendBytes:length:");
    for pkt in &packets {
        let len = pkt.len() as GuestUSize;
        let ptr: MutPtr<u8> = env.mem.alloc(len).cast();
        env.mem.bytes_at_mut(ptr, len).copy_from_slice(pkt);
        let _: () = msg_send(env, (buffer, append, ptr.cast_const(), len));
        env.mem.free(ptr.cast());
        let cmd = u32::from_le_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
        log!("[ACTIVITY] 回环喂包 cmd={} len={}", cmd, len);
    }

    // delegateGameData 兜底:为空时临时指向 GameManager,解析完恢复。
    let mut patched_delegate = false;
    if read_ivar_u32(env, nm, SLOT_NM_DELEGATE_GAMEDATA) == Some(0) {
        let gm = singleton(env, "GameManager", "sharedManager");
        if gm != nil && write_ivar_u32(env, nm, SLOT_NM_DELEGATE_GAMEDATA, gm.to_bits()) {
            patched_delegate = true;
            log!("[ACTIVITY] 回环期间临时把 delegateGameData 指向 GameManager");
        }
    }

    LOOPBACK_DEPTH.fetch_add(1, O);
    let parse = sel_named(env, "parseBufferWhenDidReadData");
    let _: () = msg_send(env, (nm, parse));
    LOOPBACK_DEPTH.fetch_sub(1, O);

    if patched_delegate {
        write_ivar_u32(env, nm, SLOT_NM_DELEGATE_GAMEDATA, 0);
    }
}

// ─────────────────────────────── 时间与节日 ───────────────────────────────

/// 当前 CFAbsoluteTime 整秒(与离线 getCurrentServerTime、NSDate 同一口径,含时间旅行偏移)。
fn now_cf_u32() -> u32 {
    let cf = crate::frameworks::core_foundation::time::cf_absolute_time_now();
    if cf.is_finite() && cf > 0.0 {
        cf.min(u32::MAX as f64) as u32
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug)]
struct LocalDate {
    year: i32,
    month: u32,
    day: u32,
}

impl LocalDate {
    fn ym(&self) -> u32 {
        (self.year.max(0) as u32) * 100 + self.month
    }
    fn ymd(&self) -> u32 {
        self.ym() * 100 + self.day
    }
}

/// 本地日期。时区取 [NSTimeZone systemTimeZone](默认北京时间,MOLE_TZ=host 跟随宿主),
/// 与 -[DailySignLayer getServerTime] 用 CFTimeZoneCopySystem 拆日期的口径一致。
fn local_date(env: &mut Environment) -> LocalDate {
    let cf = crate::frameworks::core_foundation::time::cf_absolute_time_now();
    let cf = if cf.is_finite() { cf } else { 0.0 };
    let unix = cf.floor() as i64 + 978_307_200;
    let tz_cls = env.objc.get_known_class("NSTimeZone", &mut env.mem);
    let offset: i64 = if tz_cls != nil {
        let s = sel_named(env, "systemTimeZone");
        let tz: id = msg_send(env, (tz_cls, s));
        crate::frameworks::foundation::ns_time_zone::seconds_from_gmt_at_unix(env, tz, unix) as i64
    } else {
        8 * 3600
    };
    let (year, month, day) = civil_from_days((unix + offset).div_euclid(86_400));
    LocalDate { year, month, day }
}

/// 自 1970-01-01 的天数 → (年, 月, 日)(Howard Hinnant 算法)。
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
}

/// (年, 月, 日) → 自 1970-01-01 的天数。
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (if month > 2 { month - 3 } else { month + 9 }) as u64;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Festival {
    Off,
    Spring,
    Xmas,
}

/// 春节(农历正月初一)的公历日期表 2024–2040。表外年份不开春节窗口。
const SPRING_FESTIVAL: [(i32, u32, u32); 17] = [
    (2024, 2, 10),
    (2025, 1, 29),
    (2026, 2, 17),
    (2027, 2, 6),
    (2028, 1, 26),
    (2029, 2, 13),
    (2030, 2, 3),
    (2031, 1, 23),
    (2032, 2, 11),
    (2033, 1, 31),
    (2034, 2, 19),
    (2035, 2, 8),
    (2036, 1, 28),
    (2037, 2, 15),
    (2038, 2, 4),
    (2039, 1, 24),
    (2040, 2, 12),
];

/// 今天是否处于节日窗口。春节窗口 = 除夕(初一前 1 天)到元宵(初一后 14 天);圣诞窗口 = 12-20 ~ 12-31。
/// 原版活动的确切日期本地没有依据(服务器下发),窗口为移植者自定。
/// 测试用环境变量 MOLE_FESTIVAL=spring|xmas|off 可强制指定。
fn festival_today(env: &mut Environment) -> (Festival, LocalDate) {
    let today = local_date(env);
    if let Ok(v) = std::env::var("MOLE_FESTIVAL") {
        match v.trim().to_ascii_lowercase().as_str() {
            "spring" => return (Festival::Spring, today),
            "xmas" | "christmas" => return (Festival::Xmas, today),
            "off" | "none" => return (Festival::Off, today),
            _ => {}
        }
    }
    if today.month == 12 && today.day >= 20 {
        return (Festival::Xmas, today);
    }
    let t = days_from_civil(today.year, today.month, today.day);
    for &(y, m, d) in SPRING_FESTIVAL.iter() {
        let cny = days_from_civil(y, m, d);
        if t >= cny - 1 && t <= cny + 14 {
            return (Festival::Spring, today);
        }
    }
    (Festival::Off, today)
}

fn rand_u32() -> u32 {
    let mut x = RNG_STATE.load(O);
    if x == 0 {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        x = seed | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RNG_STATE.store(x, O);
    (x >> 32) as u32
}

// ─────────────────────────────── 旁路存档 ───────────────────────────────

/// 本模块扮演"服务器"时需要记住的状态。
#[derive(Clone, Debug)]
struct ActState {
    /// 签到所属月份 yyyymm。
    sign_month: u32,
    /// 签到位图:bit d = 第 d 天已踩(1..=31);bit0 不用(回包里 bit0 表示全勤奖已领)。
    sign_days: u32,
    /// 脚印总数(跨月累计,永久有效)。
    sign_foot: u32,
    /// 本月已补签次数。
    sign_patch: u32,
    /// 本月全勤奖是否已领(0/1)。
    sign_reward: u32,
    /// 脚印兑换记录所属月份 yyyymm。
    exch_month: u32,
    /// 脚印兑换已兑位图(bit i = 第 i+1 项,parseIsExchangedInfo 读低 12 位)。
    exch_mask: u32,
    /// 海底寻宝:珍珠数。
    pearl: u32,
    /// 海底寻宝:累计挖贝次数(海王贝概率随之上升;>0 时 bDigShellPlayers=1)。
    dug: u32,
    /// 海底寻宝:5 个贝壳 (pearlType, lastTimestamp[CFAbsoluteTime 秒])。
    shells: Vec<(u32, u32)>,
    /// 节日烟花最近一次放的日期 yyyymmdd(每天最多一次)。
    firework_day: u32,
}

impl Default for ActState {
    fn default() -> Self {
        ActState {
            sign_month: 0,
            sign_days: 0,
            sign_foot: 0,
            sign_patch: 0,
            sign_reward: 0,
            exch_month: 0,
            exch_mask: 0,
            pearl: 0,
            dug: 0,
            shells: Vec::new(),
            firework_day: 0,
        }
    }
}

impl ActState {
    fn serialize(&self) -> String {
        let shells: Vec<String> = self
            .shells
            .iter()
            .map(|(t, ts)| format!("{},{}", t, ts))
            .collect();
        format!(
            "v=1\nsign_month={}\nsign_days={}\nsign_foot={}\nsign_patch={}\nsign_reward={}\nexch_month={}\nexch_mask={}\npearl={}\ndug={}\nshells={}\nfirework_day={}\n",
            self.sign_month,
            self.sign_days,
            self.sign_foot,
            self.sign_patch,
            self.sign_reward,
            self.exch_month,
            self.exch_mask,
            self.pearl,
            self.dug,
            shells.join(";"),
            self.firework_day
        )
    }

    /// 宽松解析:认不出的行/值一律忽略,保持默认。
    fn parse(text: &str) -> ActState {
        let mut st = ActState::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            let num = || v.parse::<u32>().ok();
            match k.trim() {
                "sign_month" => st.sign_month = num().unwrap_or(0),
                "sign_days" => st.sign_days = num().unwrap_or(0) & !1u32,
                "sign_foot" => st.sign_foot = num().unwrap_or(0).min(1_000_000),
                "sign_patch" => st.sign_patch = num().unwrap_or(0).min(31),
                "sign_reward" => st.sign_reward = u32::from(num().unwrap_or(0) != 0),
                "exch_month" => st.exch_month = num().unwrap_or(0),
                "exch_mask" => st.exch_mask = num().unwrap_or(0) & 0xfff,
                "pearl" => st.pearl = num().unwrap_or(0).min(1_000_000),
                "dug" => st.dug = num().unwrap_or(0),
                "firework_day" => st.firework_day = num().unwrap_or(0),
                "shells" => {
                    let mut shells = Vec::new();
                    for item in v.split(';') {
                        if let Some((t, ts)) = item.split_once(',') {
                            if let (Ok(t), Ok(ts)) = (t.trim().parse::<u32>(), ts.trim().parse::<u32>()) {
                                if (1..=4).contains(&t) {
                                    shells.push((t, ts));
                                }
                            }
                        }
                    }
                    if shells.len() == 5 {
                        st.shells = shells;
                    }
                }
                _ => {}
            }
        }
        st
    }
}

/// 旁路档完整 guest 路径(NSString,autoreleased;失败返回 nil)。
fn state_path(env: &mut Environment) -> id {
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return nil;
    }
    let pfd = sel_named(env, "pathForDataFile:");
    let fname = ns_string::from_rust_string(env, STATE_FILE.to_string());
    let path: id = msg_send(env, (gd, pfd, fname));
    // pathForDataFile:@0x75374 不保存参数,+1 临时串用完即释放(同 mole_cheats island_data_path)。
    release(env, fname);
    path
}

fn load_state(env: &mut Environment) -> ActState {
    let path = state_path(env);
    if path == nil {
        return ActState::default();
    }
    let data_cls = env.objc.get_known_class("NSData", &mut env.mem);
    let s = sel_named(env, "dataWithContentsOfFile:");
    let data: id = msg_send(env, (data_cls, s, path));
    if data == nil {
        return ActState::default();
    }
    let bytes = nsdata_bytes(env, data);
    ActState::parse(&String::from_utf8_lossy(&bytes))
}

fn save_state(env: &mut Environment, st: &ActState) {
    let path = state_path(env);
    if path == nil {
        log!("[ACTIVITY] 存档失败:拿不到 {} 的路径", STATE_FILE);
        return;
    }
    let text = st.serialize();
    let bytes = text.as_bytes();
    let len = bytes.len() as GuestUSize;
    let ptr: MutPtr<u8> = env.mem.alloc(len).cast();
    env.mem.bytes_at_mut(ptr, len).copy_from_slice(bytes);
    let data_cls = env.objc.get_known_class("NSData", &mut env.mem);
    let dwb = sel_named(env, "dataWithBytes:length:");
    let vptr: ConstVoidPtr = ptr.cast_const().cast();
    let data: id = msg_send(env, (data_cls, dwb, vptr, len));
    env.mem.free(ptr.cast());
    if data == nil {
        log!("[ACTIVITY] 存档失败:NSData 创建失败");
        return;
    }
    let w = sel_named(env, "writeToFile:atomically:");
    let ok: bool = msg_send(env, (data, w, path, true));
    log!("[ACTIVITY] 存盘 {}(ok={})", STATE_FILE, ok);
}

// ═════════════════════════════════════════════ 业务 ═════════════════════════════════════════════

/// F3-1/F9-1:活动中心总闸。等价原版超时臂(onGetActivityStatusTimeOut@0x5caa4:getChildByTag:0x16 →
/// setShowForecast: → showActionCenterLayer),但按复核更正:
/// - setShowForecast:**1**(预告页作为安全落地页;设 0 会在 initFirstFunctionLayer@0x39365c 走到邀请码页,
///   弹"无法获取当前的活动中心数据"后立即关闭);
/// - setHideCodeLayer:**0**。★纠错(2026-09-15 反汇编 + 运行时追踪):原版这个开关的跳页位置是错的——
///   向后翻时它在 3→4 处多跳一页(0x393f82,跳过的是**签到页**),向前翻时在 7→6 处多跳(0x39416c,跳过兑换码页),
///   邀请码页(3)照样出现并弹"无法获取当前的活动中心数据"。所以这里关掉它,改由 skip_online_only_pages
///   在翻页前按方向改写 layer_tag,两个方向都跳过邀请码页(3)和兑换码页(6);
/// - 原版在 1181 回包后还会经 checkVoteStatus 发 1219 置 seabedSeekingTreasureActivityFlag;这里直接置 1
///   (5.5.0 停运时海底寻宝是常驻活动),翻页才会包含海底寻宝页;
/// - 活动公告表为空时原版向后翻会跳过公告页(0x394006)。1091 由 startGame:(0x19d3c)与从好友家回村(0x10918e)
///   发出,离线经回环服务器应答;表仍为空时(比如回环包被丢弃)照同样的参数补发一次 getActivityCenterInfo:1 target:nil。
///   [复核修 2026-09-15] getActivityCenterInfo:target:@0xeacfc 的请求体 =(参数==10)?1:0(0xead30/0xead34),
///   这些发包点和补发都传 1 → body=0=主村,回环回本地活动表(见 handle_send_packet)。
fn open_action_center(env: &mut Environment, uil: id) {
    let scene = singleton(env, "InGameScene", "scene");
    if scene != nil {
        let gct = sel_named(env, "getChildByTag:");
        let acl: id = msg_send(env, (scene, gct, 0x16i32));
        if acl != nil {
            let s1 = sel_named(env, "setShowForecast:");
            let _: () = msg_send(env, (acl, s1, true));
            let s2 = sel_named(env, "setHideCodeLayer:");
            let _: () = msg_send(env, (acl, s2, false));
        } else {
            log!("[ACTIVITY] 活动中心:InGameScene 里没有 tag 0x16 的 ActionCenterLayer");
        }
    }
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd != nil {
        let get_flag = sel_named(env, "seabedSeekingTreasureActivityFlag");
        let flag: i32 = msg_send(env, (gd, get_flag));
        if flag == 0 {
            let set_flag = sel_named(env, "setSeabedSeekingTreasureActivityFlag:");
            let _: () = msg_send(env, (gd, set_flag, 1i32));
        }
    }
    if activities_count(env) == 0 {
        let nm = singleton(env, "NetworkManager", "sharedInstance");
        if nm != nil {
            let get_info = sel_named(env, "getActivityCenterInfo:target:");
            let _: () = msg_send(env, (nm, get_info, 1i32, nil));
        }
    }
    if uil != nil {
        let show = sel_named(env, "showActionCenterLayer");
        let _: () = msg_send(env, (uil, show));
    }
    log!("[ACTIVITY] 活动中心总闸:离线直开(showForecast=1 hideCodeLayer=0 seabedFlag=1)");
}

/// `[[GameData sharedInstance] activitiesInfoDataArray] count`(数组为 nil 时 0)。
fn activities_count(env: &mut Environment) -> GuestUSize {
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return 0;
    }
    let get_arr = sel_named(env, "activitiesInfoDataArray");
    let arr: id = msg_send(env, (gd, get_arr));
    if arr == nil {
        return 0;
    }
    let count_sel = sel_named(env, "count");
    msg_send(env, (arr, count_sel))
}

/// -[ActionCenterLayer changeActionLayer:]@0x393d24 前置:按翻页方向预先改写 layer_tag,让原版自己的 ±1 逻辑
/// 落到想要的页上,跳过离线用不了的邀请码页(3,要服务器发码)和兑换码页(6,要服务器验码)。
/// 页号(分发 tbb@0x3943a6,下标 layer_tag-1):1 预告 / 2 海底寻宝 / 3 邀请码 / 4 签到 / 5 活动公告 / 6 兑换码 / 7 等级礼包。
/// 原版先 `[sender visible]`(0x393d7c,不可见直接退出 0x3945d8、不翻页),再按 `[sender tag]` 分方向(0x393d88 / 0x393e28)。
/// [复核修 2026-09-15] 两张 tbh 表按指令编码独立复算(hideCodeLayer=0;isLowMemoryDevice@0x184914 只认 platformString
/// "iPod Touch 4G"/"iPad",touchHLE 的 uname 机型是 iPhone1,1 → NO):
/// - 向后(tag 1 = rightItem,tbh@0x393e48 按 layer_tag-1 索引):1→2(海底寻宝标志为 0/3 或 hideActivity 时 →3)、
///   2→3、3→4、4→5(活动表空时 →6)、5→6、6→7、7 不动;
/// - 向前(tag 2 = leftItem,tbh@0x393da0 按 layer_tag 索引):7→6、6→5、5→4、4→3、
///   3→2(海底寻宝标志为 0/3 或 hideActivity 时:showForecast 为真 →1,否则不动)、2→1(showForecast 为假时不动)、1 不动。
/// 所以:向后在 1(会落 3 时)/2 预置 3(→4)、在 4 且活动表空时预置 6(→7)、在 5 预置 6(→7);
/// 向前在 4 预置 3(→2/1)、在 7 预置 6(→5)。离线入口 open_action_center 恒设 showForecast=1,向前从 3 一定落到 2 或 1。
/// 预置值都在 1..=7 内,不会越界;只改一个 int ivar,真方法照常执行(发过 msg_send,返回前恢复 r0-r3)。
fn skip_online_only_pages(env: &mut Environment) {
    let saved = save_regs(env);
    let acl: id = Ptr::from_bits(saved[0]);
    let sender: id = Ptr::from_bits(saved[2]);
    preset_layer_tag(env, acl, sender);
    restore_regs(env, saved);
}

/// skip_online_only_pages 的主体(不管寄存器,由调用方统一恢复)。
fn preset_layer_tag(env: &mut Environment, acl: id, sender: id) {
    if sender == nil {
        return;
    }
    let Some(tag) = read_ivar_u32(env, acl, SLOT_ACL_LAYER_TAG) else {
        return;
    };
    // [复核修 2026-09-15] 方向与可见性照原版取。原先按 leftItem/rightItem 指针判方向且不看 visible:
    //   sender 不可见时原版直接退出不翻页,预置值却已写进 layer_tag,页码与正在显示的页错位。
    //   原版 0x393d7c `[sender visible]` + 0x393d80 `tst.w r0, #0xff`、0x393d62 `[sender tag]`;两个取值方法都是
    //   平凡 ivar 读(见 SLOT_CCNODE_*),这里直接读 ivar,公共路径不发任何宿主消息。
    if read_ivar_u8(env, sender, SLOT_CCNODE_VISIBLE).unwrap_or(0) == 0 {
        return;
    }
    let Some(item_tag) = read_ivar_u32(env, sender, SLOT_CCNODE_TAG) else {
        return;
    };
    let forward = item_tag == 1;
    let backward = item_tag == 2;

    let preset = if forward {
        match tag {
            1 => {
                let gd = singleton(env, "GameData", "sharedInstance");
                let flag_sel = sel_named(env, "seabedSeekingTreasureActivityFlag");
                let flag: i32 = if gd == nil { 0 } else { msg_send(env, (gd, flag_sel)) };
                let hide_activity = read_ivar_u8(env, acl, SLOT_ACL_HIDE_ACTIVITY).unwrap_or(0) != 0;
                if flag == 0 || flag == 3 || hide_activity {
                    Some(3)
                } else {
                    None
                }
            }
            2 | 3 => Some(3),
            4 if activities_count(env) == 0 => Some(6),
            5 | 6 => Some(6),
            _ => None,
        }
    } else if backward {
        match tag {
            3 | 4 => Some(3),
            6 | 7 => Some(6),
            _ => None,
        }
    } else {
        None
    };

    if let Some(new_tag) = preset {
        if new_tag != tag && write_ivar_u32(env, acl, SLOT_ACL_LAYER_TAG, new_tag) {
            log!(
                "[ACTIVITY] 活动中心翻页({}):layer_tag {}→{} 预置,跳过离线不可用的邀请码/兑换码页",
                if forward { "向后" } else { "向前" },
                tag,
                new_tag
            );
        }
    }
}

/// 回环白名单分派。返回 None 表示不认识这个命令号(放行真 sendPacket,离线等于空过)。
/// [复核修 2026-09-15] 调用处在 None 时会恢复 r0-r3;但新增分支仍应"发过宿主消息就吞掉",不要依赖兜底。
fn handle_send_packet(env: &mut Environment, nm: id, body: id, cmd: u32) -> Option<bool> {
    match cmd {
        CMD_OPEN_BOX_SWITCH => {
            // F3-8:活动预告页的开宝箱/港游开关。内容已空心化(对应层无入口且图集缺失),不注入,
            // 只显式吞掉并记日志;预告页本身不等回包,不会卡 loading。
            log!("[ACTIVITY] 吞掉 cmd=1217(活动预告开宝箱开关,离线不注入) len=0");
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_NOTICE => {
            enqueue_reply(env, nm, cmd, encode_notices());
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_ACTIVITY_CENTER => {
            // [复核修 2026-09-15] getActivityCenterInfo:target:@0xeacfc:参数既非 1 也非 10 直接返回不发包
            //   (0xead0c/0xead16/0xead18),请求体 4 字节 =(参数==10)?1:0(0xead1e/0xead30/0xead34/0xead38),
            //   与私服「0主/1岛」一致。三个发包点 startGame:(0x19d30)、FriendsVillageLayer 回家(0x109182)、
            //   ActivityBulletinLayer showLayerWithTarget:selector:(0x3a8bc4)都传 1 → body=0=主村。
            //   (更正上一轮的错误依据:0xead88/0xeadac 属于 1092 的 getPurchaseInActivity:target:@0xead78,
            //   「scope≠0 回空表」从来不会让主村拿到空表,已按协议语义恢复该分支。)
            //   body≠0(岛)回空表:parseActivityCenterInfo@0x1c18d8 先 resetActivitiesInfoData,count 为 0 时
            //   0x1c197e 直接收尾,安全。目前没有调用点传 10,黄金岛会话也已在 intercept 入口排除,这里只为与协议保持一致。
            let req = nsdata_bytes(env, body);
            let scope = if req.len() >= 4 {
                u32::from_le_bytes([req[0], req[1], req[2], req[3]])
            } else {
                0
            };
            if scope != 0 {
                log!("[ACTIVITY] 活动中心 cmd=1091 scope={}(黄金岛,回空表)", scope);
                enqueue_reply(env, nm, cmd, vec![0u8; 4]);
            } else {
                log!("[ACTIVITY] 活动中心 cmd=1091 scope=0(主村,回本地活动表)");
                enqueue_reply(env, nm, cmd, encode_activity_center());
            }
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_PURCHASE_IN_ACTIVITY => {
            // 只有购买类活动(type 1)才会发 1092;本地活动表里没有,保险起见回一对 0(8 字节)防 loading 超时。
            enqueue_reply(env, nm, cmd, vec![0u8; 8]);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_FIREWORK => {
            // [复核修 2026-09-15] R4-1:下面两种"不回包"情形之前都已发过宿主消息(festival_today 读系统时区、load_state 读盘),
            //   寄存器已被改写,不能 return None 放行真 sendPacket:commandId:——它在判 isReachable_(0xe2372)之前就先写
            //   self+204 的 packetsCount(0xe235e)并给 self+172 发 setSendFlag:(0xe2362)。离线真方法也只做这两步就返回
            //   (0xe2376 → 0xe2770),对离线没有意义,直接吞掉。
            let (fest, today) = festival_today(env);
            if fest != Festival::Spring {
                // 窗口外:不回包(与离线原行为一致)。
                env.cpu.regs_mut()[0] = 0;
                return Some(true);
            }
            let mut st = load_state(env);
            if st.firework_day == today.ymd() {
                log!("[ACTIVITY] 春节烟花今天已放过,cmd=1112 不回包");
                env.cpu.regs_mut()[0] = 0;
                return Some(true);
            }
            st.firework_day = today.ymd();
            save_state(env, &st);
            // parseFireworkFlag@0x1c2258 只读 1 字节,同时写 showFirework 与 hasFireworkGift。
            enqueue_reply(env, nm, cmd, vec![1u8]);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_SIGN_DAYS => {
            let today = local_date(env);
            let mut st = load_state(env);
            if sign_roll_month(&mut st, today.ym()) {
                save_state(env, &st);
            }
            enqueue_reply(env, nm, cmd, encode_sign_days(&st));
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_IS_EXCHANGED => {
            let today = local_date(env);
            let mut st = load_state(env);
            if sign_roll_month(&mut st, today.ym()) {
                save_state(env, &st);
            }
            let mut b = Vec::with_capacity(4);
            put_u32(&mut b, st.exch_mask & 0xfff);
            enqueue_reply(env, nm, cmd, b);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_SIGN_EXCHANGE_LIST => {
            enqueue_reply(env, nm, cmd, encode_sign_exchange_list());
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_SEABED_INFO => {
            let mut st = load_state(env);
            if seabed_ensure_shells(&mut st) {
                save_state(env, &st);
            }
            enqueue_reply(env, nm, cmd, encode_seabed_info(&st));
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_SEABED_REFRESH => {
            // 客户端已在 onSureRefreshClick@0x2c250a 本地扣了 3 贝壳(addVipGold:-3),这里重新生成 5 个贝壳。
            let mut st = load_state(env);
            let dug = st.dug;
            st.shells = (0..5).map(|_| (roll_shell_type(dug), 0u32)).collect();
            save_state(env, &st);
            let mut b = Vec::new();
            put_u32(&mut b, 5);
            for (i, (t, ts)) in st.shells.iter().enumerate() {
                put_u32(&mut b, i as u32 + 1);
                put_u32(&mut b, *t);
                put_u32(&mut b, *ts);
            }
            enqueue_reply(env, nm, cmd, b);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        CMD_DISCOUNT_LIST => {
            // [补完 2026-09-15] F2-2 主村限时折扣(黄金岛的 1073 不走这里;岛上会话已在 intercept 入口排除)。
            //   MOLE_DISCOUNT=off 时放行真 sendPacket:commandId:——原离线行为(只 packetsCount+1、setSendFlag 后返回,无折扣);
            //   此前没发过任何宿主消息,调用处 None 时还会恢复 r0-r3。
            if discount_disabled() {
                return None;
            }
            let body = encode_discount_list(env);
            enqueue_reply(env, nm, cmd, body);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        _ => None,
    }
}

// ─────────────────────────────── F3-6 系统公告 1058 ───────────────────────────────

/// 1058 body = count(i32) + 每条 [reserved(i32)=0][updateTime(u32)][msgLen(i32)][UTF-8](照抄私服 announce.rs)。
/// 回包后 GameManager onCommandReceived:(0x237a8)在玩家没点过公告栏时只给公告栏按钮加小星星
/// (addStarshineForNoticeBoardMenu),点了才弹木板,不会进村就强弹。
fn encode_notices() -> Vec<u8> {
    let text = format!(
        "欢迎回到摩尔庄园！当前是离线复刻版 {}（由 touchHLE 模拟器运行）。原版服务器已停运，本版在本机复活了活动中心、每日签到（踩脚印）、脚印兑换、海底寻宝和等级礼包，进度保存在本机存档里。好友互动、米币领取等离不开真实服务器的玩法暂时不可用。脚印兑换表和活动公告里的活动为移植者自拟，并非原版数据。祝你在庄园玩得开心！",
        crate::mole_sysinfo::USER_VERSION
    );
    let mut end = text.len().min(1023);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = &text.as_bytes()[..end];
    let mut b = Vec::with_capacity(16 + bytes.len());
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0i32.to_le_bytes());
    put_u32(&mut b, NOTICE_UPDATE_TIME);
    b.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
    b.extend_from_slice(bytes);
    b
}

// ─────────────────────────────── F3-2 活动公告 1091 ───────────────────────────────

/// 1091 body(照抄私服 activity.rs encode_activity_center):count(u32) + 每条
/// [version][activityid][跳8][type][joinType][showOrder][跳4][beginTime][endTime][switchedViewId][isNew]
/// [nameLP][descLP][reqLP][giftCount][giftId,number]×n。时间下发 unix 秒(客户端自己减 978307200)。
///
/// 本地活动表(**非原版数据**,原版活动配置随服务器停运消失):只放一个纯本地可判定的常驻活动——
/// type 2 = ReqLevel(ActivityBulletinControl checkConditions: tbb 0x3ae47e 解码),
/// 判定 checkAchieve_ReqLevel@0x3ad220 取 requirements 字典第一个键的 intValue 与 curLevel 比较;
/// switchedViewId(=activityJumpType_)取 100 落到 onJoinInActivity tbh 的默认臂(>9,什么也不跳),
/// 避免"参加"跳到未核实的页面;奖励素玉(100014)×5,落在 addActionCenterReward: 的 100001..109999
/// 晶玉臂(与等级礼包同类),数额保守。领取去重由 hasGetGiftActivityData_ 随 map 存档完成。
fn encode_activity_center() -> Vec<u8> {
    let mut b = Vec::new();
    // [复核修 2026-09-15] 只编码主村表(请求体 0);岛请求(请求体 1)的空表在 handle_send_packet 里单独回。
    put_u32(&mut b, 1);
    put_u32(&mut b, 1); // +0 version
    put_u32(&mut b, 55_001); // +4 activityid(自拟,避开原版可能用过的小号段)
    b.extend_from_slice(&[0u8; 8]); // +8 跳过
    put_u32(&mut b, 2); // +16 type = ReqLevel
    put_u32(&mut b, 0); // +20 joinType(非 2 = 非每日礼包)
    put_u32(&mut b, 1); // +24 showOrder
    b.extend_from_slice(&[0u8; 4]); // +28 跳过
    put_u32(&mut b, 1_420_070_400); // +32 beginTime 2015-01-01
    put_u32(&mut b, 2_145_830_400); // +36 endTime 2037-12-31
    put_u32(&mut b, 100); // +40 switchedViewId(>9 → 参加按钮走默认臂)
    put_u32(&mut b, 0); // +44 isNew
    put_lp_str(&mut b, "小摩尔成长礼", 63);
    put_lp_str(
        &mut b,
        "离线复刻版常驻活动：庄园等级达到 5 级即可领取素玉×5。（非原版活动，内容为移植者自拟）",
        1023,
    );
    put_lp_str(&mut b, "level/5", 255);
    put_u32(&mut b, 1); // giftCount
    put_u32(&mut b, 100_014); // 素玉
    put_u32(&mut b, 5);
    b
}

// ─────────────────────────────── F3-3 / F5-4 每日签到 ───────────────────────────────

/// 跨月:签到位图、补签次数、全勤奖清零(脚印保留,文案 DAILY_SIGN_EXCHANGE_DESC_ONE「脚印可以累计到下月」);
/// 兑换记录也按月清零。返回是否有改动。
fn sign_roll_month(st: &mut ActState, ym: u32) -> bool {
    let mut changed = false;
    if st.sign_month != ym {
        st.sign_month = ym;
        st.sign_days = 0;
        st.sign_patch = 0;
        st.sign_reward = 0;
        changed = true;
    }
    if st.exch_month != ym {
        st.exch_month = ym;
        st.exch_mask = 0;
        changed = true;
    }
    changed
}

/// 1117 body = 12 字节(parseDailySignDaysInfo@0x1c4080 逐字节核实):
/// [u32 位图:bit0=isGetReward,bit d(1..=31)=第 d 天已踩][u32 脚印数][u32 本月补签次数]。
fn encode_sign_days(st: &ActState) -> Vec<u8> {
    let mut b = Vec::with_capacity(12);
    put_u32(&mut b, (st.sign_days & !1u32) | (st.sign_reward & 1));
    put_u32(&mut b, st.sign_foot);
    put_u32(&mut b, st.sign_patch);
    b
}

/// 拦 -[NetworkManager sendSignDayToSure:isPatch:](1116,r2=day,r3=isPatch)。
/// 原版客户端不读 1116 回包,发完就 closeDailySignLayer,下次开层重拉 1117 才显示新状态;
/// 所以这里只在"服务器侧"(旁路档)记账,不碰 GameData.dailySignInfoData——复核发现它只是 1117 的临时容器:
/// initSignedDaysData@0x39a3ee 拷进层后立即 resetDailySignInfoData,且 isNoHaveOldDailySignData@0x89e34
/// 拿它判"旧版本地签到数据",往里写会让下次开层走 DATA_WARNING + sendOldSignDataToServer: 关层。
/// 补签的金币/贝壳已由 onButtonPatchSign:/onChooseUseVipGold 在本地扣过。
fn sign_day(env: &mut Environment, day: u32, is_patch: bool) {
    let today = local_date(env);
    let mut st = load_state(env);
    sign_roll_month(&mut st, today.ym());
    let dim = days_in_month(today.year, today.month);
    if day == 0 || day > dim {
        log!("[ACTIVITY] 签到忽略:day={} 不在本月 1..={} 内", day, dim);
        save_state(env, &st);
        return;
    }
    if st.sign_days & (1u32 << day) != 0 {
        log!("[ACTIVITY] 签到忽略:第 {} 天已经踩过", day);
        save_state(env, &st);
        return;
    }
    if !is_patch && day != today.day {
        log!(
            "[ACTIVITY] 签到提示:非补签却不是今天(day={} today={}),按客户端请求记账",
            day,
            today.day
        );
    }
    st.sign_days |= 1u32 << day;
    st.sign_foot = st.sign_foot.saturating_add(1);
    if is_patch {
        st.sign_patch = st.sign_patch.saturating_add(1);
    }
    save_state(env, &st);
    log!(
        "[ACTIVITY] 签到记账 cmd=1116 day={} patch={} 脚印={} 本月补签={}",
        day,
        is_patch,
        st.sign_foot,
        st.sign_patch
    );
}

/// 拦 -[NetworkManager getAllDaysReward](1118,onButtonGetReward:@0x39996e 唯一调用者)。
/// 奖励内容取游戏自带文案 DAILY_SIGN_GET_REWARD_DESC:「每月全部都踩满(包括补踩)的小摩尔可以在最后一天
/// 获得额外的 10 个脚印」——这是原版客户端写明的规则,不是自拟。parseData 里没有 1118 的解析臂,不回包。
fn sign_full_attendance_reward(env: &mut Environment) {
    let today = local_date(env);
    let mut st = load_state(env);
    sign_roll_month(&mut st, today.ym());
    let dim = days_in_month(today.year, today.month);
    let mut signed = 0;
    for d in 1..=dim {
        if st.sign_days & (1u32 << d) != 0 {
            signed += 1;
        }
    }
    if st.sign_reward != 0 {
        log!("[ACTIVITY] 全勤奖忽略:本月已领过");
    } else if signed < dim {
        log!("[ACTIVITY] 全勤奖忽略:本月只踩了 {}/{} 天", signed, dim);
    } else {
        st.sign_reward = 1;
        st.sign_foot = st.sign_foot.saturating_add(10);
        log!("[ACTIVITY] 全勤奖 cmd=1118:脚印 +10 → {}", st.sign_foot);
    }
    save_state(env, &st);
}

/// 脚印兑换表(**非原版数据**:原版由服务器 1090 下发,已随停运消失;移植者自拟,取值保守)。
/// 只放晶玉类(海底寻宝挖贝要用的素玉/绿叶水晶/橙六彩,见 SEA_TREASURE_TIPS_2~4 与 onDigShellClick:
/// 0x2c1d54/0x2c1e02 的 getCouponNumber: 检查),它们走 addInvisibleReward 的晶玉臂,不需要放置。
/// 条目:(所需脚印, 物品 id, 数量)。每项每月可兑一次(兑换位图按月清零)。
const SIGN_EXCHANGE_TABLE: [(u32, u32, u32); 4] = [
    (5, 100_014, 2),  // 素玉 ×2
    (10, 100_014, 5), // 素玉 ×5
    (15, 100_006, 1), // 绿叶水晶 ×1
    (20, 100_005, 1), // 橙六彩 ×1
];

/// 1090 body = count(u32) + 每条 12 字节 [foodPrints][itemId][number]
/// (parseDailySignInfo@0x1c17da:+0→foodPrints、+4→itemId、+8→number,再 initWithItemId:number:foodPrints:)。
fn encode_sign_exchange_list() -> Vec<u8> {
    let mut b = Vec::new();
    put_u32(&mut b, SIGN_EXCHANGE_TABLE.len() as u32);
    for &(foot, item, num) in SIGN_EXCHANGE_TABLE.iter() {
        put_u32(&mut b, foot);
        put_u32(&mut b, item);
        put_u32(&mut b, num);
    }
    b
}

/// 拦 -[NetworkManager getFoodsExchangeToSure:](1120,r2 = foodPrintExchangeIndex,1 起;
/// 0x266600/0x4f806 都要求 >=1 才发)。原版扣脚印在服务器侧,这里在旁路档扣并记兑换位。
fn sign_exchange_confirm(env: &mut Environment, index: u32) {
    let today = local_date(env);
    let mut st = load_state(env);
    sign_roll_month(&mut st, today.ym());
    let idx = index as usize;
    if idx == 0 || idx > SIGN_EXCHANGE_TABLE.len() {
        log!("[ACTIVITY] 脚印兑换忽略:index={} 超出本地兑换表", index);
        save_state(env, &st);
        return;
    }
    let (cost, item, num) = SIGN_EXCHANGE_TABLE[idx - 1];
    let bit = 1u32 << (idx - 1);
    if st.exch_mask & bit != 0 {
        log!("[ACTIVITY] 脚印兑换提示:第 {} 项本月已兑过,仍按客户端请求扣脚印", index);
    }
    st.sign_foot = st.sign_foot.saturating_sub(cost);
    st.exch_mask |= bit;
    save_state(env, &st);
    log!(
        "[ACTIVITY] 脚印兑换 cmd=1120 index={} 物品={}×{} 花费={} 剩余脚印={}",
        index,
        item,
        num,
        cost,
        st.sign_foot
    );
}

/// 拦 -[NetworkManager sendOldSignDataToServer:](r2 = GameData.dailySignInfoData)。
/// showWithParent:@0x397294 在检测到"旧版本地签到数据"时调用它上传,然后弹 DATA_WARNING 关层。
/// 离线扮演服务器:把旧数据并进旁路档,再 resetDailySignInfoData 清掉本地旧数据,下次开层即恢复正常。
fn sign_migrate_legacy(env: &mut Environment, info: id) {
    let today = local_date(env);
    let mut st = load_state(env);
    sign_roll_month(&mut st, today.ym());
    if info != nil {
        let s_month = sel_named(env, "month");
        let legacy_month: u32 = msg_send(env, (info, s_month));
        let s_foot = sel_named(env, "hasCollectedFoodPrintNum");
        let legacy_foot: i32 = msg_send(env, (info, s_foot));
        let s_patch = sel_named(env, "curMonthPatchSignNum");
        let legacy_patch: u32 = msg_send(env, (info, s_patch));
        let s_days = sel_named(env, "hasSignedDays");
        let arr: id = msg_send(env, (info, s_days));
        let mut legacy_days = 0u32;
        if arr != nil {
            let s_count = sel_named(env, "count");
            let count: GuestUSize = msg_send(env, (arr, s_count));
            let s_at = sel_named(env, "objectAtIndex:");
            let s_int = sel_named(env, "intValue");
            for i in 0..count.min(62) {
                let n: id = msg_send(env, (arr, s_at, i));
                if n == nil {
                    continue;
                }
                let d: i32 = msg_send(env, (n, s_int));
                if (1..=31).contains(&d) {
                    legacy_days |= 1u32 << d;
                }
            }
        }
        // 脚印取较大值(避免同一批脚印重复叠加);同月的签到位图合并。
        if legacy_foot > 0 {
            st.sign_foot = st.sign_foot.max(legacy_foot as u32);
        }
        if legacy_month == today.month || legacy_month == today.ym() {
            st.sign_days |= legacy_days;
            st.sign_patch = st.sign_patch.max(legacy_patch.min(31));
        }
        log!(
            "[ACTIVITY] 旧签到数据并入旁路档:month={} 脚印={} 位图={:#x} 补签={}",
            legacy_month,
            legacy_foot,
            legacy_days,
            legacy_patch
        );
    }
    save_state(env, &st);
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd != nil {
        let reset = sel_named(env, "resetDailySignInfoData");
        let _: () = msg_send(env, (gd, reset));
    }
}

// ─────────────────────────────── F3-4 海底寻宝 ───────────────────────────────

/// 新贝壳类型:1=摩尔豆贝(1000 豆,1 珍珠)、2=素玉×5+绿叶水晶×1(3 珍珠)、3=素玉×10+橙六彩×1(5 珍珠)、
/// 4=海王贝(直接出道具,客户端 generateRandomRewardId 掷骰)。概率为移植者自定;
/// 文案 SEA_TREASURE_RULE「挖取海贝数量越多,越有可能遇到珍贵的海王贝」→ 海王贝概率随累计挖贝数上升。
fn roll_shell_type(dug: u32) -> u32 {
    let r = rand_u32() % 100;
    let haiwang = 6 + (dug / 20).min(10);
    if r < haiwang {
        4
    } else if r < haiwang + 12 {
        3
    } else if r < haiwang + 12 + 25 {
        2
    } else {
        1
    }
}

/// 贝壳表缺失/损坏时生成 5 个新贝壳(时间戳 0 = 无冷却;displayUI@0x2c11e4 对 <=1000 的时间戳不算冷却)。
fn seabed_ensure_shells(st: &mut ActState) -> bool {
    if st.shells.len() == 5 {
        return false;
    }
    let dug = st.dug;
    st.shells = (0..5).map(|_| (roll_shell_type(dug), 0u32)).collect();
    true
}

/// 1219 body(parseSeabedSeekingTreasureActivityInfo@0x1ca98c 核实):
/// [flag][珍珠数][n] + n×[pearlPos][pearlType][lastTimestamp] + [bDigShellPlayers][bExchangeReward]。
/// pearlPos 按 1..5(onDigShellClick: 发 1220 时传的是 tag+1,0x2c1d02)。
fn encode_seabed_info(st: &ActState) -> Vec<u8> {
    let mut b = Vec::new();
    put_u32(&mut b, 1); // 活动进行中
    put_u32(&mut b, st.pearl);
    put_u32(&mut b, st.shells.len() as u32);
    for (i, (t, ts)) in st.shells.iter().enumerate() {
        put_u32(&mut b, i as u32 + 1);
        put_u32(&mut b, *t);
        put_u32(&mut b, *ts);
    }
    put_u32(&mut b, u32::from(st.dug > 0));
    put_u32(&mut b, 0);
    b
}

/// 拦 -[NetworkManager seabedSeekingTreasureDigShellWith:shellType:pearlCount:](r2=pos 1..5,r3=贝壳类型)。
/// 挖贝费用已由 onDigShellClick: 本地扣过;珍珠数按客户端同一规则(0x2c2014/0x2c1dbe/0x2c1e6e:
/// 类型 1/2/3 → 1/3/5,海王贝 0)累加。回 1220:[新贝壳类型][珍珠总数][bDigShellPlayers=1]。
fn seabed_dig(env: &mut Environment, nm: id, pos: u32, shell_type: u32) {
    let mut st = load_state(env);
    seabed_ensure_shells(&mut st);
    let gained = match shell_type {
        1 => 1,
        2 => 3,
        3 => 5,
        _ => 0,
    };
    st.pearl = st.pearl.saturating_add(gained);
    st.dug = st.dug.saturating_add(1);
    let new_type = roll_shell_type(st.dug);
    let idx = (pos.clamp(1, 5) - 1) as usize;
    // 挖过的贝壳换成新类型并记下挖掘时间(CFAbsoluteTime 纪元),重开层时 displayUI 据此算 5 分钟冷却。
    st.shells[idx] = (new_type, now_cf_u32());
    save_state(env, &st);
    log!(
        "[ACTIVITY] 海底寻宝挖贝 pos={} type={} 珍珠+{} → {} 新贝壳类型={}",
        pos,
        shell_type,
        gained,
        st.pearl,
        new_type
    );
    let mut b = Vec::with_capacity(12);
    put_u32(&mut b, new_type);
    put_u32(&mut b, st.pearl);
    put_u32(&mut b, 1);
    enqueue_reply(env, nm, CMD_SEABED_DIG, b);
}

/// 拦 -[NetworkManager seabedSeekingTreasureExchangeRewardWithPearlCount:](r2 = 所需珍珠,
/// 由 EditMenuLayer onButtonOkSelected: 按兑换档位给出 10/…/100)。奖励物品已由客户端
/// SeabedSeekingTreasureExchageRewardLayer onChangeItemReward: 放进村庄;这里扣珍珠,回 1221 [bExchangeReward=0]。
fn seabed_exchange(env: &mut Environment, nm: id, cost: u32) {
    let mut st = load_state(env);
    let cost = cost.min(1_000);
    if st.pearl < cost {
        log!(
            "[ACTIVITY] 海底寻宝兑换提示:珍珠不足({} < {}),按客户端请求扣到 0",
            st.pearl,
            cost
        );
    }
    st.pearl = st.pearl.saturating_sub(cost);
    save_state(env, &st);
    log!(
        "[ACTIVITY] 海底寻宝兑换 cmd=1221 花费珍珠={} 剩余={}",
        cost,
        st.pearl
    );
    let mut b = Vec::with_capacity(4);
    put_u32(&mut b, 0);
    enqueue_reply(env, nm, CMD_SEABED_EXCHANGE, b);
}

// ─────────────────────────────── [补完 2026-09-15] F2-2 限时折扣 1049 ───────────────────────────────

/// 每天挑几件。
const DISCOUNT_COUNT: usize = 6;
/// 折扣率(百分比,7~8 折)。
const DISCOUNT_PCTS: [u32; 3] = [70, 75, 80];
/// 贝壳原价下限(太便宜的打完折看不出差价)。
const DISCOUNT_MIN_PRICE: u32 = 5;
/// 贝壳原价上限(防脏数据;属性表在售贝壳商品最高 150)。
const DISCOUNT_MAX_PRICE: u32 = 10_000;
/// 只挑装饰类(ObjectData.type 14):属性表在售纯贝壳商品 313 件里 268 件是 14;
/// -[NewStyleStoreMainLayer onBuyItem:] 0x3b27a0 起对 rest_place==2、type 20/25 与个别 ID 另走分支,避开最稳。
const DISCOUNT_OBJECT_TYPE: u8 = 14;
/// 充值解锁物(getLockType4Object: 返回 13 = RECHARGE_TO_UNLOCK,离线永锁,打折也买不了):16283 都教授在售(shop_type 2 / sub 4);
/// 14956 乐乐水塔 / 14974 克劳神父 / 14987 织女鹊桥 本来不在商店,一并列出防御。
const DISCOUNT_EXCLUDE: [u32; 4] = [14956, 14974, 14987, 16283];

/// MOLE_DISCOUNT=off|0|false|no|none 关闭离线限时折扣。
fn discount_disabled() -> bool {
    match std::env::var("MOLE_DISCOUNT") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no" | "none"
        ),
        Err(_) => false,
    }
}

/// 本地"今天"的日期与次日 0:00 的 unix 秒。时区口径同 local_date([NSTimeZone systemTimeZone],默认北京时间,
/// 含开发工具时间旅行偏移);次日 0:00 用那一刻的 UTC 偏移换算,MOLE_TZ=host 跨夏令时也准。
fn local_today_and_midnight(env: &mut Environment) -> (LocalDate, i64) {
    use crate::frameworks::foundation::ns_time_zone::seconds_from_gmt_at_unix;
    let cf = crate::frameworks::core_foundation::time::cf_absolute_time_now();
    let cf = if cf.is_finite() { cf } else { 0.0 };
    let unix = cf.floor() as i64 + 978_307_200;
    let tz_cls = env.objc.get_known_class("NSTimeZone", &mut env.mem);
    let tz: Option<id> = if tz_cls != nil {
        let s = sel_named(env, "systemTimeZone");
        let tz_obj: id = msg_send(env, (tz_cls, s));
        Some(tz_obj)
    } else {
        None
    };
    let offset: i64 = match tz {
        Some(t) => seconds_from_gmt_at_unix(env, t, unix) as i64,
        None => 8 * 3600,
    };
    let day_index = (unix + offset).div_euclid(86_400);
    let (year, month, day) = civil_from_days(day_index);
    let next_local_midnight = (day_index + 1) * 86_400;
    let offset_next: i64 = match tz {
        Some(t) => seconds_from_gmt_at_unix(env, t, next_local_midnight - offset) as i64,
        None => 8 * 3600,
    };
    (
        LocalDate { year, month, day },
        next_local_midnight - offset_next,
    )
}

/// 从主村商店分页数组收集可打折的贝壳商品:(物品 ID, 贝壳原价),按 ID 升序去重。
/// 这两个数组就是商店实际展示的数据源(-[NewStyleStoreItemsView loadObjectsDataByType:] 5..16 直接取用),元素是 ObjectData;
/// 只对数组发 count / objectAtIndex:,字段直接读 ivar,不逐个发消息。
/// 选品规则(移植者自拟,非原版):shop_type 1/2 · 装饰类 type 14 · 纯贝壳价(cost_gold==0 且 cost_vip_gold 5..=10000)·
/// 非 VIP 专属(vip_level==0)· 不限购(limit_count==0)· ID>1000 且不在充值解锁清单。
/// - 为什么只挑贝壳价:消费方拿 goodsPrice 顶替的是 cost_vip_gold(见 encode_discount_list),金币价物品打折会变成贝壳价。
/// - ID>1000:addOneDiscountGood:@0x82240 对 ID 1..7(贝壳充值包)不查物品表直接收,商店详情对 ID<=1000 显示
///   SUPER_SHELL_DISCOUNT「打折期间额外赠送%d个超级贝壳」,那是内购档位,离线不碰。
/// - 隐藏物品页注入进同一数组的物品没有 shop_type(属性表缺该键 → 0),天然被排除。
/// - 不筛等级(属性表里在售贝壳商品 level 全为 1,也免得同一天因玩家升级而换品);不查美术(在售商品商店本来就要画)。
fn discount_candidates(env: &mut Environment) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return out;
    }
    let count_sel = sel_named(env, "count");
    let at_sel = sel_named(env, "objectAtIndex:");
    for (slot, want_shop_type) in [
        (SLOT_GD_STORE_BUILDINGS, 1u8),
        (SLOT_GD_STORE_DECORATIONS, 2u8),
    ] {
        let pages: id = match read_ivar_u32(env, gd, slot) {
            Some(bits) => Ptr::from_bits(bits),
            None => nil,
        };
        if pages == nil {
            continue;
        }
        let page_count: GuestUSize = msg_send(env, (pages, count_sel));
        for p in 0..page_count.min(16) {
            let page: id = msg_send(env, (pages, at_sel, p));
            if page == nil {
                continue;
            }
            let n: GuestUSize = msg_send(env, (page, count_sel));
            for i in 0..n.min(4096) {
                let obj: id = msg_send(env, (page, at_sel, i));
                if obj == nil {
                    continue;
                }
                // 只认 ObjectData 本类(主村商店数组的元素类型;岛上的是别的数据源)。
                let isa = crate::objc::ObjC::read_isa(obj, &env.mem);
                if env.objc.try_get_class_name(isa) != Some("ObjectData") {
                    continue;
                }
                let shop_type = read_ivar_u8(env, obj, SLOT_OBJ_SHOP_TYPE).unwrap_or(0);
                let obj_type = read_ivar_u8(env, obj, SLOT_OBJ_TYPE).unwrap_or(0);
                let limit_count = read_ivar_u8(env, obj, SLOT_OBJ_LIMIT_COUNT).unwrap_or(1);
                let vip_level = read_ivar_u32(env, obj, SLOT_OBJ_VIP_LEVEL).unwrap_or(1);
                let cost_gold = read_ivar_u32(env, obj, SLOT_OBJ_COST_GOLD).unwrap_or(1);
                let price = read_ivar_u32(env, obj, SLOT_OBJ_COST_VIP_GOLD).unwrap_or(0);
                let Some(item) = read_ivar_u32(env, obj, SLOT_OBJ_ID) else {
                    continue;
                };
                if shop_type != want_shop_type
                    || obj_type != DISCOUNT_OBJECT_TYPE
                    || limit_count != 0
                    || vip_level != 0
                    || cost_gold != 0
                    || !(DISCOUNT_MIN_PRICE..=DISCOUNT_MAX_PRICE).contains(&price)
                    || item <= 1000
                    || DISCOUNT_EXCLUDE.contains(&item)
                {
                    continue;
                }
                out.push((item, price));
            }
        }
    }
    out.sort_unstable();
    out.dedup_by_key(|e| e.0);
    out
}

/// splitmix64:按日期做确定性伪随机(同一天多次请求结果一致,不依赖进程内随机状态,重启游戏也一样)。
fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// 从候选里按日期 yyyymmdd 确定性地挑 DISCOUNT_COUNT 件(部分 Fisher-Yates),返回 (ID, 原价, 折后价)。
fn pick_discounts(candidates: &[(u32, u32)], ymd: u32) -> Vec<(u32, u32, u32)> {
    let mut pool = candidates.to_vec();
    let n = DISCOUNT_COUNT.min(pool.len());
    let mut state = splitmix64(u64::from(ymd) ^ 0x4d4f_4c45_0419);
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        state = splitmix64(state);
        let j = k + (state % (pool.len() - k) as u64) as usize;
        pool.swap(k, j);
        let (item, orig) = pool[k];
        let pct = DISCOUNT_PCTS[((state >> 32) % DISCOUNT_PCTS.len() as u64) as usize];
        // 四舍五入到整贝壳,且至少便宜 1 个(候选原价 >= 5,区间 [1, orig-1] 合法)。
        let price = ((orig * pct + 50) / 100).clamp(1, orig - 1);
        out.push((item, orig, price));
    }
    out
}

/// 1049 body(parseDiscountList:pos:len:@0x1bfa8c 逐字节核实):count(u32) + count × 12 字节
/// [goodsId(+0 → setGoodsId:)][goodsPrice(+4 → setGoodsPrice:)][expireTime(+8 → setExpireTime:)](0x1bfbb4/0x1bfbc4/0x1bfbd6)。
/// - goodsPrice = 折后**贝壳**单价:消费方都拿它顶替 cost_vip_gold——getLockType4Object:@0x7d8c2、onBuyItem:@0x3b2790、
///   VillageMenuLayer canBuyMultiple:@0x643bc、商店详情 updateObjectInfo(0x3ba2dc 判定后 0x3baf10 取价,discount_line.png 划线)。
///   [补完 2026-09-15] 复核更正扣款点:主村真正扣贝壳的是 -[Porter finishBuildWithHouseLevel:isGift:](0x2b7c6 取 goodsPrice →
///   0x2b80e 取负 → 0x2b856 addVipGold:)与 -[VillageMenuLayer showCostGoldView:](0x64a64 取价 → 0x64a88 取负 → 0x64ad2 addVipGold:);
///   -[EditMenuLayer onButtonOkSelected:]@0x4ed6e 也按折扣价判定要不要花贝壳;onChooseUse@0x53380 只查 checkIsDiscountObj:,
///   命中时上报折扣购买统计(0x534d2 addAnalyticsEvent:eventName:)。
///   -[GameData addVipGoldForBuy:UIUpdate:](IMP 0x86c30)与商店扣款无关:它只被 addAlreadyPurchaseVipgoldWithPurchaseInfo:@0x7f176
///   和 -[InAppPurchaseManager onPurchaseSuccessful]@0x117c4c 调用，是内购充值包(itemid 1..7,0x86d1a/0x86d1e)命中折扣时把
///   goodsPrice 作为额外赠送的贝壳加到到账数上(0x86d84 取价、0x86d8e 相加、0x86db4 addVipGold:)。本模块选品已排除 ID<=1000,不走这条路。
/// - expireTime:客户端从不读(DiscountInfo 的 expireTime 取值方法无 selref,ivar 只有 init/存取器引用;DiscountInfoLayer
///   lefttime_ 无写入者、startTimer 无调用者,CommonEffectController innerupdateDiscount: 只由 startTimer 排程,均是死代码)。
///   照私服 economy.rs 口径填当天本地 24:00 的 unix 秒,仅作语义与日志用。
/// - count==0 时解析器不清旧表(0x1bfae2 在 removeAllObjectFromDiscountArr 之前返回);count>0 先清再加,所以跨天后的下一次
///   1049(进村/回前台)会整表换成新一天的折扣。原版客户端不在会话中途过期清表,这里保持一致。
fn encode_discount_list(env: &mut Environment) -> Vec<u8> {
    let (today, midnight_unix) = local_today_and_midnight(env);
    let candidates = discount_candidates(env);
    let picks = pick_discounts(&candidates, today.ymd());
    let expire = midnight_unix.clamp(0, u32::MAX as i64) as u32;
    let mut b = Vec::with_capacity(4 + picks.len() * 12);
    put_u32(&mut b, picks.len() as u32);
    let mut desc: Vec<String> = Vec::with_capacity(picks.len());
    for &(item, orig, price) in picks.iter() {
        put_u32(&mut b, item);
        put_u32(&mut b, price);
        put_u32(&mut b, expire);
        desc.push(format!("{}:{}→{}", item, orig, price));
    }
    log!(
        "[ACTIVITY] 限时折扣 cmd=1049 日期={} 候选={} 选中{}件 [{}] 到期unix={}",
        today.ymd(),
        candidates.len(),
        picks.len(),
        desc.join(" "),
        expire
    );
    b
}
