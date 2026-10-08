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
//! - [2026-10-03] 截包时只拷下请求体、置标志,应答在运行循环受理点(run_loop_poll,ns_run_loop 主线程 perform 相位之后)算好入队,
//!   下一轮受理点再把包追加进 `NetworkManager.buffer_`(ivar +196,
//!   偏移从 guest 的 _OBJC_IVAR 槽 0xb043f0 现读,兼容非脆弱 ivar 修正),然后 msg_send
//!   `parseBufferWhenDidReadData`@0xebefc(parseData:header:pos: 的唯一调用者)。原版的解码、
//!   GameManager/各层 onCommandReceived: 分发、hideLoadingLayer 全部照原链路跑。
//!   以前是截包时当场组回包,再 `performSelector:withObject:afterDelay:0` 排自拦选择子 `moleActivityLoopback` 来喂;
//!   发包点在 drawScene / CCScheduler 帧栈上时(比如从岛回村的进村补发),等于在帧栈上发宿主消息。
//! - `parseBufferWhenDidReadData` 每处理完一个包会 `changeStateTo:7 withMessage:@""`(0xec920)。离线网络
//!   状态机不该被回环改成"已收包",所以回环期间把这一次 state=7 吞掉。
//! - 它还会 `[UnreadPacketsDic_ removeObjectForKey:@"<sendFlag>"]`;离线真 sendPacket 在登记超时表之前就
//!   返回了,表里没有条目,sendFlag 填 0 即可,移除不存在的键是空操作。
//! - GameManager 是在 `-[GameManager startGame:]`+0x7a8 设成 delegateGameData 的;万一回环时它为空
//!   (比如从岛上回来的中间态),回环期间临时指回 GameManager,解析完恢复原值。
//!
//! # 本地数据
//! 签到/脚印兑换/海底寻宝/烟花去重/每日任务选题等"服务器侧状态"存旁路文件 `mole_activity.dat`
//! (路径取 `-[GameData pathForDataFile:]`,与岛档同目录;`writeToFile:atomically:YES` 原子写)。
//! [2026-09-16] F2-06 格式升 v=2,末行 `sum=<fnv1a>` 必须存在且匹配;v=1 旧档宽松读一次,下次保存自动升级。
//! 坏档先原样备份成 `.corrupt`(已存在加 `-<unix秒>`)再按默认值继续;备份失败则本会话不再覆盖原文件。
//! [2026-09-16] F2-05 开发工具「时间旅行」偏移非 0 时只读写内存缓存、不落盘:重启回到现实,正式档不会被「未来日期」改写。
//! [2026-09-16] X4-02 但补签、挖贝、刷新贝壳、脚印兑换、珍珠兑换的扣款与发物都由客户端本地完成、照常进主档,侧档不落盘就会在重启后
//! 变成「扣了款、进度回滚」或「同一档位能再兑一次」。所以旅行期间在本地扣款之前拦下这些付费入口(见 block_paid_action_in_time_travel),
//! 免费踩格、看翻月不拦。
//!
//! # 限时折扣 1049([补完 2026-09-15] F2-2)
//! 进村(-[GameManager startGame:])与回前台(applicationDidBecomeActive:)会发 1049。回环服务器按本地日期确定性地
//! 挑几件主村商店的纯贝壳商品打 7~8 折回包,经原版 parseDiscountList:pos:len: → addOneDiscountGood: 进
//! GameData.discountObjDataArr_,商店划线价/买得起判定/扣款全走原版。选品规则为移植者自拟,非原版数据;不落盘。
//! 原版回包后 GameManager 只弹赛尔号/中信跨游戏推广层(折扣面板 UI 在 5.5.0 已是死代码),离线吞掉这次分发。
//! MOLE_DISCOUNT=off 关闭(恢复原离线行为:无折扣)。
//!
//! # 每日任务 1074([2026-09-16] E-03)
//! 离线时原版只在 isConnected 门内(startGame:+0xb04)或点 NPC 发现列表为空时发 1074,没人应答,点日常 NPC 就弹「没有连接网络」。
//! 主村由回环应答;黄金岛不走回环(岛上会话吞掉全部 sendPacket),在宿主侧照 parseDailyTaskListWithSceneId:pos:len: 的做法
//! 构造 DailyQuestList,再交给原版 updateDailyQuestListInHolidayVillageWithCurrentServerData:。选题规则见 daily_values_for_today。
//!
//! # 黄金岛限时折扣 1073([2026-09-24] 第四轮 K6 I8-4)
//! 进岛 -[HolidayVillageLayer onEnter] 发 getDiscountObjectsListFormServerWithMapId:10。岛上会话吞掉全部 sendPacket,与每日任务同一写法:
//! 接住请求、排一次运行循环回调,在宿主侧照 parseDiscountListWithSceneId:pos:len: 岛分支构造 DiscountInfo 交给 NewSceneData,
//! 建设庄园的划线价/买得起判定/扣款全走原版。选品规则为移植者自拟,非原版数据(见 island_discount_candidates);MOLE_DISCOUNT=off 同样关闭。
//!
//! # VIP 信息 1084([2026-09-25] 第五轮遗留 V)
//! 离线接住 -[NetworkManager getVipInfo] 只置一个排队标志,由主线程运行循环受理点(ns_run_loop,perform 相位之后)照原版回包的
//! 两条分发臂执行:主村 -[GameManager onCommandReceived:] 0x239f6(HUD VIP 按钮/徽章、AchievementControl checkConditions:0x800),
//! 岛上 -[HolidayVillageLayer onNewSceneGameDataCommandReceived:] 0x23e7c4(HUD、NewSceneAchievement checkConditions:0x1000、贝壳树 rescheduleTree)。
//! 不伪造报文、不走 parseVipInfo(本地 VIP 三值由 mole_items 的 vip.dat 侧档负责);进村(0x19ca0)与假充值(0x117dcc)照原版入口补发;
//! 修改器「强制 VIP」开着时跳过成就判定那一步(只刷新 HUD 与贝壳树)。见 vip_info_poll。

use crate::frameworks::foundation::ns_string;
use crate::fs::GuestPath;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, Ptr};
use crate::objc::{id, msg_send, nil, release, SEL};
use crate::Environment;
use digest::Digest;
use md5::Md5;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
/// [2026-09-16] E-03 1074 getDailyTaskListFromServerWithSceneId:(0x1cb5cc,0x1cb60e `movw r3, #0x432`)→
/// parseDailyTaskListWithSceneId:pos:len:(0x1c0398)。请求体 1 字节:参数 1 → 0(主村)、10 → 1(黄金岛),其它参数不发包。
const CMD_DAILY_TASK_LIST: u32 = 1074;
/// [2026-10-06 第九轮 R9-D1] 1051 getLoginCountFromServer(0x1cb1b0,0x1cb1bc `movw r3, #0x41b`)→ parseLoginCount:pos:len:@0x1bfca8
/// (只 getBytes 1 字节 → [GameData setLoginTimesCounter:]);onCommandReceived: tbh@0x22ed8 → 0x23620
/// [[AchievementControl shareInstance] checkConditions:0x20](全二进制唯一触发「连续登录」成就判定的地方)。
const CMD_LOGIN_COUNT: u32 = 1051;
/// [2026-10-06 第九轮 R9-D1] 1050 getOnlineTimeFormServer(0x1cb194,0x1cb1a0 `movw r3, #0x41a`)→ parseOnlineTime:pos:len:@0x1bfc30
/// (getBytes 4 字节 → setOnlineTimer:,单位秒);分发 0x235f2 checkConditions:0x10(累计在线成就)。
const CMD_ONLINE_TIME: u32 = 1050;
/// [2026-10-04 第八轮 R8-C2] 1138 -[NetworkManager getDivineDataList]@0x1cbe6c(sendPacket 0x472)→ parseDivineDataList:pos:len:@0x1c4d80。
/// 回包:[u32 今天已用过免费(非 0=已用,0x1c4eb6 → setHasFreeDivinedToday:)][u32 组数] + 每组 [u32 轮次][u32 件数]
/// + 件数×[u32 objectId][u32 num][u32 posibility]。轮次 1..=5 进 divineDataArray[轮次-1](0x1c5044 cmp/bls → 0x1c5062),
/// 轮次 10 → setTopDivinePrize:(0x1c5048,取最后一件),轮次 0 整组跳过,其它丢弃。
const CMD_DIVINE_LIST: u32 = 1138;
/// [2026-10-04 第八轮 R8-C2] 1139 setFreeDivineTag(@0x1cbe88):免费占卜那一次在免费分支发出;服务器记「今天已免费」,客户端不读回包。
const CMD_FREE_DIVINE_TAG: u32 = 1139;
/// [2026-10-04 第八轮 R8-C2] 占卜的三道 isConnected 门(blx 地址):-[MiniGameManager enterMiniGame:stage:] 占卜分支 0xf4132
/// (为真才 showLoadingLayer + getDivineDataList)、-[DivineGame firstCostPlay] 0x21638a、-[DivineGame costGoldToDivine] 0x21718a。
/// 「修复占卜功能」开着时离线按调用点放行,由回环应答 1138/1139(以前是破解字节跳过 1138、奖池恒空)。
const SITE_DIVINE_ENTER_CONNECTED: u32 = 0xf4132;
const SITE_DIVINE_FIRST_CONNECTED: u32 = 0x21638a;
const SITE_DIVINE_COST_CONNECTED: u32 = 0x21718a;
/// [2026-10-04 第八轮 R8-C2] 离线占卜奖池(用户拍板「恢复流程 + 自拟奖池」,**移植者自拟,非原版数据**;原版奖池只在服务器)。
/// (轮次, [(objectId, 数量, 概率)]),每轮概率之和必须是 100:-[DivineGame generatePresentId]@0x217f28 用 random()×2⁻³¹×100.0
/// (0x218044)取整后逐件减 posibility,减到负数那件就是抽中的。特殊编号照原版发奖代码(-[WrapperManager releaseFirstChargeGift:]
/// 0x2630e6/0x2630ec):701 摩尔豆、702 经验,经 -[DivineGame confirmRandomGift] 0x2170ec addInvisibleReward:num: 直接入账
/// (719..724 要联网,不用);其余是物品,「拿走所有奖励」-[DivineGame putAllGiftOnMap]@0x2180c0 交给主界面
/// onAddAllDivineGifts 进摆放模式,由玩家摆进庄园。
/// 不放贝壳(704):一局只在开始时扣一次(firstCostPlay 0x216402 占卜券 −1,无券时 0x2165b0 贝壳 −1),之后五轮「继续占卜」
/// 不再扣费,奖池里放贝壳就能拿 1 贝壳换回好几个贝壳、无限刷。
/// 物品挑各城堡系列里商店不卖、描述写明要去占卜屋拿的那一件(property.dat 无 shop_type):14926 天鹅堡主城沙雕、
/// 16046 古堡主塔、16103 东欧城堡左塔楼、16212 茉莉公主皇宫副宫,以及描述写明「还可通过占卜屋获得」的 16269 小鸟水台。
/// 装饰物不能卖回换钱(回收站只收房屋和动物),不构成刷钱途径。越往后轮奖励越好,第 5 轮全是城堡部件(呼应原版台词
/// 「最好的宝贝放在最后一个水晶球里」);第 10 轮是背景里展示的「今日宝贝」(-[DivineGame setBg] 0x214062 读 topDivinePrize)。
#[rustfmt::skip]
const DIVINE_POOL: [(u32, &[(u32, u32, u32)]); 6] = [
    (1, &[(701, 1000, 45), (702, 300, 45), (16269, 1, 10)]),
    (2, &[(701, 3000, 40), (702, 800, 40), (16269, 1, 20)]),
    (3, &[(701, 8000, 35), (702, 2000, 35), (16212, 1, 15), (16103, 1, 15)]),
    (4, &[(701, 20000, 30), (702, 5000, 30), (16103, 1, 20), (16046, 1, 20)]),
    (5, &[(16046, 1, 30), (16212, 1, 30), (16103, 1, 20), (14926, 1, 20)]),
    (10, &[(14926, 1, 100)]),
];

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
/// [2026-10-07 第十一轮 R11-F-1] -[iMoleVillageAppDelegate applicationWillEnterForeground:]@0x1133c 的 isConnected
/// (blx 0x1171c,LR 0x11721)。原版回前台时只有在线才 getServerTime(0x1174e)+ 按场景要当天的每日任务
/// getDailyTaskListFromServerWithSceneId:(0x11780),这是跨天回来换日常列表的唯一入口;离线主村这道门恒假,
/// 挂后台跨过零点回来日常任务还是昨天那组。放行后离线 getServerTime 发包空过,1074 由回环按当天应答。
const SITE_FOREGROUND_DAILY_CONNECTED: u32 = 0x1171c;
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

/// [2026-09-16] A1-01 春节烟花 1112 回包的延迟槽:(整包, 已检查次数, 最早下次检查时刻)。
/// 回包到达时村庄场景可能还没挂上 FireworkLayer(见 feed_or_defer_firework),先放这里,约每秒重查一次。
static FIREWORK_DEFERRED: Mutex<Option<(Vec<u8>, u32, Instant)>> = Mutex::new(None);
/// [2026-09-16] A1-01 一个 1112 回包从入队到喂进解析链(或放弃)之间置位:同一会话再次进村时不重复排第二个烟花包。
static FIREWORK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// [2026-09-16] A1-01 烟花回包等场景就绪的最多检查次数(间隔约 1 秒)。
const FIREWORK_RETRY_MAX: u32 = 10;
/// [2026-10-03] FIREWORK_DEFERRED 里是否有包在等(受理点每轮先读它,只有为真才去拿锁看到没到点)。
static FIREWORK_WAITING: AtomicBool = AtomicBool::new(false);

/// [2026-10-03] 回环改由运行循环受理点驱动(见 run_loop_poll),原调用栈上只做内存读写与原子操作。
/// 以前 sendPacket:commandId: 臂当场读旁路档、组回包(要给 GameData 等发宿主消息),再 performSelector:withObject:afterDelay:0
/// 排 moleActivityLoopback;从岛回村时进村补发跑在 CCScheduler 帧栈上(-[NewBaseLoading endLoading] → … → startGame:),
/// 好友村回家 reduceMemoryCallBack_goToHomeVillage、DailyQuest activate: 等原版发包点也可能在调度器回调里,都违反「帧栈上不发宿主消息」。
/// 截下的白名单请求:(NetworkManager, 命令号, 请求体拷贝)。★锁绝不跨 msg_send 持有。
static PENDING_REQUESTS: Mutex<Vec<(u32, u32, Vec<u8>)>> = Mutex::new(Vec::new());
/// PENDING_REQUESTS 非空(受理点每轮只读这一个原子)。
static REQUESTS_PENDING: AtomicBool = AtomicBool::new(false);
/// 本轮有新回包入队;受理点在本轮末尾把它转成 LOOPBACK_ARMED,下一轮受理点再喂(等价原 afterDelay:0 下一轮才到)。
static LOOPBACK_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 上一轮入队的回包本轮受理点要喂。
static LOOPBACK_ARMED: AtomicBool = AtomicBool::new(false);
/// 回包要喂给的 NetworkManager(入队时记下,与原来 performSelector 的接收者相同)。
static LOOPBACK_NM: AtomicU32 = AtomicU32::new(0);
/// -[GameManager startGame:] 的离线进村补发(1049 与 startgame_resend_offline)待受理。
static STARTGAME_RESEND_PENDING: AtomicBool = AtomicBool::new(false);

/// [2026-09-16] F2-06 读到坏档且原样备份失败:本会话 save_state 一律跳过,不拿默认值覆盖原文件。
static ACT_SAVE_BLOCKED: AtomicBool = AtomicBool::new(false);
/// [2026-09-16] F2-06 「跳过写盘」提示是否已打过(防刷屏)。
static ACT_BLOCK_LOGGED: AtomicBool = AtomicBool::new(false);
/// [2026-09-16] F2-06 本会话已备份过的坏档内容指纹((1<<32)|fnv1a;0 = 没有)。
/// 备份之后、下一次保存之前还可能有只读不写的 load_state(比如烟花判定),同一份坏档不重复备份出一串 .corrupt-*。
static ACT_CORRUPT_BACKED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// [2026-09-16] F2-05 时间旅行隔离缓存:偏移非 0 时 load_state 首次从盘读进这里,之后只读写它,save_state 不落盘。
    static TT_STATE: RefCell<Option<ActState>> = const { RefCell::new(None) };
}

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
                // [2026-09-16] E-03 黄金岛每日任务:请求入口 + 排到运行循环的自用选择子(宿主侧构造列表)
                | "getDailyTaskListFromServerWithSceneId:"
                | "moleActivityIslandDailyQuest"
                // [2026-09-24 第四轮 K6 I8-4] 黄金岛限时折扣 1073:请求入口 + 排到运行循环的自用选择子(宿主侧构造 DiscountInfo)
                | "getDiscountObjectsListFormServerWithMapId:"
                | "moleActivityIslandDiscount"
                // [2026-09-25 第五轮遗留 V] VIP 信息 1084 的请求入口(离线只置排队标志,由运行循环受理点补回包分发)
                | "getVipInfo"
        ),
        // [补完 2026-09-15] F2-2 回环喂 1049 时吞掉 GameManager 的推广弹窗分发;离线进村时补发 1049
        "GameManager" => sel == "onCommandReceived:" || sel == "startGame:",
        "UserInfoLayer" => sel == "checkActivityStatus",
        "ActionCenterLayer" => sel == "changeActionLayer:",
        // [2026-09-16] X4-02 三个活动层除 checkNetWork 外,再加时间旅行期间要在本地扣款之前拦下的付费入口
        "DailySignLayer" => matches!(
            sel,
            "checkNetWork" | "onButtonPatchSign:" | "onChooseUseVipGold"
        ),
        "SealExchangeLayer" => matches!(
            sel,
            "checkNetWork" | "onButtonExchange:" | "onChooseConfirm"
        ),
        "SeabedSeekingTreasureMainLayer" => matches!(
            sel,
            "checkNetWork" | "onDigShellClick:" | "onSureRefreshClick" | "onExchangeRewardClick:"
        ),
        // [2026-10-04 第八轮 R8-C1] 海王贝奖励面板自己的网络检查(见 intercept 里的 checkNetWork 臂)
        "GetItemRewardFromHaiwangLayer" => sel == "checkNetWork",
        "GameData" => sel == "isHighPriceRecycleTime" || sel == "hasFireworkGift",
        // [2026-09-16] A1-01 春节烟花真正开播时才记当天额度
        "FireworkLayer" => sel == "showFireWorkFullScreen",
        // [2026-09-24 第四轮 K6 N-D4-1] 岛上 VIP 在线奖励:离线时奖励表为空就不开空奖励板,改弹原版离线提示。
        //   OnlineTimeManager 不在 mole_cheats 的 CLASSES 里,靠这里 OR 进 intercept_wants,release 下才不会静默失效。
        "OnlineTimeManager" => sel == "showRewardLayer",
        _ => false,
    }
}

/// 前置拦截。None = 不归本模块管;Some(true) = 已吞掉调用(返回值寄存器已写好);
/// Some(false) = 做完副作用后放行真方法(若发过宿主 msg_send,返回前必须恢复 r0-r3)。
pub fn intercept(env: &mut Environment, class: &str, sel: &str) -> Option<bool> {
    // [扫描修 2026-09-15] 回环自用的选择子要最先处理:NetworkManager 并不实现它,放行会 unrecognized selector。
    // [2026-10-03] 宿主已不再排 moleActivityLoopback(回环改由运行循环受理点 run_loop_poll 喂包),这里只保留吞臂防御,
    //   不做任何事:待喂的包仍在队列里,由受理点照常处理。
    if class == "NetworkManager" {
        if sel == "moleActivityLoopback" {
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        // [2026-09-16] E-03 黄金岛每日任务。岛上会话里 mole_cheats 吞掉所有 sendPacket:commandId:,本模块的回环在岛上也整体停用
        //   (上面 moleActivityLoopback 会清队列),两边都不放开;改在请求入口接住参数 10,排一次运行循环回调,由宿主侧照
        //   parseDailyTaskListWithSceneId:pos:len: 的做法构造 DailyQuestList 交给原版。不在当前调用栈里同步做:岛上的调用点之一
        //   -[HolidayVillageLayer onEnter](0x239484)跑在切场景的 drawScene 帧栈上。参数 1(主村)照常放行,由 handle_send_packet 应答。
        //   条件不满足时只读过寄存器、没发消息,落到下面照常处理。
        //   [2026-09-16] 复审:并成一个条件,免得 lint.sh 的 clippy --deny warnings 报 collapsible_if。
        if sel == "getDailyTaskListFromServerWithSceneId:"
            && !env.options.network_access
            && crate::mole_cheats::island_session_active()
            && env.cpu.regs()[2] == 10
        {
            // [2026-10-03 第六波] 帧栈上只置原子标志(不再在这里发 performSelector:withObject:afterDelay:),
            //   由运行循环受理点 run_loop_poll 里的 island_deferred_poll 在当前调用栈整个返回之后构造(见 ISLAND_DAILY_PENDING)。
            ISLAND_DAILY_PENDING.store(true, O);
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        if sel == "moleActivityIslandDailyQuest" {
            // NetworkManager 并不实现它,任何状态下都必须接住。
            // [2026-10-03 第六波] 请求入口已改为置标志 + 运行循环受理(island_deferred_poll),本臂只为接住
            //   旧写法排进运行循环、尚未执行的回调(同一会话内不会再新排),照旧按当时状态构造或放弃。
            if !env.options.network_access && crate::mole_cheats::island_session_active() {
                island_daily_quest_apply(env);
            } else {
                log!("[ACTIVITY] 黄金岛每日任务:回调到达时已不在岛上会话,放弃构造");
            }
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        // [2026-09-24 第四轮 K6 I8-4] 黄金岛限时折扣 1073。-[HolidayVillageLayer onEnter] 在网络门之后无条件
        //   0x23946a `[nm getDiscountObjectsListFormServerWithMapId:10]`(r2 在 0x239460 硬编码 10;参数 1 那条分支 0x226cf0
        //   全二进制无调用者)→ -[NetworkManager getDiscountObjectsListFormServerWithMapId:]@0x226cd0 在 0x226d30 发 1073,
        //   岛上会话里被 mole_cheats 的 sendPacket 吞包臂吃掉,NewSceneData.discountObjDataArr_ 永远是空的,岛商店从不打折。
        //   照 E-03 每日任务同一写法:接住参数 10,排一次运行循环回调,由宿主侧照 parseDiscountListWithSceneId:pos:len: 岛分支
        //   (0x2272fc 起)直接构造 DiscountInfo 交给 NewSceneData(不走报文,免得漏了首字节 0x01 岛标志被 0x227184 当成主村包
        //   写进 GameData)。不在当前调用栈同步做:onEnter 跑在切场景的 drawScene 帧栈上。
        //   MOLE_DISCOUNT=off 时不接,放行原版(包照旧被吞,等于原离线行为:岛上无折扣)。条件不满足时只读过寄存器、没发消息。
        if sel == "getDiscountObjectsListFormServerWithMapId:"
            && !env.options.network_access
            && crate::mole_cheats::island_session_active()
            && env.cpu.regs()[2] == 10
            && !discount_disabled()
        {
            // [2026-10-03 第六波] 同上:帧栈上只置标志,运行循环受理(见 ISLAND_DISCOUNT_PENDING)。
            ISLAND_DISCOUNT_PENDING.store(true, O);
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        if sel == "moleActivityIslandDiscount" {
            // NetworkManager 并不实现它,任何状态下都必须接住。
            // [2026-10-03 第六波] 请求入口已改为置标志 + 运行循环受理(island_deferred_poll),本臂只为接住
            //   旧写法排进运行循环、尚未执行的回调(同一会话内不会再新排),照旧按当时状态构造或放弃。
            if !env.options.network_access && crate::mole_cheats::island_session_active() {
                island_discount_apply(env);
            } else {
                log!("[ACTIVITY] 黄金岛折扣:回调到达时已不在岛上会话,放弃构造");
            }
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        // [2026-09-25 第五轮遗留 V] VIP 信息 1084。-[NetworkManager getVipInfo]@0xeac2c(v8@0:4)本体只有 sendPacket:nil commandId:0x43c
        //   (0xeac38),没有任何前置门或副作用。在线时回包经 parseData 进 parseVipInfo:pos:len:@0x1c0b9c 写本地三值,再在公共尾
        //   0xe6d9e..0xe6de4 先给 delegateGameData 发 onCommandReceived:(主村 1084 臂 0x239f6)、再给 delegateNewSceneGameData 发
        //   onNewSceneGameDataCommandReceived:(岛 1084 臂 0x23e7c4)。离线时主村这一包被真 sendPacket 在 isReachable_=0 处空过,
        //   岛上被 mole_cheats 的 sendPacket 吞包臂吃掉,两条分发臂从不执行:VIP 成就(主村 AchievementControl 0x800、岛 NewSceneAchievement 0x1000,各是唯一入口)永远解锁不了,
        //   HUD 的 VIP 徽章(updateUI4VIP)不刷新,贝壳树(rescheduleTree 唯一调用点)不重排。
        //   这里只补「分发」那一半,不走 parseVipInfo:它在 0x1c0c96 见包里等级低于本地 vipLevelWithNewType 就弹作弊警告,
        //   0x1c0cea/0x1c0d1c/0x1c0d5a 用包里的值覆盖本地三值,0x1c0e34 起还会判首充礼包;本地三值由 mole_items 的 vip.dat 侧档负责。
        //   离线一律接住(主村、岛上都会发),只置排队标志(纯原子操作,不发任何宿主消息),由 ns_run_loop 主线程
        //   perform 相位之后的 vip_info_poll 执行两条臂:调用点里 -[HolidayVillageLayer onEnter] 0x23949c 跑在切场景的 drawScene 帧栈上,
        //   不能就地发一串宿主消息(规则③)。在线模式不接:只读过 options,落到下面照常处理。getVipInfo 返回 void,吞掉后写 r0=0。
        //   原版调用点(selref 0xadc680 共 14 处,下列为 blx 地址,日志里的 LR = blx+4 带 Thumb 位):进岛 -[HolidayVillageLayer onEnter]
        //   0x23949c(LR 0x2394a1)、岛建设按钮 -[NewSceneVillageMenuLayer onButtonBuildSelected:] 0x25ab6e(LR 0x25ab73)、主村建设按钮
        //   -[VillageMenuLayer onButtonBuildSelected:] 0x61d78(LR 0x61d7d)、关 VIP 面板 -[VIPLayer detach] 0x37f730(LR 0x37f735;
        //   来自关闭按钮,或 -[VIPLayer showWithTarget:selector:] 0x37f526 注册的 NSNotificationCenter 观察者 onHideView 0x37f648 → detach)、
        //   活动公告 -[ActivityBulletinLayer onJoinInActivity] 0x3aa7fa、TestLayer/NewSceneTestLayer 调试层(init 与 VIP 值加减按钮);离线走不到的:
        //   进村 -[GameManager startGame:] 0x19ca0(isConnected 门内,由 startgame_resend_offline 补)、内购成功 0x117dcc 与
        //   -[GameData addAlreadyPurchaseVipgoldWithPurchaseInfo:] 0x7f284(SHELLHOOK 整段绕开,由 mole_items::on_shells_purchased 补)、
        //   好友村回家 -[FriendsVillageLayer reduceMemoryCallBack_goToHomeVillage] 0x109172(第八轮 R8-B1 起主村离线也能进好友村,
        //   这条回家路径离线可达:跑在回家的调度帧上,本臂只置排队标志,运行循环受理点分发)。
        if sel == "getVipInfo" && !env.options.network_access {
            let lr = env.cpu.regs()[14];
            request_vip_info(env, &format!("离线接住 getVipInfo(调用方 LR={lr:#x})"));
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

    // [2026-09-24 第四轮 K6 N-D4-1] 岛上 VIP 在线奖励(OnlineTimeManager)离线永远是空奖励板。奖励表 reward_list_ 唯一的赋值在
    //   setNextReward_time:NextReward_list:NextReward_level:@0x38aac8,唯一上游是 setNextOnlineRewardData@0x38aa1c,它只在 1088 回包分发
    //   -[HolidayVillageLayer onNewSceneGameDataCommandReceived:] 0x23e946 被调;last_rew_data_ 靠 1062 的 localVipRewardData,离线同样没有。
    //   于是 showRewardLayer@0x38b0cc 在 0x38b10a 见 [last_rew_data_ reward_array] count 为 0(last_rew_data_ 为 nil)、走 0x38b15e
    //   以 initWithType:2 rewardsList:reward_list_(nil) 建出「连续 0 次、剩余 0 小时 0 分 0 秒」、一个格子都没有的空板,
    //   倒计时也不走(startOrPauseCaculateLoginTime: 0x38abde 见 reward_time_==0 不调度)。
    //   原版奖励表客户端里没有;私服 vip.rs 的表是猜的、time 字段还当成纪元时间(客户端当逐秒递减的倒计时,0x38aca6),不能移植,
    //   也不自己编。采用退路:离线且两项都为空时不开空板,改弹原版文案 ACTION_CENTER_NETWARNING(「该功能需要联网才能使用哦!」),
    //   写法照 mole_cheats 的 onButtonFriendSelected:/onButtonAdwallSelected: 两条离线提示臂(弹框类/文案缺失就恢复 r0-r3 放行原版,最坏空板)。
    //   showRewardLayer 全二进制只有 -[NewSceneVillageMenuLayer onButtonOnlineRewardSelected:] 0x25a8f6 一个调用点,在
    //   checkCanStartVipOnlineRewardFunc(0x25a8aa,VIP>3 才放行)之后:VIP<4 时原版的 NO_VIP_LEVEL_ONLINE 提示不受影响,宿主不用自己判 VIP。
    //   菜单按钮回调栈,不在 drawScene/mainLoop 帧栈上,可以发宿主消息。getVipDailyReward:/sendOnlineLevelToServer/setLastOnLineReward
    //   离线本来就是空过(包被岛吞包臂吃掉),不拦。在线模式零影响。
    if class == "OnlineTimeManager" && sel == "showRewardLayer" && !env.options.network_access {
        let saved = save_regs(env);
        let otm: id = Ptr::from_bits(env.cpu.regs()[0]);
        if online_reward_board_would_be_empty(env, otm) && show_offline_net_warning_box(env) {
            log!("[ACTIVITY] 黄金岛 VIP 在线奖励:离线拿不到 1088 奖励表(reward_list_/last_rew_data_ 皆空)→ 弹「该功能需要联网」提示,不开空奖励板");
            env.cpu.regs_mut()[0] = 0;
            return Some(true);
        }
        // 奖励表不空(不该离线出现)或弹框没发出去:恢复寄存器,放行原版。
        restore_regs(env, saved);
        return None;
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
            // [2026-09-16] X4-02 时间旅行中不放行 -[EditMenuLayer onButtonOkSelected:] 珍珠兑换放置确认的网络门。原版门失败时
            //   弹"需要联网才能领取奖励哦!"并 onButtonDeleteSelected: 删掉刚放下的奖励(0x4eb20 → 0x4ebd4 → 0x4ebb4..0x4ebcc),
            //   1221 按 exChangeRewardButtonTag(tbb@0x4eb60)分四处取选择子(0x4eb7c/0x4ec1a/0x4ec32/0x4ec4a),共用 0x4ec4e 一条 blx 发出,
            //   全在两道门(0x4eb18 isReachable、0x4eb36 isConnected)之后;门失败时不发 1221、珍珠不扣,已放下的奖励被删掉。兑换层入口 onExchangeRewardClick:
            //   另有拦截,这里兜的是旅行开始前就已选好奖励、正在摆放的那一次。只读寄存器与原子变量,没发消息,返回 None 时寄存器原样;
            //   主村离线时 mole_cheats 只在登录门与进岛/在岛窗口里强制 isReachable,放行后真 getter 读到的是离线值 0。
            if lr_is(env, SITE_EDIT_SEABED_REACHABLE) && time_travel_active() {
                log!("[ACTIVITY] 时间旅行中:珍珠兑换放置确认的网络门不放行 → 原版「需要联网才能领取奖励哦!」并删掉刚放下的奖励(不发 1221、不扣珍珠)");
                return None;
            }
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
            // [2026-09-16] X4-02 对称兜底(正常到不了:时间旅行中同一分支上面的 isReachable 门已先失败)。只读寄存器,没发消息。
            if lr_is(env, SITE_EDIT_SEABED_CONNECTED) && time_travel_active() {
                return None;
            }
            // [2026-10-04 第八轮 R8-C2] 占卜三道门(「修复占卜功能」开着时;关着时照原版得到离线提示)。
            let divine = crate::mole_cheats::is_on("fix_divine")
                && (lr_is(env, SITE_DIVINE_ENTER_CONNECTED)
                    || lr_is(env, SITE_DIVINE_FIRST_CONNECTED)
                    || lr_is(env, SITE_DIVINE_COST_CONNECTED));
            if divine
                || lr_is(env, SITE_UIL_CONNECTED)
                || lr_is(env, SITE_BULLETIN_CONNECTED)
                || lr_is(env, SITE_SIGN_SHOW_CONNECTED)
                || lr_is(env, SITE_FOREGROUND_DAILY_CONNECTED)
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
        // 三个层自己的 checkNetWork(签到层 0x39a3fc / 脚印兑换层 0x39c7d4 / 海底寻宝层 0x2c28b8;第四个海王贝奖励面板见下):
        // 原版在无网时弹"该功能需要联网"或 showNetWorkError 并返回 NO。离线由回环服务器代答,直接返回 YES。
        ("DailySignLayer", "checkNetWork")
        | ("SealExchangeLayer", "checkNetWork")
        | ("SeabedSeekingTreasureMainLayer", "checkNetWork") => {
            env.cpu.regs_mut()[0] = 1;
            Some(true)
        }
        // [2026-10-04 第八轮 R8-C1] 第四个:海王贝奖励面板。挖到海王贝(贝壳类型 4,-[SeabedSeekingTreasureMainLayer onDigShellClick:]
        //   0x2c203a → 0x2c205c 奖励层 setVisible:YES,0x2c2080 displayUIWithBgSpr: 里 generateRandomRewardId 写 rewardId_)后,
        //   回环 1220 已把这只贝壳换掉并写进 mole_activity.dat;玩家点奖励面板时 -[GetItemRewardFromHaiwangLayer ccTouchBegan:withEvent:]
        //   @0x3fa544 先在 0x3fa5a2 调本层 checkNetWork@0x3fa710(判 isReachable 0x3fa740 / isConnected 0x3fa75e),离线失败就
        //   showNetWorkError(GET_ACTION_CENTER_INFO_ERROR「无法获取当前的活动中心数据」,回调 closeMainLayer)并在 0x3fa5ac 返回,
        //   真正的发奖(0x3fa5cc closeMainLayer → 0x3fa606 initActivityGiftNum:1 → 0x3fa626 onAddActivityGiftToMap:rewardId_ →
        //   0x3fa654 setIsGetDigHaiwangShellReward:1)整段走不到,奖励永久丢失。同上口径返回 YES,让原版自己关页、发活动礼包、进摆放。
        //   触摸回调不在帧栈上;只写 r0,不发消息。岛上与在线由 intercept 入口「只在离线主村生效」的总闸排除。
        ("GetItemRewardFromHaiwangLayer", "checkNetWork") => {
            log!("[ACTIVITY] 海王贝奖励面板 checkNetWork:离线由回环代答,返回 YES,照原版关页并把奖励发到庄园");
            env.cpu.regs_mut()[0] = 1;
            Some(true)
        }

        // ── [2026-09-16] X4-02 时间旅行期间,在客户端本地扣款/发物之前拦下活动中心的付费入口 ──
        // F2-05 让旅行期间的侧档只写内存,但下面这些操作的扣款与发物由客户端本地完成、照常经 saveUserInfoData/saveMapData 进主档。
        // 重启后侧档回到旅行前:补签格、5 个贝壳、珍珠回滚而钱已扣;脚印/珍珠没扣而物品已入账,同一档位还能再兑。
        // 不能在 sendPacket 处吞包(发包前已扣款);入口逐个反汇编确认都在扣款之前:
        // - -[DailySignLayer onButtonPatchSign:]@0x39907c:needToPatchDay 非 0 且本月补签不足 3 次时,0x399406 addGold:-needGoldNum
        //   后直接 completePatchSign(发 1116);之后改弹「用贝壳补签」确认框,回调 onChooseUseVipGold@0x399ab8 在 0x399b56 addVipGold:-needShellsNum。
        // - -[SealExchangeLayer onButtonExchange:]@0x39c344 只弹「确定兑换」框;回调 onChooseConfirm@0x39c5d0 在 0x39c748 调
        //   -[WrapperManager addIceCreamActivityRewardToMap:num:],晶玉类当场 addInvisibleReward:num: 入账,之后 0x266636 才发 1120。
        // - -[SeabedSeekingTreasureMainLayer onSureRefreshClick]@0x2c2454:0x2c250a addVipGold:-3 之后才在 0x2c2550 发 1223。
        // - -[SeabedSeekingTreasureMainLayer onExchangeRewardClick:]@0x2c26b4 打开珍珠兑换层;选中奖励后 onChangeItemReward:@0x23fc88
        //   直接 addItemObjectToMap: 把物品放进村庄。
        // selref 核对:这些选择子只出现在 displayUI/updatePatchSignMenu/addAndUpdateExchangeMenu 建菜单项、各自的确认框,以及
        // onDigShellClick: 内部转发,都是菜单项或 MessageBox 按钮回调,不在 drawScene 帧栈上。没在旅行中只做一次原子读就返回 None,寄存器未动。
        ("DailySignLayer", "onButtonPatchSign:")
        | ("DailySignLayer", "onChooseUseVipGold")
        | ("SealExchangeLayer", "onButtonExchange:")
        | ("SealExchangeLayer", "onChooseConfirm")
        | ("SeabedSeekingTreasureMainLayer", "onSureRefreshClick")
        | ("SeabedSeekingTreasureMainLayer", "onExchangeRewardClick:") => {
            if !time_travel_active() {
                return None;
            }
            // 确认框回调(onChooseUseVipGold / onChooseConfirm / onSureRefreshClick)只记日志不弹框,原因见 block_paid_action_in_time_travel。
            let (what, show_box) = match sel {
                "onButtonPatchSign:" => ("补签", true),
                "onChooseUseVipGold" => ("贝壳补签确认", false),
                "onButtonExchange:" => ("脚印兑换", true),
                "onChooseConfirm" => ("脚印兑换确认", false),
                "onSureRefreshClick" => ("刷新贝壳确认", false),
                _ => ("珍珠兑换", true),
            };
            block_paid_action_in_time_travel(env, what, show_box);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }
        ("SeabedSeekingTreasureMainLayer", "onDigShellClick:") => {
            // -[SeabedSeekingTreasureMainLayer onDigShellClick:]@0x2c1c64 按 [sender tag] 分派(tbh@0x2c2024,表项已按指令编码复算):
            // - 0..4 挖第几个贝壳:摩尔豆贝 0x2c2010 addGold:-1000,两种晶玉贝 0x2c1da0/0x2c1dba、0x2c1e4a/0x2c1e66 addCoupon:number: 扣晶玉,
            //   海王贝不扣钱但由海王贝奖励层发物品;这些都在 0x2c20c6/0x2c21bc 发 1220 之前。
            // - 5 活动规则:免费,放行。
            // - 6 弹「刷新贝壳」确认框(回调 onSureRefreshClick):提前拦掉,不让框弹出。
            // - 7 转发 onExchangeRewardClick::由上面那条臂拦;tag>4 时原版不取贝壳也不扣费。
            // tag 是 -[CCNode tag]@0x2d466c 的平凡 ivar 读(CCMenuItemSpriteIndependent 只覆写了 selected/unselected/dealloc),
            // 直接读 ivar 不发消息;sender 为 nil 时原版 [nil tag]=0 走挖第 1 个贝壳,这里同样按 0 算。放行路径寄存器未动。
            if !time_travel_active() {
                return None;
            }
            let sender: id = Ptr::from_bits(env.cpu.regs()[2]);
            let tag = read_ivar_u32(env, sender, SLOT_CCNODE_TAG).unwrap_or(0);
            let what = match tag {
                0..=4 => "挖贝壳",
                6 => "刷新贝壳",
                _ => return None,
            };
            block_paid_action_in_time_travel(env, what, true);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
        }

        // ── 回环服务器主入口 ──
        ("NetworkManager", "sendPacket:commandId:") => {
            // [2026-10-03] 原调用栈上只决定接不接、截下请求体拷贝,应答由运行循环受理点算(answer_request,见 PENDING_REQUESTS)。
            //   这里不发任何宿主消息:请求体从宿主 NSData 直接拷字节。不认识的命令号(以及 MOLE_DISCOUNT=off 时的 1049)
            //   放行真 sendPacket:commandId:(离线等于空过),只读过寄存器,寄存器未动——真方法在判 isReachable_ 之前就写
            //   self+204、给 self+172 发 setSendFlag:(0xe235e/0xe2362),r0 必须仍是 NetworkManager。
            let regs = env.cpu.regs();
            let (nm_bits, body, cmd): (u32, id, u32) = (regs[0], Ptr::from_bits(regs[2]), regs[3]);
            if !loopback_accepts(cmd) {
                return None;
            }
            let copied = if body == nil {
                Some(Vec::new())
            } else {
                crate::frameworks::foundation::ns_data::try_copy_bytes(env, body)
            };
            let req = copied.unwrap_or_else(|| {
                log!(
                    "[ACTIVITY] 回环截包 cmd={}:请求体不是宿主 NSData,按空请求体处理",
                    cmd
                );
                Vec::new()
            });
            match PENDING_REQUESTS.lock() {
                Ok(mut q) => q.push((nm_bits, cmd, req)),
                Err(_) => return None,
            }
            REQUESTS_PENDING.store(true, O);
            env.cpu.regs_mut()[0] = 0;
            Some(true)
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
            // 所以离线主村原版永远不会发 1049。这里在进村时(商店数组已由 load:type: 加载好)照原版同一个入口
            // 补发一次:[[NetworkManager sharedInstance] getDiscountListFromServer] → sendPacket:1049 → 回环应答。
            // [2026-09-16] E-02 / A1-01 / E-03 同一个门内还有公告 1058、春节烟花 1112、每日任务 1074,1049 之后按原版顺序补发
            //   (见 startgame_resend_offline)。
            // [2026-10-03] 从岛回村时本臂跑在 CCScheduler 帧栈上(-[NewBaseLoading endLoading] → … → -[SceneMannager endLoadingScene]
            //   loadMainVillageScene → startGame → startGame:),以前在这里就地发一串宿主消息。现在只置标志,补发由运行循环受理点
            //   (run_loop_poll → startgame_resend_poll)在 startGame: 整个返回之后执行;原版这几条也只是发包,回包要等一次网络往返,
            //   startGame: 后半段不读它们的结果(isUserSelectedNoticeBoardMenu 的读者只有 onCommandReceived:、超时控制与公告解析)。
            //   只写了一个原子,寄存器未动。
            STARTGAME_RESEND_PENDING.store(true, O);
            None
        }
        ("NetworkManager", "getDiscountListFromServer") => {
            // 原版 0x1cb174:state==4 直接返回不发包;其它状态走 sendPacket:commandId:1049,由 answer_request 应答。
            // 离线状态机按理到不了 4,这里只是兜底(state==4 时照样本地应答并记日志)。放行路径只读内存,寄存器未动。
            if discount_disabled() {
                return None;
            }
            let nm: id = Ptr::from_bits(env.cpu.regs()[0]);
            if read_ivar_u32(env, nm, SLOT_NM_STATE) != Some(4) {
                return None;
            }
            log!("[ACTIVITY] 限时折扣:NetworkManager.state==4,原版不会发 1049,本地兜底应答");
            // [2026-10-03] 与 sendPacket:commandId: 臂同一写法:只截下请求,应答由运行循环受理点算(不在原调用栈上发宿主消息)。
            match PENDING_REQUESTS.lock() {
                Ok(mut q) => q.push((nm.to_bits(), CMD_DISCOUNT_LIST, Vec::new())),
                Err(_) => return None,
            }
            REQUESTS_PENDING.store(true, O);
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
        // ── [2026-09-16] A1-01 春节烟花:真开播才记当天额度 ──
        ("FireworkLayer", "showFireWorkFullScreen") => {
            // -[FireworkLayer showFireWorkFullScreen]@0x3e3198 全二进制只有 -[GameManager onCommandReceived:] 的 1112 臂(0x23ee2)调用,
            // 离线只可能来自回环喂进去的 1112。以前在 1112 入队前就写 firework_day,场景没就绪、烟花没放出来也白扣一天;改到这里记。
            // save_state 要发 dataWithBytes:length: / writeToFile:atomically:,放行真方法前恢复 r0-r3。
            let saved = save_regs(env);
            let today = local_date(env);
            let mut st = load_state(env);
            if st.firework_day != today.ymd() {
                st.firework_day = today.ymd();
                save_state(env, &st);
            }
            log!(
                "[ACTIVITY] 春节烟花开播 showFireWorkFullScreen:记下今天({})已放过",
                today.ymd()
            );
            restore_regs(env, saved);
            None
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

/// 回环包入队,下一轮运行循环受理点再喂(见 run_loop_poll)。
/// [2026-10-03] 只读 guest 内存、写宿主队列与原子,不发任何宿主消息,任何栈上都能调。
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
    // 不在发包的调用栈里同步解析(那样回包处理会早于调用方后续的 showLoadingLayer 等)。
    // [2026-10-03] 以前在这里 performSelector:withObject:afterDelay:0 排 moleActivityLoopback(下一轮 perform 相位喂);
    //   现在只置标志,本轮受理点把它转成「下一轮喂」,时机相同,但不再在原调用栈上发消息。
    LOOPBACK_NM.store(nm.to_bits(), O);
    LOOPBACK_REQUESTED.store(true, O);
}

/// 运行循环安全点:把队列里的包追加进 buffer_ 并调原版解析。
fn run_loopback(env: &mut Environment, nm: id) {
    let queued: Vec<Vec<u8>> = match LOOPBACK_QUEUE.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    if nm == nil {
        return;
    }
    // [2026-09-16] A1-01 春节烟花 1112 单独拿出来按场景就绪与否决定喂不喂,其它命令照常立即喂,不被它拖住。
    let (firework_pkts, mut packets): (Vec<Vec<u8>>, Vec<Vec<u8>>) = queued
        .into_iter()
        .partition(|p| packet_cmd(p) == CMD_FIREWORK);
    let fed_firework = feed_or_defer_firework(env, firework_pkts, &mut packets);
    if packets.is_empty() {
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
        if fed_firework {
            FIREWORK_IN_FLIGHT.store(false, O);
        }
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
    if fed_firework {
        // 解析链已同步跑完 onCommandReceived:;真开播时 showFireWorkFullScreen 钩子已记下今天。
        FIREWORK_IN_FLIGHT.store(false, O);
    }
}

/// 整包里的命令号(24 字节头的第 2 个 u32;build_packet 组的包至少 40 字节)。
fn packet_cmd(pkt: &[u8]) -> u32 {
    if pkt.len() < 8 {
        return 0;
    }
    u32::from_le_bytes([pkt[4], pkt[5], pkt[6], pkt[7]])
}

/// [2026-09-16] A1-01 决定本轮喂不喂 1112 烟花包:场景就绪就追加进 packets 并返回 true;没就绪就放进 FIREWORK_DEFERRED,
/// 约 1 秒后由运行循环受理点重查(firework_retry_due),最多 FIREWORK_RETRY_MAX 次,超限丢弃(不写 firework_day,下次进村再试)。
/// 为什么要等:startGame: 由 -[LoadingLayer loadTarget](0x12ee32)调用,之后才切到村庄场景;回包下一轮运行循环就到,
/// 而 -[GameManager onCommandReceived:] 的 1112 臂要 curSceneId==1(0x23e90)且 runningScene 有 tag 1 子节点(0x23ed0)才开播,
/// 否则静默退出。只在运行循环受理点里调用(不在帧栈上)。
fn feed_or_defer_firework(
    env: &mut Environment,
    fresh: Vec<Vec<u8>>,
    packets: &mut Vec<Vec<u8>>,
) -> bool {
    let mut slot = firework_slot_take();
    if slot.is_none() {
        // FIREWORK_IN_FLIGHT 挡住了重复回包,同一轮理论上最多一个 1112;万一有多个只留第一个。
        slot = fresh.into_iter().next().map(|p| (p, 0u32, Instant::now()));
    }
    let Some((pkt, tries, not_before)) = slot else {
        return false;
    };
    let now = Instant::now();
    if tries > 0 && now < not_before {
        // 别的命令顺路触发的回环轮次:还没到下次检查时间,原样放回,不计次(到点后受理点会再来查)。
        firework_slot_put(Some((pkt, tries, not_before)));
        return false;
    }
    if firework_scene_ready(env) {
        log!(
            "[ACTIVITY] 春节烟花 cmd=1112:村庄场景已挂好 FireworkLayer(第 {} 次检查),喂包",
            tries + 1
        );
        packets.push(pkt);
        return true;
    }
    if tries + 1 >= FIREWORK_RETRY_MAX {
        log!(
            "[ACTIVITY] 春节烟花 cmd=1112:检查 {} 次场景里仍没有 FireworkLayer,丢弃回包(不记今天的额度,下次进村再试)",
            FIREWORK_RETRY_MAX
        );
        FIREWORK_IN_FLIGHT.store(false, O);
        return false;
    }
    log!(
        "[ACTIVITY] 春节烟花 cmd=1112:村庄场景还没就绪,1 秒后重查({}/{})",
        tries + 1,
        FIREWORK_RETRY_MAX
    );
    // [2026-10-03] 以前在这里 performSelector:withObject:afterDelay:1.0 排下一次重查;现在只记下最早重查时刻,
    //   由运行循环受理点每轮看到点没有(firework_retry_due)。
    firework_slot_put(Some((pkt, tries + 1, now + Duration::from_secs(1))));
    false
}

/// [2026-10-03] 取走烟花延迟槽(同时清 FIREWORK_WAITING)。
fn firework_slot_take() -> Option<(Vec<u8>, u32, Instant)> {
    let slot = match FIREWORK_DEFERRED.lock() {
        Ok(mut d) => d.take(),
        Err(_) => None,
    };
    FIREWORK_WAITING.store(false, O);
    slot
}

/// [2026-10-03] 放回 / 清空烟花延迟槽(同时维护 FIREWORK_WAITING)。
fn firework_slot_put(slot: Option<(Vec<u8>, u32, Instant)>) {
    let waiting = slot.is_some();
    if let Ok(mut d) = FIREWORK_DEFERRED.lock() {
        *d = slot;
    }
    FIREWORK_WAITING.store(waiting, O);
}

/// [2026-10-03] 烟花延迟槽里有包、且到了下次检查时刻。平时只有一次原子读。
fn firework_retry_due() -> bool {
    if !FIREWORK_WAITING.load(O) {
        return false;
    }
    match FIREWORK_DEFERRED.lock() {
        Ok(d) => d
            .as_ref()
            .is_some_and(|(_, _, not_before)| Instant::now() >= *not_before),
        Err(_) => false,
    }
}

/// [2026-09-16] A1-01 与 -[GameManager onCommandReceived:] 1112 臂同一判据:[[SceneMannager sharedManager] curSceneId]==1,
/// 且 [[[CCDirector sharedDirector] runningScene] getChildByTag:1] 存在并且是 FireworkLayer(宿主侧沿 isa 链判,等价 isKindOfClass:;
/// InGameScene init 在 0x18722/0x18736 以 tag 1 挂 FireworkLayer)。加载阶段 tag 1 子节点可能是别的层,不能只判非空。
fn firework_scene_ready(env: &mut Environment) -> bool {
    let sm = singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return false;
    }
    let cur_sel = sel_named(env, "curSceneId");
    let cur: i32 = msg_send(env, (sm, cur_sel));
    if cur != 1 {
        return false;
    }
    let director = singleton(env, "CCDirector", "sharedDirector");
    if director == nil {
        return false;
    }
    let rs_sel = sel_named(env, "runningScene");
    let scene: id = msg_send(env, (director, rs_sel));
    if scene == nil {
        return false;
    }
    let gct = sel_named(env, "getChildByTag:");
    let child: id = msg_send(env, (scene, gct, 1i32));
    crate::mole_items::is_kind_of(env, child, "FireworkLayer")
}

// ─────────────────────────────── 时间与节日 ───────────────────────────────

/// 当前 CFAbsoluteTime 整秒(含时间旅行偏移)。
/// [2026-09-25 第五轮遗留 MISC-4] 取时改用 crate::mole_cheats::now_cf_secs(),与离线 -[NewSceneTimer getCurrentServerTime] 臂同一单调时钟
/// (那里返回 now_cf_secs().max(0.0) as u32,这里同样截断取整)。根因:游戏侧拿来与本模块对账的都是 getCurrentServerTime——签到
/// -[DailySignLayer getServerTime] 0x39a6c8/0x39a6d8 → 0x39a704 拆日期,海底寻宝 -[SeabedSeekingTreasureMainLayer displayUI] 0x2c0b4a
/// 算 5 分钟冷却,每日任务回包时间经 updateDailyQuestListWithCurrentServerData: 在 0x82fe6 算截止时间;以前这里直读墙钟
/// cf_absolute_time_now,第四轮 K4 把 getCurrentServerTime 改成单调后,进程内宿主时间往回拨时两边日界、冷却差出回拨量(最多一天)。
/// 正常运行时单调时钟等于墙钟;只有进程内回拨时它不倒退,这时与 NSDate/CFAbsoluteTimeGetCurrent(墙钟)不再相同。
/// 时间旅行偏移仍即时生效(now_cf_secs 里的墙钟项含 time_offset_secs,偏移只增不减,max 之后立即体现)。
fn now_cf_u32() -> u32 {
    let cf = crate::mole_cheats::now_cf_secs();
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
/// [2026-09-25 第五轮遗留 MISC-4] 取时也与 -[DailySignLayer getServerTime] 同源(0x39a6c8/0x39a6d8 → getCurrentServerTime):
/// 改用单调时钟 crate::mole_cheats::now_cf_secs(),见 now_cf_u32。
fn local_date(env: &mut Environment) -> LocalDate {
    let cf = crate::mole_cheats::now_cf_secs();
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

/// 今天是否处于节日窗口。
/// [2026-09-16] F2-07 统一节日日历:窗口与 MOLE_FESTIVAL 的取值都改用 mole_items 那一份(与商店节日物同一张表),
/// 删掉了这里自带的春节表和环境变量解析。圣诞 = christmas 窗口(12/10~次年 1/06),春节 = newyear 窗口(初一前后 15 天,
/// 表外年份 1/20~2/28)。原版活动的确切日期本地没有依据(服务器下发),窗口为移植者自拟;统一后废品站高价回收从 12/10 起、
/// 春节烟花从初一前 15 天起生效(以前分别是 12/20 与除夕)。
/// MOLE_FESTIVAL:christmas/xmas → 强制圣诞;newyear/spring → 强制春节;off 或强制成其它节日 → 都不开;
/// all/date/未设置 → 按日期(两个窗口不重叠,判定先后不影响结果)。菜单「节日商店」轮换只管商店,不影响这里。
/// [2026-09-25 第五轮遗留 MISC-4] 日期来自 local_date(单调时钟);mole_items 的节日商店(festival_active_mask)与进村登录日界
/// (local_day_index → local_wall_secs)同改为 now_cf_secs,两边同一天,F2-07 统一日历不会因宿主时间回拨而错开一天。
/// 烟花额度记账(local_date)与检查(festival_today 的日期)也同源。
fn festival_today(env: &mut Environment) -> (Festival, LocalDate) {
    let today = local_date(env);
    let by_date = || {
        let day = days_from_civil(today.year, today.month, today.day);
        if crate::mole_items::festival_on_date("christmas", day) {
            Festival::Xmas
        } else if crate::mole_items::festival_on_date("newyear", day) {
            Festival::Spring
        } else {
            Festival::Off
        }
    };
    let fest = match crate::mole_items::festival_forced() {
        Some("christmas") => Festival::Xmas,
        Some("newyear") => Festival::Spring,
        None | Some("all") | Some("date") => by_date(),
        Some(_) => Festival::Off,
    };
    (fest, today)
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
    /// [2026-09-16] E-03 主村每日任务选题所属日期 yyyymmdd(与客户端截止时间同一口径,见 daily_day_key)。
    daily_day: u32,
    /// 主村 1074 回包里的 5 个原始值(不是任务 ID,客户端 hashDailyQuestIdInMainVillage: 映射后才是 ID)。
    daily_vals: Vec<u32>,
    /// [2026-09-16] E-03 黄金岛每日任务选题所属日期 yyyymmdd。
    hv_daily_day: u32,
    /// 黄金岛列表的 3 个原始值(hashDailyQuestIdInHolidayVillage: 映射之前)。
    hv_daily_vals: Vec<u32>,
    /// [2026-10-04 第八轮 R8-C2] 占卜今天已经免费过的日期 yyyymmdd(北京时间日界,与 daily_day_key 同口径);1139 时记账,1138 回包据此给首字段。
    divine_free_day: u32,
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
            daily_day: 0,
            daily_vals: Vec::new(),
            hv_daily_day: 0,
            hv_daily_vals: Vec::new(),
            divine_free_day: 0,
        }
    }
}

impl ActState {
    /// [2026-09-16] F2-06 v=2:正文之后追加 `sum=<fnv1a(正文)>` 一行(与 vip.dat 同一算法);读档时 v=2 缺 sum 或不匹配就是坏档。
    fn serialize(&self) -> String {
        let shells: Vec<String> = self
            .shells
            .iter()
            .map(|(t, ts)| format!("{},{}", t, ts))
            .collect();
        let mut body = format!(
            "v=2\nsign_month={}\nsign_days={}\nsign_foot={}\nsign_patch={}\nsign_reward={}\nexch_month={}\nexch_mask={}\npearl={}\ndug={}\nshells={}\nfirework_day={}\ndaily_day={}\ndaily_vals={}\nhv_daily_day={}\nhv_daily_vals={}\ndivine_free_day={}\n",
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
            self.firework_day,
            self.daily_day,
            join_u32(&self.daily_vals),
            self.hv_daily_day,
            join_u32(&self.hv_daily_vals),
            self.divine_free_day
        );
        let sum = crate::mole_items::fnv1a(body.as_bytes());
        body.push_str(&format!("sum={:08x}\n", sum));
        body
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
                "daily_day" => st.daily_day = num().unwrap_or(0),
                "daily_vals" => st.daily_vals = parse_u32_list(v),
                "hv_daily_day" => st.hv_daily_day = num().unwrap_or(0),
                "hv_daily_vals" => st.hv_daily_vals = parse_u32_list(v),
                "divine_free_day" => st.divine_free_day = num().unwrap_or(0),
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

    /// [2026-09-16] F2-06 带校验的读档。Ok = 可用(空文件也算,按默认值);Err(原因) = 坏档。
    /// 首行 v=1:旧档,按宽松规则读一次(下次保存自动升 v=2);首行 v=2:末行必须是 sum= 且与正文 fnv1a 一致
    /// (sum 在文件末尾,截断时最先丢的就是它,所以 v=2 缺 sum 也算坏档);首行两者都不是的非空文件同样算坏档。
    /// 只做宿主侧字符串处理,不发消息。
    fn parse_checked(bytes: &[u8]) -> Result<ActState, &'static str> {
        if bytes.is_empty() {
            return Ok(ActState::default());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "不是合法 UTF-8")?;
        match text.lines().next().unwrap_or("").trim() {
            "v=1" => Ok(ActState::parse(text)),
            "v=2" => {
                let pos = text.rfind("sum=").ok_or("v=2 缺 sum 行(文件可能被截断)")?;
                let (body, tail) = text.split_at(pos);
                if !body.ends_with('\n') {
                    return Err("sum 不在行首");
                }
                let sum = u32::from_str_radix(tail["sum=".len()..].trim(), 16)
                    .map_err(|_| "sum 值不是十六进制")?;
                if crate::mole_items::fnv1a(body.as_bytes()) != sum {
                    return Err("校验和不匹配");
                }
                Ok(ActState::parse(body))
            }
            _ => Err("首行既不是 v=1 也不是 v=2"),
        }
    }
}

/// u32 列表 → "a,b,c"(空列表为空串)。
fn join_u32(v: &[u32]) -> String {
    v.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// "a,b,c" → u32 列表;任何一项解析失败返回空列表(调用方据此当作没有记录)。
fn parse_u32_list(s: &str) -> Vec<u32> {
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in s.split(',') {
        match item.trim().parse::<u32>() {
            Ok(x) => out.push(x),
            Err(_) => return Vec::new(),
        }
    }
    out
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

/// [2026-09-16] F2-05 开发工具「时间旅行」偏移是否生效(偏移只增不减,一旦非 0 本会话一直非 0)。
fn time_travel_active() -> bool {
    crate::libc::time::time_offset_secs() != 0
}

/// [2026-09-16] X4-02 时间旅行期间付费入口被拦时给玩家看的提示(get_static_str 静态串,不释放)。
const TIME_TRAVEL_PAID_BLOCKED_MSG: &str = "时间旅行中，补签、挖贝壳、刷新贝壳和兑换奖励暂不可用（这段时间的活动进度不会保存）。重新启动游戏回到现实时间后即可恢复。";

/// [2026-09-16] X4-02 拦下一次活动中心付费操作:记日志;show_box 为真时再弹游戏自带 MessageBox(type 6 = 只有「确定」,关框无回调)。
/// 调用序列照原版同一批层里的提示,例如 -[SeabedSeekingTreasureMainLayer onDigShellClick:] 0x2c1f3c..0x2c1fda 的
/// `[[MessageBox sharedInstance] showWithTarget:self selector:0 title:nil message:msg type:6 vipgold:0]`;target 传 nil,
/// 与 mole_cheats::show_game_message_box 一致(type 6 的 onButtonOK: 只关框)。那个函数是私有的,mole_menu 也没有公开的提示函数,
/// 这里照抄它的调用序列:宿主 msg_send 只实现到「接收者+选择子+5 个参数」,type(低 32 位)与 vipgold(高 32 位,恒 0)合成一个 u64,
/// 落到 sp+8/sp+0xc,与分开传逐字节相同。
/// show_box=false 用于 MessageBox 按钮回调:原版 onSureRefreshClick 在回调里发现贝壳不足时也不当场弹框,而是置 isNoMuchVipGold
/// 再 scheduleUpdate,延到 update: 里弹(0x2c24d2/0x2c24d6)。可见在回调栈里再弹框不可靠,所以只记日志;入口已拦,正常走不到回调。
/// 只在菜单/按钮回调里调用(不在 drawScene/mainLoop 帧栈上);会改写 r0-r3,调用方吞掉调用并自写 r0。
fn block_paid_action_in_time_travel(env: &mut Environment, what: &str, show_box: bool) {
    log!(
        "[ACTIVITY] 时间旅行中(偏移 {} 秒):拦下「{}」——活动侧档此时只写内存,客户端本地扣款/发物却照常进主档,放行会在重启后变成扣了款、进度回滚或同一档位可再兑",
        crate::libc::time::time_offset_secs(),
        what
    );
    if !show_box {
        return;
    }
    let mb_cls = env.objc.get_known_class("MessageBox", &mut env.mem);
    if mb_cls == nil {
        return;
    }
    let sh = sel_named(env, "sharedInstance");
    let mb: id = msg_send(env, (mb_cls, sh));
    if mb == nil {
        return;
    }
    let msg = ns_string::get_static_str(env, TIME_TRAVEL_PAID_BLOCKED_MSG);
    let show = sel_named(env, "showWithTarget:selector:title:message:type:vipgold:");
    let type_and_vipgold: u64 = 6; // 低 32 位 = type 6,高 32 位 = vipgold 0
    let _: () = msg_send(env, (mb, show, nil, SEL::null(), nil, msg, type_and_vipgold));
}

fn load_state(env: &mut Environment) -> ActState {
    // [2026-09-16] F2-05 时间旅行隔离:偏移非 0 时第一次从盘读进 TT_STATE,之后只读缓存。
    //   以前拨到下个月再开签到层,sign_roll_month 会清掉本月签到并落盘,重启回到现实又清一次;现在旅行期间的改动只留在内存里。
    if time_travel_active() {
        if let Some(st) = TT_STATE.with(|c| c.borrow().clone()) {
            return st;
        }
        let st = load_state_from_disk(env);
        TT_STATE.with(|c| *c.borrow_mut() = Some(st.clone()));
        log!(
            "[ACTIVITY] 时间旅行中:{} 读入内存缓存,本会话之后的改动不落盘(重启即回到旅行前的档)",
            STATE_FILE
        );
        return st;
    }
    load_state_from_disk(env)
}

fn load_state_from_disk(env: &mut Environment) -> ActState {
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
    match ActState::parse_checked(&bytes) {
        Ok(st) => st,
        Err(why) => {
            backup_corrupt_state(env, path, &bytes, why);
            ActState::default()
        }
    }
}

/// [2026-09-16] F2-06 坏档原样备份为 `<路径>.corrupt`(已存在则 `.corrupt-<unix秒>`,不覆盖更早的备份);
/// 成功才允许之后按默认值覆盖原文件,失败置 ACT_SAVE_BLOCKED,本会话 save_state 全部跳过。
/// 做法同 vip.dat(mole_items side_ensure_loaded):只动宿主 fs(env.fs.exists / write_atomic),不另发 msg_send。
/// 路径 NSString 来自 pathForDataFile:(0x75374,NSSearchPathForDirectoriesInDomains + stringByAppendingPathComponent:),
/// 是宿主字符串对象,to_rust_string 直接读(mole_cheats quarantine_corrupt_file 同样用法)。
fn backup_corrupt_state(env: &mut Environment, path: id, bytes: &[u8], why: &str) {
    if ACT_SAVE_BLOCKED.load(O) {
        return; // 已判定备份失败:本会话不再重复尝试,也不刷日志
    }
    let fingerprint = (1u64 << 32) | u64::from(crate::mole_items::fnv1a(bytes));
    if ACT_CORRUPT_BACKED.load(O) == fingerprint {
        return; // 同一份坏档本会话已经备份过
    }
    let src = ns_string::to_rust_string(env, path).into_owned();
    let mut bak = format!("{}.corrupt", src);
    if env.fs.exists(GuestPath::new(&bak)) {
        bak = format!(
            "{}.corrupt-{}",
            src,
            crate::libc::time::host_now_unix_secs()
        );
    }
    if env.fs.write_atomic(GuestPath::new(&bak), bytes).is_ok() {
        ACT_CORRUPT_BACKED.store(fingerprint, O);
        log!(
            "[ACTIVITY] ⚠️ {} 是坏档({})→ 已原样备份为 {},本次按默认值处理",
            STATE_FILE,
            why,
            bak
        );
    } else {
        ACT_SAVE_BLOCKED.store(true, O);
        log!(
            "[ACTIVITY] ⚠️ {} 是坏档({})且备份到 {} 失败 → 本会话不覆盖它(签到/脚印/海底寻宝等本会话不落盘)",
            STATE_FILE,
            why,
            bak
        );
    }
}

fn save_state(env: &mut Environment, st: &ActState) {
    // [2026-09-16] F2-05 时间旅行中只写内存缓存。
    if time_travel_active() {
        TT_STATE.with(|c| *c.borrow_mut() = Some(st.clone()));
        log!("[ACTIVITY] 时间旅行中:{} 只写内存缓存,不落盘", STATE_FILE);
        return;
    }
    // [2026-09-16] F2-06 坏档备份失败时,不拿内存里的默认值覆盖原文件(提示只打一次,签到/挖贝每次操作都会走到这里)。
    if ACT_SAVE_BLOCKED.load(O) {
        if !ACT_BLOCK_LOGGED.swap(true, O) {
            log!(
                "[ACTIVITY] 跳过写 {}(原文件是未能备份的坏档,本会话不再提示)",
                STATE_FILE
            );
        }
        return;
    }
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

/// [2026-10-03] 回环白名单:sendPacket:commandId: 臂据此当场决定接不接(接了就吞掉真方法,应答交给运行循环受理点)。
/// 纯判断,不发消息、不碰寄存器。必须与 answer_request 的分支一一对应。
fn loopback_accepts(cmd: u32) -> bool {
    match cmd {
        // MOLE_DISCOUNT=off 时放行真 sendPacket:commandId:——原离线行为(只 packetsCount+1、setSendFlag 后返回,无折扣)。
        CMD_DISCOUNT_LIST => !discount_disabled(),
        CMD_OPEN_BOX_SWITCH
        | CMD_NOTICE
        | CMD_ACTIVITY_CENTER
        | CMD_PURCHASE_IN_ACTIVITY
        | CMD_FIREWORK
        | CMD_SIGN_DAYS
        | CMD_IS_EXCHANGED
        | CMD_SIGN_EXCHANGE_LIST
        | CMD_SEABED_INFO
        | CMD_SEABED_REFRESH
        | CMD_DAILY_TASK_LIST
        | CMD_DIVINE_LIST
        | CMD_FREE_DIVINE_TAG
        | CMD_LOGIN_COUNT
        | CMD_ONLINE_TIME => true,
        _ => false,
    }
}

/// 回环白名单分派:按截下的请求算应答并入队。
/// [2026-10-03] 以前叫 handle_send_packet,在 sendPacket:commandId: 的调用栈上同步执行(读旁路档、组回包都要发宿主消息);
///   现在只由运行循环受理点(run_loop_poll → answer_pending_requests)调用,不在任何游戏方法体的栈上,不碰寄存器。
///   哪些命令号会走到这里由 loopback_accepts 决定;`req` 是截包时拷下的请求体(nil 请求体为空)。
fn answer_request(env: &mut Environment, nm: id, cmd: u32, req: &[u8]) {
    match cmd {
        CMD_OPEN_BOX_SWITCH => {
            // F3-8:活动预告页的开宝箱/港游开关。内容已空心化(对应层无入口且图集缺失),不注入,
            // 只显式吞掉并记日志;预告页本身不等回包,不会卡 loading。
            log!("[ACTIVITY] 吞掉 cmd=1217(活动预告开宝箱开关,离线不注入) len=0");
        }
        CMD_NOTICE => {
            enqueue_reply(env, nm, cmd, encode_notices());
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
        }
        CMD_PURCHASE_IN_ACTIVITY => {
            // 只有购买类活动(type 1)才会发 1092;本地活动表里没有,保险起见回一对 0(8 字节)防 loading 超时。
            enqueue_reply(env, nm, cmd, vec![0u8; 8]);
        }
        CMD_FIREWORK => {
            // 下面几种"不回包"情形:真 sendPacket:commandId: 已在截包时吞掉(离线真方法也只是 packetsCount+1、setSendFlag: 后
            //   在 0xe2376 → 0xe2770 返回,对离线没有意义),这里只是不入队。
            let (fest, today) = festival_today(env);
            if fest != Festival::Spring {
                // 窗口外:不回包(与离线原行为一致)。
                return;
            }
            let st = load_state(env);
            if st.firework_day == today.ymd() {
                log!("[ACTIVITY] 春节烟花今天已放过,cmd=1112 不回包");
                return;
            }
            if nm != nil && FIREWORK_IN_FLIGHT.swap(true, O) {
                log!("[ACTIVITY] 春节烟花:已有一个 1112 回包在途,本次不重复回包");
                return;
            }
            // [2026-09-16] A1-01 不再在这里写 firework_day:回包要等村庄场景挂好 FireworkLayer 才喂(feed_or_defer_firework),
            //   真开播时由 (FireworkLayer, showFireWorkFullScreen) 钩子记账;没放出来就不扣当天额度。
            // parseFireworkFlag@0x1c2258 只读 1 字节,同时写 showFirework 与 hasFireworkGift。
            enqueue_reply(env, nm, cmd, vec![1u8]);
        }
        CMD_SIGN_DAYS => {
            let today = local_date(env);
            let mut st = load_state(env);
            if sign_roll_month(&mut st, today.ym()) {
                save_state(env, &st);
            }
            enqueue_reply(env, nm, cmd, encode_sign_days(&st));
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
        }
        CMD_SIGN_EXCHANGE_LIST => {
            enqueue_reply(env, nm, cmd, encode_sign_exchange_list());
        }
        CMD_SEABED_INFO => {
            let mut st = load_state(env);
            if seabed_ensure_shells(&mut st) {
                save_state(env, &st);
            }
            enqueue_reply(env, nm, cmd, encode_seabed_info(&st));
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
        }
        CMD_DISCOUNT_LIST => {
            // [补完 2026-09-15] F2-2 主村限时折扣(黄金岛的 1073 不走这里;岛上会话已在 intercept 入口排除)。
            //   MOLE_DISCOUNT=off 时 loopback_accepts 不接,真 sendPacket:commandId: 照常空过,走不到这里;
            //   state==4 兜底臂截下的请求也已先查过开关。
            let body = encode_discount_list(env);
            enqueue_reply(env, nm, cmd, body);
        }
        CMD_DAILY_TASK_LIST => {
            // [2026-09-16] E-03 每日任务列表。请求体首字节:0 主村 / 1 黄金岛(getDailyTaskListFromServerWithSceneId:@0x1cb5cc)。
            //   岛上会话在 intercept 入口就已排除,岛上的请求在请求入口改走宿主侧构造(island_daily_quest_apply)。
            //   这里若还收到 1(不在岛上会话却传了 10,正常走不到),不回包,免得在主村场景跑岛上的列表更新。
            match req.first().copied() {
                Some(0) => {
                    if let Some(reply) = encode_daily_task_list_main(env) {
                        enqueue_reply(env, nm, cmd, reply);
                    }
                }
                other => {
                    log!(
                        "[ACTIVITY] 每日任务 cmd=1074 请求体首字节={:?}(非主村),不回包",
                        other
                    );
                }
            }
        }
        // [2026-10-06 第九轮 R9-D1] 连续登录 1051 / 累计在线 1050:按离线侧档回包(数值与 GameData loginTimesCounter /
        //   onlineTimer 两个 getter 的离线拦截同源),由原版 onCommandReceived: 自己发 checkConditions:0x20 / 0x10 判成就,
        //   currentGameMode 门、已解锁去重、发奖与存档都走原版。连续天数只在本地日期 == 上次 + 1 时加 1(on_enter_village),
        //   同一天重复进村不变,刷不出来。
        CMD_LOGIN_COUNT => {
            let (streak, _) = crate::mole_items::offline_login_stats(env);
            log!("[ACTIVITY] 连续登录 1051 回包 {} 天", streak);
            enqueue_reply(env, nm, cmd, vec![streak.min(255) as u8]);
        }
        CMD_ONLINE_TIME => {
            let (_, secs) = crate::mole_items::offline_login_stats(env);
            log!("[ACTIVITY] 在线时长 1050 回包 {} 秒", secs);
            enqueue_reply(env, nm, cmd, secs.to_le_bytes().to_vec());
        }
        // [2026-10-04 第八轮 R8-C2] 占卜奖池 1138:照 parseDivineDataList 逐字段编码(奖池见 DIVINE_POOL,移植者自拟)。
        CMD_DIVINE_LIST => {
            let body = encode_divine_list(env);
            enqueue_reply(env, nm, cmd, body);
        }
        // [2026-10-04 第八轮 R8-C2] 1139 今天的免费占卜已用:记账,不回包(客户端不读)。时间旅行期间 save_state 只写内存(F2-05)。
        CMD_FREE_DIVINE_TAG => {
            let today = daily_day_key(now_cf_u32());
            let mut st = load_state(env);
            st.divine_free_day = today;
            save_state(env, &st);
            log!(
                "[ACTIVITY] 占卜 cmd=1139:今天({})的免费占卜已用,记入 {}",
                today,
                STATE_FILE
            );
        }
        other => {
            log!(
                "[ACTIVITY] 回环受理 cmd={}:不在白名单(loopback_accepts 与本函数不一致),不回包",
                other
            );
        }
    }
}

// ─────────────────────────────── [2026-10-04 第八轮 R8-C2] 占卜奖池 1138 ───────────────────────────────

/// 1138 回包:首字段「今天已用过免费」(divine_free_day 是今天就是 1),然后 DIVINE_POOL 各组。
fn encode_divine_list(env: &mut Environment) -> Vec<u8> {
    let today = daily_day_key(now_cf_u32());
    let used_free = load_state(env).divine_free_day == today;
    let mut b = Vec::new();
    put_u32(&mut b, u32::from(used_free));
    put_u32(&mut b, DIVINE_POOL.len() as u32);
    for (round, items) in DIVINE_POOL {
        put_u32(&mut b, round);
        put_u32(&mut b, items.len() as u32);
        for &(id, num, pct) in items {
            put_u32(&mut b, id);
            put_u32(&mut b, num);
            put_u32(&mut b, pct);
        }
    }
    log!(
        "[ACTIVITY] 占卜 cmd=1138:回奖池 {} 组(轮次 1-5 + 最高大奖;移植者自拟,非原版数据),今天({}){}",
        DIVINE_POOL.len(),
        today,
        if used_free { "已免费过" } else { "还能免费一次" }
    );
    b
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

/// 拦 -[NetworkManager getFoodsExchangeToSure:](1120)。r2 是【0 起】的兑换项下标:三处发包
/// 0x26662a / 0x4f830 / 0x53654 都是 `subs r2, r0, #1`,即 foodPrintExchangeIndex(= 菜单 tag + 1,
/// 0x39c70a)减 1;0x266600/0x4f806 要求 foodPrintExchangeIndex >= 1 才发。与 1119 回包的兑换位
/// (parseIsExchangedInfo 0x1c4288:第 i 位 ↔ 第 i 项,0 起)一致。
/// [2026-10-06 第十轮 R10-C2] 这里曾按 1 起处理:兑第 1 项发的 0 被当成越界忽略(不扣脚印、不记已兑,
/// 同月可无限白拿),其余各项扣的、记的都是上一项。现按 0 起。
/// 原版扣脚印在服务器侧,这里在旁路档扣并记兑换位。本月该项已兑过时按已处理应答、不再扣:兑换菜单
/// 0x39bf58~0x39bf74 用 hasExchangedItemDataFix 禁用已兑项,玩家不能在同月重新发起同一项,同一位的
/// 重复 1120 只会来自 onCancelExchange 0x53660 / onButtonOkSelected: 0x4f83c 带着旧下标的重发。
fn sign_exchange_confirm(env: &mut Environment, index: u32) {
    let today = local_date(env);
    let mut st = load_state(env);
    sign_roll_month(&mut st, today.ym());
    let idx = index as usize;
    if idx >= SIGN_EXCHANGE_TABLE.len() {
        log!("[ACTIVITY] 脚印兑换忽略:index={} 超出本地兑换表", index);
        save_state(env, &st);
        return;
    }
    let (cost, item, num) = SIGN_EXCHANGE_TABLE[idx];
    let bit = 1u32 << idx;
    if st.exch_mask & bit != 0 {
        log!(
            "[ACTIVITY] 脚印兑换:第 {} 项(index={})本月已兑过,按已处理应答,不再扣脚印",
            idx + 1,
            index
        );
        save_state(env, &st);
        return;
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
/// [2026-10-07 第十一轮 R11-F-2] kCFAbsoluteTimeIntervalSince1970:2001-01-01 的 Unix 秒。
const CF_EPOCH_UNIX_SECS: u32 = 978_307_200;

fn encode_seabed_info(st: &ActState) -> Vec<u8> {
    let mut b = Vec::new();
    put_u32(&mut b, 1); // 活动进行中
    put_u32(&mut b, st.pearl);
    put_u32(&mut b, st.shells.len() as u32);
    for (i, (t, ts)) in st.shells.iter().enumerate() {
        put_u32(&mut b, i as u32 + 1);
        put_u32(&mut b, *t);
        // [2026-10-07 第十一轮 R11-F-2] 「上次挖掘时间」按 Unix 秒下发:-[SeabedSeekingTreasureMainLayer displayUI]
        // 在 0x2c1214 无条件减 kCFAbsoluteTimeIntervalSince1970(978307200)再与当前时间比 300 秒冷却。以前存发的是
        // 2001 纪元秒,减成负数后截成 0,关掉页面再打开冷却全没了。旧档里存的 2001 纪元值(1000 < ts < 978307200)
        // 在这里换算;0 = 刷新后无冷却,照旧。
        let ts = if *ts > 1000 && *ts < CF_EPOCH_UNIX_SECS {
            ts + CF_EPOCH_UNIX_SECS
        } else {
            *ts
        };
        put_u32(&mut b, ts);
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
    // 挖过的贝壳换成新类型并记下挖掘时间(Unix 秒,见 encode_seabed_info:客户端会先减掉 1970→2001 的差值),
    // 重开层时 displayUI 据此算 5 分钟冷却。
    st.shells[idx] = (new_type, now_cf_u32().saturating_add(CF_EPOCH_UNIX_SECS));
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
/// [2026-09-25 第五轮遗留 MISC-4] 取时同 local_date,用单调时钟 crate::mole_cheats::now_cf_secs()。
fn local_today_and_midnight(env: &mut Environment) -> (LocalDate, i64) {
    use crate::frameworks::foundation::ns_time_zone::seconds_from_gmt_at_unix;
    let cf = crate::mole_cheats::now_cf_secs();
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

// ─────────────────────────────── [2026-09-24 第四轮 K6 I8-4] 黄金岛限时折扣 1073 ───────────────────────────────

/// [2026-09-24 第四轮 K6 I8-4] NewSceneData.storeBuildingsArray_(@"NSMutableArray",编译期 +172):建设庄园 shop_type 1 的分页数组
/// (-[NewSceneData parseObjectData:] 0x21a952..0x21a992 按 shop_sub_type 1..6 归页;-[NewStyleStoreItemsView loadObjectsDataByType:]
/// 在 curSceneId==10 时 5..10 页直接取它,0x3b96ca)。
const SLOT_NSD_STORE_BUILDINGS: u32 = 0xb05df4;
/// [2026-09-24 第四轮 K6 I8-4] NewSceneData.storeDecorationsArray_(+176):shop_type 2 的分页数组(0x21a9e0;商店 11..16 页,0x3b9708)。
const SLOT_NSD_STORE_DECORATIONS: u32 = 0xb05df8;
/// [2026-09-24 第四轮 K6 I8-4] NewSceneData.discountObjDataArr_(+52):岛上折扣表(addOneDiscountGood:@0x21fc8c 往里加)。只用来数件数写日志。
const SLOT_NSD_DISCOUNT_ARR: u32 = 0xb05db4;
/// [2026-09-24 第四轮 K6 I8-4] ObjectData.rest_place_(i,+112;-[ObjectData rest_place]@0x8e130 是平凡 ivar 读)。
const SLOT_OBJ_REST_PLACE: u32 = 0xb03cac;
/// [2026-09-25 第五轮遗留 MISC-2] ObjectData.level_(C,+12;-[ObjectData level]@0x8dd60 是平凡 ivar 读:0x8dd68 取槽、0x8dd6c ldrb)。
/// 岛物品由 -[NewSceneData parseObjectData:] 在 0x21a2b0 setLevel:,NewSceneObjectData 只新增 +136/+140 两个 ivar,level_ 继承自 ObjectData。
const SLOT_OBJ_LEVEL: u32 = 0xb03c30;

/// [2026-09-24 第四轮 K6 I8-4] 从 NewSceneData 的建设庄园分页数组收集可打折的贝壳商品:(物品 ID, 贝壳原价),按 ID 升序去重。
/// 岛上折扣只被建设庄园与建造链消费,全部经 -[WrapperManager checkIsDiscountObj:]@0x2610d0 按 curSceneId 选 NewSceneData:
/// 商店详情 -[NewStyleStoreItemsView updateObjectInfo](0x3ba2dc 判定、0x3baef4 取价)、-[NewStyleStoreMainLayer onBuyItem:](0x3b26e8/0x3b2790)、
/// -[NewSceneVillageMenuLayer showCostGoldView:](0x25b8a4)、-[NewSceneEditMenuLayer onButtonOkSelected:](0x269296)、
/// -[NewScenePorter finishBuild:](0x26d466/0x26d830,0x26d4b2 拿 goodsPrice 顶替 cost_vip_gold → 0x26d502 addVipGoldInNewScene: 扣贝壳)、
/// -[NewSceneData getLockType4Object:](0x21eb30,同样顶替 cost_vip_gold 判买不买得起)。食材店不查折扣,不在候选里。
/// 选品规则(移植者自拟,非原版数据;沿用主村 discount_candidates 的口径,元素类换成岛上的,另加 rest_place 与 level 两条过滤):
/// - 元素类是 NewSceneObjectData(岛上 parseObjectData: 0x21a23c 建的,ObjectData 子类)或 ObjectData;字段直接读 ivar,不逐个发消息;
/// - shop_type 1/2 · 装饰类 type 14 · 纯贝壳价(cost_gold==0 且 cost_vip_gold 5..=10000,排除 0 价)· 非 VIP 专属(vip_level==0)·
///   不限购(limit_count==0,限购已拥有的物品打折也买不了)· ID>1000(NewSceneData addOneDiscountGood: 在 getObjectDataWithId: 取不到时
///   仍收 ID 1..7(0x21fd0c..0x21fd12),那是内购档位,离线不碰);
/// - 岛上另排除 rest_place==2:onBuyItem: 0x3b27b0 对它走「用贝壳购买」确认框分支,避开最稳(主村同理只挑装饰类)。
/// - [2026-09-25 第五轮遗留 MISC-2] 岛上另排除 level>1(静态口径,不按玩家当前等级动态筛,免得同一天升级就换品):
///   -[NewSceneData getLockType4Object:] 先在 0x21eb36 checkIsDiscountObj: 换成折扣价,再 0x21eb82 取 [obj level]、0x21ebc8 取主村
///   [[GameData sharedInstance] userInfoData] curLevel,0x21ebd0 cmp + 0x21ebd2 bgt 等级不够返回 1(等级锁)。propertyHV 按其余规则
///   筛出的 200 件候选里只有 32036(80 贝壳)是 level 18,其余都是 1 级;主村 property.dat 的候选全是 1 级,这条让岛候选与主村
///   「全是 1 级」的口径一致。正常进岛要主村 curLevel>17(-[VillageLayer checkSpecailZone:] 0x373b4、-[ActivityBulletinLayer
///   onJoinInActivity] 0x3aab1e,否则弹 NEED_LEVEL_UNLOCK_ISLAND),对他们 32036 永远不锁;只有修改器一键进岛绕过等级门的低等级
///   玩家才会看到它「打了折却锁着」。候选从 200 变 199,改动当天岛折扣整表会换一次;岛折扣表每次进岛重建、回主村清空,不落盘。
fn island_discount_candidates(env: &mut Environment, nsd: id) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    if nsd == nil {
        return out;
    }
    let count_sel = sel_named(env, "count");
    let at_sel = sel_named(env, "objectAtIndex:");
    for (slot, want_shop_type) in [
        (SLOT_NSD_STORE_BUILDINGS, 1u8),
        (SLOT_NSD_STORE_DECORATIONS, 2u8),
    ] {
        let pages: id = match read_ivar_u32(env, nsd, slot) {
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
                let isa = crate::objc::ObjC::read_isa(obj, &env.mem);
                if !matches!(
                    env.objc.try_get_class_name(isa),
                    Some("NewSceneObjectData") | Some("ObjectData")
                ) {
                    continue;
                }
                let shop_type = read_ivar_u8(env, obj, SLOT_OBJ_SHOP_TYPE).unwrap_or(0);
                let obj_type = read_ivar_u8(env, obj, SLOT_OBJ_TYPE).unwrap_or(0);
                let limit_count = read_ivar_u8(env, obj, SLOT_OBJ_LIMIT_COUNT).unwrap_or(1);
                let vip_level = read_ivar_u32(env, obj, SLOT_OBJ_VIP_LEVEL).unwrap_or(1);
                let cost_gold = read_ivar_u32(env, obj, SLOT_OBJ_COST_GOLD).unwrap_or(1);
                let price = read_ivar_u32(env, obj, SLOT_OBJ_COST_VIP_GOLD).unwrap_or(0);
                let rest_place = read_ivar_u32(env, obj, SLOT_OBJ_REST_PLACE).unwrap_or(2);
                // 读不到按最高等级处理,保守排除(与 limit_count 读不到给 1 同理)。
                let level = read_ivar_u8(env, obj, SLOT_OBJ_LEVEL).unwrap_or(u8::MAX);
                let Some(item) = read_ivar_u32(env, obj, SLOT_OBJ_ID) else {
                    continue;
                };
                if shop_type != want_shop_type
                    || obj_type != DISCOUNT_OBJECT_TYPE
                    || limit_count != 0
                    || vip_level != 0
                    || cost_gold != 0
                    || !(DISCOUNT_MIN_PRICE..=DISCOUNT_MAX_PRICE).contains(&price)
                    || rest_place == 2
                    || level > 1
                    || item <= 1000
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

/// [2026-09-24 第四轮 K6 I8-4] 黄金岛:照 -[NetworkManager parseDiscountListWithSceneId:pos:len:]@0x2270f8 的岛分支在宿主侧构造
/// (0x227306 [NewSceneData sharedInstance] → 0x227316 removeAllObjectFromDiscountArr → 0x2273a0 起逐条 [[DiscountInfo alloc] init]
/// → setGoodsId:/setGoodsPrice:/setExpireTime:(0x2273e4/0x2273ee/0x2273f8,签名都是 v12@0:4L8,按 u32 传)→
/// 0x227412 [[NewSceneData sharedInstance] addOneDiscountGood:](v12@0:4@8)→ 0x22741a release),商店划线价、买得起判定、扣款全走原版。
/// - 选品见 island_discount_candidates;挑选、折扣率、到期时间照主村 1049 的自拟口径(pick_discounts:本地日期 yyyymmdd 做种子
///   确定性挑 DISCOUNT_COUNT 件打 7~8 折,同一天多次进岛结果一致;选品规则为移植者自拟,非原版数据)。
/// - expireTime 与主村一样填本地次日 0:00 的 unix 秒,只作语义与日志用:DiscountInfo expireTime 取值方法无 selref(只有 SHK 同名),
///   -[CommonEffectController innerupdateDiscount:]@0x322f50 走的是 lefttime_ 浮点倒计时,只由 startDiscountTimer:withTarget:selecter:
///   排程,而它唯一的调用者 -[DiscountInfoLayer startTimer]@0x1ebbe8 没人发,原版客户端在会话中途不按到期时间清表。
///   岛上折扣表由原版在回主村时清掉(-[LoadingMainVillage updateLoading:] 0x254416 [[NewSceneData sharedInstance] removeAllObjectFromDiscountArr]),
///   所以按「每次进岛喂一次」即可,跨天后下次进岛整表换成新一天的折扣。
/// - 候选为空时不清表、不构造:原版 count==0 也不清(0x2272e6 在 removeAllObjectFromDiscountArr 之前返回)。
/// - 只写 NewSceneData,不碰 GameData 的主村折扣表。
///
/// 只在运行循环回调里调用(会发宿主消息,调用方吞掉调用并自写 r0)。
fn island_discount_apply(env: &mut Environment) {
    let nsd = singleton(env, "NewSceneData", "sharedInstance");
    if nsd == nil {
        log!("[ACTIVITY] 黄金岛折扣:NewSceneData 未就绪,放弃构造");
        return;
    }
    let di_cls = env.objc.get_known_class("DiscountInfo", &mut env.mem);
    if di_cls == nil {
        log!("[ACTIVITY] 黄金岛折扣:找不到 DiscountInfo 类,放弃构造");
        return;
    }
    let (today, midnight_unix) = local_today_and_midnight(env);
    let candidates = island_discount_candidates(env, nsd);
    let picks = pick_discounts(&candidates, today.ymd());
    if picks.is_empty() {
        log!(
            "[ACTIVITY] 黄金岛折扣:日期={} 候选={} 件,无可打折商品(岛上物品表可能还没加载),不清表、不构造",
            today.ymd(),
            candidates.len()
        );
        return;
    }
    let expire = midnight_unix.clamp(0, u32::MAX as i64) as u32;

    let remove_all = sel_named(env, "removeAllObjectFromDiscountArr");
    let _: () = msg_send(env, (nsd, remove_all));
    let alloc_sel = sel_named(env, "alloc");
    let init_sel = sel_named(env, "init");
    let set_id = sel_named(env, "setGoodsId:");
    let set_price = sel_named(env, "setGoodsPrice:");
    let set_expire = sel_named(env, "setExpireTime:");
    let add_sel = sel_named(env, "addOneDiscountGood:");
    let mut desc: Vec<String> = Vec::with_capacity(picks.len());
    for &(item, orig, price) in picks.iter() {
        let di: id = msg_send(env, (di_cls, alloc_sel));
        let di: id = msg_send(env, (di, init_sel));
        if di == nil {
            continue;
        }
        let _: () = msg_send(env, (di, set_id, item));
        let _: () = msg_send(env, (di, set_price, price));
        let _: () = msg_send(env, (di, set_expire, expire));
        // 原版每条都重新取 [NewSceneData sharedInstance](0x22740a),单例不变,这里复用 nsd。
        let _: () = msg_send(env, (nsd, add_sel, di));
        // alloc/init 得到的 +1:addOneDiscountGood: 放进数组(或因 getObjectDataWithId: 取不到而不收)之后照原版 0x22741a release。
        release(env, di);
        desc.push(format!("{}:{}→{}", item, orig, price));
    }
    let arr: id = match read_ivar_u32(env, nsd, SLOT_NSD_DISCOUNT_ARR) {
        Some(bits) => Ptr::from_bits(bits),
        None => nil,
    };
    let accepted: GuestUSize = if arr == nil {
        0
    } else {
        let count_sel = sel_named(env, "count");
        msg_send(env, (arr, count_sel))
    };
    log!(
        "[ACTIVITY] 黄金岛折扣:宿主侧构造 {} 件(日期={} 候选={} 选中 [{}] 到期unix={}),照 parseDiscountListWithSceneId:pos:len: 岛分支交给 NewSceneData addOneDiscountGood:(选品规则为移植者自拟,非原版数据)",
        accepted,
        today.ymd(),
        candidates.len(),
        desc.join(" "),
        expire
    );
}

// ─────────────────────────────── [2026-09-16] E-02 / A1-01 / E-03 进村补发 ───────────────────────────────

/// 离线进村补发 -[GameManager startGame:] 在 isConnected 门内(0x19938..0x19e18)跳过的另外四条同步,按原版顺序:
/// - 0x19ae6 `[[GameData sharedInstance] setIsUserSelectedNoticeBoardMenu:NO]`(re.py 追寄存器:接收者 r11 = r5 = 0x197ae 的
///   GameData 类引用、r8 = sharedInstance;全二进制也只有 GameData 实现这个选择子)→ 0x19b00 `[nm getNoticeMessages]`(1058;
///   回包后 onCommandReceived: 0x237a4 在玩家没点过公告栏时只给公告按钮加小星星,不强弹);
/// - 0x19b68 `[nm getFireworkFlagFromServer]`(1112):只在春节窗口、今天还没放过、也没有烟花包在途时补发;
/// - 0x19c6c `[nm getDailyTaskListFromServerWithSceneId:1]`(1074):进村就备好当天列表。-[ActorManager touchEnd:] 在列表为空时
///   先弹「没有连接网络」再重发(0x9eba4/0x9ebb4 → 0x9ec40),不提前备好的话第一次点日常 NPC 仍会弹框。
/// - [2026-09-25 第五轮遗留 V] 0x19ca0 `[nm getVipInfo]`(1084):只补 GameManager 的 1084 分发臂(0x239f6),由运行循环受理点
///   vip_info_poll 执行,不走回环(见 request_vip_info)。
/// 前三个发包方法都不查 state,直接 sendPacket:commandId:(0x1cb33a / 0x1cbc68 / 0x1cb618),由 handle_send_packet 回环应答。
/// 从岛回村时第四轮 K7 N-D5-2 的 (SceneMannager, loadMainVillageScene) 臂已在 startGame: 之前清掉离岛标志,所以冷启动进村与
/// 从岛回村都会走到这里。
/// [2026-10-03] 只由运行循环受理点调用(startGame: 臂只置标志,见 startgame_resend_poll),不在 startGame: 的调用栈上,不碰寄存器。
///   三个发包方法照常走原版入口,在 sendPacket:commandId: 臂被截下,同一受理点紧接着算应答,回包下一轮喂。
fn startgame_resend_offline(env: &mut Environment, nm: id) {
    // [2026-10-06 第九轮 R9-D1] 原版 isConnected 门内 0x19aa8 [nm getLoginCountFromServer]、0x19ac2 [nm getOnlineTimeFormServer]
    //   (排在公告 1058 之前;两者之间原版还有一次 sleep,离线无意义不照抄)。两方法体只有 sendPacket:nil commandId:,
    //   被回环截下,回包里的连续天数/在线秒数由原版分发去判「衷心感谢」「超感动」成就。
    let get_login = sel_named(env, "getLoginCountFromServer");
    let _: () = msg_send(env, (nm, get_login));
    let get_online = sel_named(env, "getOnlineTimeFormServer");
    let _: () = msg_send(env, (nm, get_online));
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd != nil {
        let set_flag = sel_named(env, "setIsUserSelectedNoticeBoardMenu:");
        let _: () = msg_send(env, (gd, set_flag, false));
    }
    let get_notice = sel_named(env, "getNoticeMessages");
    let _: () = msg_send(env, (nm, get_notice));

    let (fest, today) = festival_today(env);
    if fest == Festival::Spring {
        if FIREWORK_IN_FLIGHT.load(O) {
            log!("[ACTIVITY] 春节烟花:上一个 1112 回包还在等场景就绪,本次进村不重复补发");
        } else if load_state(env).firework_day == today.ymd() {
            log!("[ACTIVITY] 春节烟花今天已放过,进村不补发 1112");
        } else {
            let get_fw = sel_named(env, "getFireworkFlagFromServer");
            let _: () = msg_send(env, (nm, get_fw));
        }
    }

    let get_daily = sel_named(env, "getDailyTaskListFromServerWithSceneId:");
    let _: () = msg_send(env, (nm, get_daily, 1i32));

    // [2026-09-25 第五轮遗留 V] 0x19ca0 `[nm getVipInfo]`(1084):isConnected 门内紧跟 1074(0x19c6c)、圣诞活动标志(0x19c86,不补)之后。
    //   getVipInfo@0xeac2c 本体只有 sendPacket:nil commandId:0x43c、没有别的副作用,这里直接置排队标志,与经原版入口再被
    //   getVipInfo 臂接住等价。startGame: 的两条来路:从岛回村是调度器帧栈(-[NewBaseLoading endLoading] → … →
    //   -[SceneMannager endLoadingScene] 0x241668 loadMainVillageScene → startGame → startGame:,同一次 CCScheduler tick);
    //   冷启动则是 -[LoadingLayer update:] 0x12f30c performSelectorOnMainThread:loadTarget waitUntilDone:NO → perform 相位 →
    //   loadTarget 0x12f0ea startGame:。两路都在同一轮受理点里先跑本函数、再分发 1084,那时 startGame: 早已返回:
    //   curSceneId 在 0x12efbc(回村 0x241660)已置 1、userInfoLayer 在 0x193f0 已赋值、initGameData(0x19e26)以 gameMode==1 为前提,
    //   原版 currentGameMode 门放行。
    request_vip_info(env, "进村补发 getVipInfo(0x19ca0)");
}

// ─────────────────────────────── [2026-09-16] E-03 每日任务 1074 ───────────────────────────────

/// 主村每日任务表 dec/DailyQuest.dat(34 条,包内静态数据)的 take_level,下标 = 任务 ID − 1。
const DAILY_MAIN_TAKE_LEVEL: [u8; 34] = [
    1, 2, 4, 5, 6, 8, 9, 10, 10, // type 1 建造 ID 1-9
    1, 5, 5, 8, 8, 10, 10, // type 2 摆放 ID 10-16
    1, 5, 5, 7, 8, 9, // type 3 收获 ID 17-22
    1, 6, 10, // type 4 打工 ID 23-25
    3, 4, 9, // type 5 小游戏 ID 26-28
    1, 5, 7, 9, 10, 10, // type 6 条件 ID 29-34
];
/// 主村列表第 i 条映射到的任务 ID 段 (起始 ID, 条数)。
/// -[GameData hashDailyQuestIdInMainVillage:]@0x829f8 按「GameData 列表里已有几条」tbb 分派(0x82aa0,跳转表字节 03 11 1f 2d 3a 47,
/// 已对二进制原始字节核对):第 0..5 条依次取 v%9+1、v%7+10、v%6+17、v%3+23、v%3+26、v%6+29,正好是 type 1..6 的 ID 段。
/// 也就是说原版主村每天 6 条、每个类型各 1 条;服务器给的是任意整数,ID 由客户端映射,回包不能直接填任务 ID。
const DAILY_MAIN_SLOTS: [(u32, u32); 6] = [(1, 9), (10, 7), (17, 6), (23, 3), (26, 3), (29, 6)];
/// 主村回包的原始值个数。-[NetworkManager parseDailyTaskListWithSceneId:pos:len:] 主村分支(0x1c055a..0x1c059c)会把第 1 个值
/// 再追加到列表末尾,所以回 5 个值,客户端列表就是 6 条(第 6 条 = v0%6+29)。
/// 回 6 个会让第 7 条走「已有条数 > 5」的 v % 条数 分支(0x82a82),可能映射成不存在的 ID 0。
const DAILY_MAIN_WIRE_COUNT: usize = 5;
/// 第 1 个原始值的取值范围 0..18(9 与 6 的最小公倍数):它同时决定第 1 条与第 6 条。
const DAILY_MAIN_V0_SPAN: u32 = 18;
/// [2026-10-04 第八轮 R8-B2] 离线时第 6 条固定取 type 6 段的偏移 0 = ID 29「拜访 1 个推荐好友」(act_condition 1,take_level 1)。
/// ID 30-34 要拜访 2~6 个不同的人:计数 -[DailyQuest visitFriendsVillage:]@0x341e74 按 userId 去重(0x341f0c),
/// 推荐格、好友格的两个计数点 0x107e18、0x10829e 都在 isReachable 网络门之后,离线唯一能计数的是丝尔特庄园(0x1086d4),
/// 同一 userId 只算 1 次,所以离线当天做不完。选题规则本就是移植者自拟(原版在服务器),只收窄取值,不动任何原版数据。
const DAILY_MAIN_OFFLINE_VISIT_OFFSET: u32 = 0;

/// 黄金岛每日任务表 dec/DailyQuestHV.dat(15 条)的 take_level,下标 = 任务 ID − 1。
const DAILY_HV_TAKE_LEVEL: [u8; 15] = [18, 20, 21, 18, 24, 18, 24, 18, 30, 9, 1, 1, 1, 1, 1];
/// 黄金岛列表第 i 条的 ID 段:-[GameData hashDailyQuestIdInHolidayVillage:]@0x83f40 对已有 0/1/2 条分别取
/// v%5+1、v%5+6、v%5+11(0x84066 / 0x84026 / 0x84044);解析函数岛分支不追加重复值 → 每天 3 条。
const DAILY_HV_SLOTS: [(u32, u32); 3] = [(1, 5), (6, 5), (11, 5)];

/// 客户端判「今天」的口径:-[GameData updateDailyQuestListWithCurrentServerData:] 把截止时间设为
/// t + 86400 − ((t + 28800) % 86400)(0x82fe2..0x83014,t = 回包服务器时间换成的 CFAbsoluteTime),即写死北京时间的下一个 0 点,
/// 与 MOLE_TZ 无关。选题的「同一天」按同一公式算,列表只在客户端自己认为跨天时才换(2001-01-01 正好是日界,换算成 unix 不变)。
fn daily_day_key(cf: u32) -> u32 {
    let unix = i64::from(cf) + 978_307_200;
    let (year, month, day) = civil_from_days((unix + 28_800).div_euclid(86_400));
    LocalDate { year, month, day }.ymd()
}

/// [2026-09-16] X1-01 UserInfoData.curLevel_(i,编译期 +16)的 _OBJC_IVAR 槽,存的是密文(re.py ivar 核对)。
/// -[UserInfoData curLevel]@0xbaffc 读这个槽(0xbb00c/0xbb026)后调 +[CryptUtils decryptInt:]@0x124b34 解密:
/// `movw r0,#0x1011; movt r0,#0x101; eors r0,r2; bx lr`。偏移运行时从槽里现读。
const SLOT_UI_CUR_LEVEL: u32 = 0xb03ff8;
/// curLevel 密文的 XOR 掩码(decryptInt: 与 encryptInt:@0x124b28 同一常量)。
const CUR_LEVEL_XOR: u32 = 0x0101_1011;
/// 解出的等级超过它就当没读对(等级表 114_0 共 52 级;密文槽若存的是明文 0,会解出 0x01011011)。
const CUR_LEVEL_SANE_MAX: i32 = 999;

/// 主村玩家真实等级:[[GameData sharedInstance] userInfoData] curLevel(getter 自己解 XOR 混淆);取不到对象按 1。
/// [2026-09-16] X1-01 修改器「等级=N」开着时,mole_cheats 的 FORCE_LEVEL 臂对所有调用者(含宿主 msg_send)的 curLevel 都返回强制等级。
///   选题结果却按天写进 mole_activity.dat、同一天原样复用:关掉作弊甚至重启后,真实 1~9 级的玩家当天仍拿着 take_level 9/10 的任务,
///   岛上的 hv_daily_vals 同样受影响。那个臂只该改显示(与深扫 #5 encryptCurLevel 同类)。所以作弊开着时不发 curLevel,
///   直接读密文 ivar,按原 getter 解密。返回 None = 作弊开着且读不到可信的真实等级,调用方只临时选题回包、不落盘。
///   作弊关着照旧发消息。
fn main_player_level(env: &mut Environment) -> Option<i32> {
    let gd = singleton(env, "GameData", "sharedInstance");
    if gd == nil {
        return Some(1);
    }
    let ui_sel = sel_named(env, "userInfoData");
    let ui: id = msg_send(env, (gd, ui_sel));
    if ui == nil {
        return Some(1);
    }
    let forced = crate::mole_cheats::level();
    if forced > 0 {
        let real = read_ivar_u32(env, ui, SLOT_UI_CUR_LEVEL).map(|c| (c ^ CUR_LEVEL_XOR) as i32);
        return match real {
            Some(lv) if (0..=CUR_LEVEL_SANE_MAX).contains(&lv) => {
                let lv = lv.max(1);
                log!(
                    "[ACTIVITY] 每日任务选题:修改器「等级={}」开着,不发被覆盖的 curLevel,按密文 ivar 解出的真实等级 {} 选题",
                    forced,
                    lv
                );
                Some(lv)
            }
            other => {
                log!(
                    "[ACTIVITY] 每日任务选题:修改器「等级={}」开着,真实等级读不到(解密结果 {:?}),本次只临时选题回包、不落盘",
                    forced,
                    other
                );
                None
            }
        };
    }
    let lv_sel = sel_named(env, "curLevel");
    let lv: i32 = msg_send(env, (ui, lv_sel));
    Some(lv.max(1))
}

/// 某一条的候选偏移(相对该段起始 ID):take_level ≤ level 的全部偏移;一个都没有时退回 take_level 最低的那个
/// (第几条对应哪个类型是客户端写死的,不能空着不发)。
fn daily_slot_candidates(take_levels: &[u8], start: u32, count: u32, level: i32) -> Vec<u32> {
    let lv = |o: u32| i32::from(take_levels[(start + o - 1) as usize]);
    let ok: Vec<u32> = (0..count).filter(|&o| lv(o) <= level).collect();
    if !ok.is_empty() {
        return ok;
    }
    vec![(0..count).min_by_key(|&o| lv(o)).unwrap_or(0)]
}

/// 按日期确定性地挑主村 5 个原始值。规则为移植者自拟(原版选题在服务器,私服也没实现):
/// 每个类型在 take_level ≤ 当前主村等级的任务里随机取一条,没有够得着的就取该类型等级要求最低的一条。
/// 第 1 个值同时决定第 1 条(v%9+1)与第 6 条(v%6+29),在 0..18 里找两边都够得着的取值,找不到就只保证第 1 条。
/// [2026-10-04 第八轮 R8-B2] 第 6 条离线固定为「拜访 1 个推荐好友」(见 DAILY_MAIN_OFFLINE_VISIT_OFFSET),第 1 个值因此只取
/// 0/6/12,第 1 条随之只会是 ID 1/7/4,仍按等级过滤。当天已落盘的选题照旧复用,次日起生效。
fn pick_daily_main(ymd: u32, level: i32) -> Vec<u32> {
    let mut state = splitmix64(u64::from(ymd) ^ 0x4d4f_4c45_0432);
    let mut next = || {
        state = splitmix64(state);
        state
    };
    let (s0, n0) = DAILY_MAIN_SLOTS[0];
    let (_, n5) = DAILY_MAIN_SLOTS[5];
    let ok0 = daily_slot_candidates(&DAILY_MAIN_TAKE_LEVEL, s0, n0, level);
    let ok5 = [DAILY_MAIN_OFFLINE_VISIT_OFFSET];
    let mut pool: Vec<u32> = (0..DAILY_MAIN_V0_SPAN)
        .filter(|v| ok0.contains(&(v % n0)) && ok5.contains(&(v % n5)))
        .collect();
    if pool.is_empty() {
        pool = (0..DAILY_MAIN_V0_SPAN)
            .filter(|v| ok0.contains(&(v % n0)))
            .collect();
    }
    let mut vals = Vec::with_capacity(DAILY_MAIN_WIRE_COUNT);
    vals.push(pool[(next() % pool.len() as u64) as usize]);
    for &(start, count) in &DAILY_MAIN_SLOTS[1..DAILY_MAIN_WIRE_COUNT] {
        let cands = daily_slot_candidates(&DAILY_MAIN_TAKE_LEVEL, start, count, level);
        vals.push(cands[(next() % cands.len() as u64) as usize]);
    }
    vals
}

/// 黄金岛 3 个原始值:每一条在 take_level ≤ 等级的任务里随机取一条。等级口径为移植者自拟:DailyQuestHV.dat 的 take_level
/// 最高到 30,而岛升级表 levelupHV.dat 只有 26 级,按岛等级会有任务永远拿不到,所以暂用主村等级。
fn pick_daily_island(ymd: u32, level: i32) -> Vec<u32> {
    let mut state = splitmix64(u64::from(ymd) ^ 0x4d4f_4c45_1074);
    DAILY_HV_SLOTS
        .iter()
        .map(|&(start, count)| {
            state = splitmix64(state);
            let cands = daily_slot_candidates(&DAILY_HV_TAKE_LEVEL, start, count, level);
            cands[(state % cands.len() as u64) as usize]
        })
        .collect()
}

/// 旁路档里记下的原始值是否还能用(个数与每条的取值范围)。
fn daily_vals_valid(vals: &[u32], island: bool) -> bool {
    if island {
        vals.len() == DAILY_HV_SLOTS.len()
            && vals
                .iter()
                .zip(DAILY_HV_SLOTS.iter())
                .all(|(&v, &(_, n))| v < n)
    } else {
        vals.len() == DAILY_MAIN_WIRE_COUNT
            && vals[0] < DAILY_MAIN_V0_SPAN
            && vals[1..]
                .iter()
                .zip(DAILY_MAIN_SLOTS[1..].iter())
                .all(|(&v, &(_, n))| v < n)
    }
}

/// 原始值 → 客户端映射出的任务 ID(只用于日志)。
fn daily_ids(vals: &[u32], island: bool) -> Vec<u32> {
    if island {
        vals.iter()
            .zip(DAILY_HV_SLOTS.iter())
            .map(|(&v, &(s, n))| v % n + s)
            .collect()
    } else {
        let mut ids: Vec<u32> = vals
            .iter()
            .zip(DAILY_MAIN_SLOTS.iter())
            .map(|(&v, &(s, n))| v % n + s)
            .collect();
        if let Some(&v0) = vals.first() {
            let (s5, n5) = DAILY_MAIN_SLOTS[5];
            ids.push(v0 % n5 + s5);
        }
        ids
    }
}

/// 取当天的原始值:旁路档里有同一天、格式正确的记录就原样复用,否则挑一次并存档(之后同一天不受升级影响)。
/// 同一天必须回同一份:原版 update… 在截止时间未到时保留进度(unfinishedDailyQuestData 的 currentDoingQuestId/nextQuestId),
/// 却会整表替换列表(0x82c6c removeAllObjects 后重填),列表一变进度就对不上;而 isDailyQuestListForTodayRecieved 在列表为空时
/// 会反复发 1074(0x8329e),每次重启列表对象也是空的,都会再要一次。
fn daily_values_for_today(env: &mut Environment, island: bool, ymd: u32) -> Vec<u32> {
    let mut st = load_state(env);
    let (day, stored) = if island {
        (st.hv_daily_day, &st.hv_daily_vals)
    } else {
        (st.daily_day, &st.daily_vals)
    };
    if day == ymd && daily_vals_valid(stored, island) {
        return stored.clone();
    }
    let (level, persist) = match main_player_level(env) {
        Some(lv) => (lv, true),
        // [2026-09-16] X1-01 修改器等级开着且读不到真实等级:按最保守的 1 级选(每条都做得了),本次只回包,
        //   不写 daily_day/daily_vals(岛上是 hv_daily_day/hv_daily_vals),下一次请求(比如重启后客户端列表为空再要)重新选题。
        None => (1, false),
    };
    let vals = if island {
        pick_daily_island(ymd, level)
    } else {
        pick_daily_main(ymd, level)
    };
    if !persist {
        log!(
            "[ACTIVITY] 每日任务选题({}):日期={} 按 1 级临时选题 原始值={:?} → 任务ID={:?},不写旁路档(修改器等级开着且读不到真实等级)",
            if island { "黄金岛" } else { "主村" },
            ymd,
            vals,
            daily_ids(&vals, island)
        );
        return vals;
    }
    if island {
        st.hv_daily_day = ymd;
        st.hv_daily_vals = vals.clone();
    } else {
        st.daily_day = ymd;
        st.daily_vals = vals.clone();
    }
    save_state(env, &st);
    log!(
        "[ACTIVITY] 每日任务选题({}):日期={} 主村等级={} 原始值={:?} → 任务ID={:?}(选题规则为移植者自拟,非原版数据)",
        if island { "黄金岛" } else { "主村" },
        ymd,
        level,
        vals,
        daily_ids(&vals, island)
    );
    vals
}

/// 主村 1074 回包(parseDailyTaskListWithSceneId:pos:len:@0x1c0398 逐字节核实):
/// [u8 场景标志 0=主村(1=岛,0x1c0420)][u32 unix 秒(0x1c046c 转 double、减 kCFAbsoluteTimeIntervalSince1970 → setCurrentServerTime:)]
/// [u32 个数][u32 原始值 × 个数]。时间取 now_cf_u32,与离线 NewSceneTimer getCurrentServerTime 同一单调时钟(含时间旅行偏移),
/// 否则 isDailyQuestListForTodayRecieved 拿截止时间比较时会每次都判成跨天并清进度。
/// [2026-09-25 第五轮遗留 MISC-4] 截止时间由 updateDailyQuestListWithCurrentServerData: 在 0x82fe6 用回包时间 +0x7080 算出,跨天判定
/// -[GameData isDailyQuestListForTodayRecieved]@0x830c0 在 0x831d4 比的也是回包 currentServerTime,两边都是本函数给的时间。
/// 主村倒计时 -[DailyQuest leftTime] 经 -[DailyQuest currentTime]@0x342870 → -[WrapperManager getCurrentTime]@0x2615ec,在 curSceneId≠10 时
/// 0x261656 直读 CFAbsoluteTimeGetCurrent(墙钟);进程内宿主时间回拨时主村倒计时会多出回拨量——这与原版「截止按服务器时间、主村
/// 倒计时按设备时钟」一致,不另处理。岛上倒计时走 getCurrentServerTime,与回包对齐。
/// GameData.dailyQuestData 不足 34 条时不回包:hashDailyQuestIdInMainVillage: 在表条数小于原始值时做 v % 条数(0x82a50),
/// 表没加载好(0 条)会除以 0。
fn encode_daily_task_list_main(env: &mut Environment) -> Option<Vec<u8>> {
    let gd = singleton(env, "GameData", "sharedInstance");
    let loaded: usize = if gd == nil {
        0
    } else {
        let dq_sel = sel_named(env, "dailyQuestData");
        let arr: id = msg_send(env, (gd, dq_sel));
        if arr == nil {
            0
        } else {
            let count_sel = sel_named(env, "count");
            let n: GuestUSize = msg_send(env, (arr, count_sel));
            n as usize
        }
    };
    if loaded < DAILY_MAIN_TAKE_LEVEL.len() {
        log!(
            "[ACTIVITY] 每日任务 cmd=1074:GameData.dailyQuestData 只有 {} 条(应为 {}),表未加载好,本次不回包",
            loaded,
            DAILY_MAIN_TAKE_LEVEL.len()
        );
        return None;
    }
    let cf = now_cf_u32();
    let ymd = daily_day_key(cf);
    let vals = daily_values_for_today(env, false, ymd);
    let unix = (u64::from(cf) + 978_307_200).min(u64::from(u32::MAX)) as u32;
    let mut b = Vec::with_capacity(9 + vals.len() * 4);
    b.push(0u8);
    put_u32(&mut b, unix);
    put_u32(&mut b, vals.len() as u32);
    for &v in &vals {
        put_u32(&mut b, v);
    }
    Some(b)
}

/// 黄金岛:照 parseDailyTaskListWithSceneId:pos:len: 岛分支在宿主侧构造(0x1c03e4 alloc/init → 0x1c044c setSceneId:10 →
/// 0x1c0496 setCurrentServerTime: → 0x1c04f8 起逐个 [currentQuestList addObject:[NSNumber numberWithInt:]] →
/// 0x1c05fe [[GameData sharedInstance] updateDailyQuestListInHolidayVillageWithCurrentServerData:] → 0x1c0610 release),
/// 列表更新、排序、截止时间、进度保留全部走原版。只在运行循环回调里调用。
/// NewSceneData.dailyQuestData 不足 15 条时不构造(hashDailyQuestIdInHolidayVillage: 0x83fea 同样会做 v % 条数)。
fn island_daily_quest_apply(env: &mut Environment) {
    let nsd = singleton(env, "NewSceneData", "sharedInstance");
    let gd = singleton(env, "GameData", "sharedInstance");
    if nsd == nil || gd == nil {
        log!("[ACTIVITY] 黄金岛每日任务:NewSceneData/GameData 未就绪,放弃构造");
        return;
    }
    let dq_sel = sel_named(env, "dailyQuestData");
    let arr: id = msg_send(env, (nsd, dq_sel));
    let loaded: usize = if arr == nil {
        0
    } else {
        let count_sel = sel_named(env, "count");
        let n: GuestUSize = msg_send(env, (arr, count_sel));
        n as usize
    };
    if loaded < DAILY_HV_TAKE_LEVEL.len() {
        log!(
            "[ACTIVITY] 黄金岛每日任务:NewSceneData.dailyQuestData 只有 {} 条(应为 {}),表未加载好,放弃构造",
            loaded,
            DAILY_HV_TAKE_LEVEL.len()
        );
        return;
    }
    let list_cls = env.objc.get_known_class("DailyQuestList", &mut env.mem);
    let num_cls = env.objc.get_known_class("NSNumber", &mut env.mem);
    if list_cls == nil || num_cls == nil {
        log!("[ACTIVITY] 黄金岛每日任务:找不到 DailyQuestList/NSNumber 类,放弃构造");
        return;
    }
    let cf = now_cf_u32();
    let ymd = daily_day_key(cf);
    let vals = daily_values_for_today(env, true, ymd);

    let alloc_sel = sel_named(env, "alloc");
    let init_sel = sel_named(env, "init");
    let list: id = msg_send(env, (list_cls, alloc_sel));
    let list: id = msg_send(env, (list, init_sel));
    if list == nil {
        log!("[ACTIVITY] 黄金岛每日任务:DailyQuestList init 返回 nil,放弃构造");
        return;
    }
    let set_scene = sel_named(env, "setSceneId:");
    let _: () = msg_send(env, (list, set_scene, 10u32));
    let set_time = sel_named(env, "setCurrentServerTime:");
    let _: () = msg_send(env, (list, set_time, cf));
    let cql_sel = sel_named(env, "currentQuestList");
    let quests: id = msg_send(env, (list, cql_sel));
    let nwi_sel = sel_named(env, "numberWithInt:");
    let add_sel = sel_named(env, "addObject:");
    for &v in &vals {
        let num: id = msg_send(env, (num_cls, nwi_sel, v as i32));
        let _: () = msg_send(env, (quests, add_sel, num));
    }
    let update = sel_named(
        env,
        "updateDailyQuestListInHolidayVillageWithCurrentServerData:",
    );
    let _: () = msg_send(env, (gd, update, list));
    release(env, list);
    log!(
        "[ACTIVITY] 黄金岛每日任务:宿主侧构造 DailyQuestList(sceneId=10 serverTime={} 原始值={:?} → 任务ID={:?}),交给 updateDailyQuestListInHolidayVillageWithCurrentServerData:",
        cf,
        vals,
        daily_ids(&vals, true)
    );
    // [2026-09-16] 复审补:真回包解析完还会分发给 HolidayVillageLayer,宿主侧构造绕过了这一步,照原版补上 NPC 提示图标。
    island_daily_quest_prompt(env, gd);
}

/// [2026-09-16] 复审补 E-03:原版岛上 1074 回包在 parse 之后还要走 -[HolidayVillageLayer onNewSceneGameDataCommandReceived:]
/// 的 1074 臂(0x23e744..0x23ebda),宿主侧构造没有这次分发,不补的话岛上日常 NPC 头顶不出提示图标。逐条照原版:
/// - NPC 编号 = curSceneId==1 ? 98 : 96(0x23e7a6/0x23e7ae);curSceneId==1 走另一支(0x23e7b8,主村数据),与岛上列表无关,这里不做;
/// - 取 [GameData unfinishedDailyQuestDataInHolidayVillage] 与 dailyQuestListInHolidayVillage,[[ActorManager Instance] GetNpcActor:96]
///   非 nil、两者非 nil、currentQuestList.count>0,且 currentDoingQuestId>0 或 nextQuestId>=1 时 [actor showPromptIcon:YES](0x23ebba),
///   再 [[DailyQuest sharedInstance] resetTimer](0x23ebc8 → 公共尾 0x23df1a);任一条件不满足原版直接收尾(0x23ea68),这里也什么都不做。
/// 主村走回环,GameManager onCommandReceived: 的同一臂(0x24978..0x24a46)由原版照跑,不用补。只在运行循环回调里调用。
fn island_daily_quest_prompt(env: &mut Environment, gd: id) {
    let sm = singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        return;
    }
    let cur_sel = sel_named(env, "curSceneId");
    let cur: i32 = msg_send(env, (sm, cur_sel));
    if cur == 1 {
        return;
    }
    let unfinished_sel = sel_named(env, "unfinishedDailyQuestDataInHolidayVillage");
    let unfinished: id = msg_send(env, (gd, unfinished_sel));
    let list_sel = sel_named(env, "dailyQuestListInHolidayVillage");
    let list: id = msg_send(env, (gd, list_sel));
    let am = singleton(env, "ActorManager", "Instance");
    if am == nil {
        return;
    }
    let get_npc = sel_named(env, "GetNpcActor:");
    let actor: id = msg_send(env, (am, get_npc, 96i32));
    if actor == nil || unfinished == nil || list == nil {
        return;
    }
    let cql_sel = sel_named(env, "currentQuestList");
    let quests: id = msg_send(env, (list, cql_sel));
    if quests == nil {
        return;
    }
    let count_sel = sel_named(env, "count");
    let n: GuestUSize = msg_send(env, (quests, count_sel));
    if n == 0 {
        return;
    }
    let doing_sel = sel_named(env, "currentDoingQuestId");
    let doing: i32 = msg_send(env, (unfinished, doing_sel));
    if doing <= 0 {
        let next_sel = sel_named(env, "nextQuestId");
        let next: i32 = msg_send(env, (unfinished, next_sel));
        if next < 1 {
            return;
        }
    }
    let show_sel = sel_named(env, "showPromptIcon:");
    let _: () = msg_send(env, (actor, show_sel, true));
    let dq = singleton(env, "DailyQuest", "sharedInstance");
    if dq != nil {
        let reset_sel = sel_named(env, "resetTimer");
        let _: () = msg_send(env, (dq, reset_sel));
    }
    log!("[ACTIVITY] 黄金岛每日任务:照原版分发臂给日常 NPC(96)挂提示图标并 resetTimer");
}

// ─────────────────────────────── [2026-09-25 第五轮遗留 V] VIP 信息 1084 回包分发 ───────────────────────────────

/// [2026-09-25 第五轮遗留 V] 「VIP 信息 1084 回包分发」是否在排队。只由 request_vip_info 置位、vip_info_poll 清零;
/// 同一轮运行循环里的多次请求天然合并成一次分发(分发幂等:已解锁的成就会被原版 checkInAlreadyUnlockList: 跳过)。
static VIP_INFO_PENDING: AtomicBool = AtomicBool::new(false);
/// +[GameNewScene scene]@0x23ed08 的单例槽(0x23ed14 `add r4, pc` 得 0xb40bd8)。只读不建:槽空时 +scene 会在 0x23ed36
/// alloc/init 一个新场景;+purgeSharedInstance@0x23ed50 先 release 再清零,所以读到的非 0 值一定是活对象。
const SLOT_GAME_NEW_SCENE: u32 = 0xb40bd8;
/// 岛 1084 臂 0x23e896 `movw r2, #0x7d0f`:贝壳树(SuperShellTree)的唯一对象 ID。
const VIP_INFO_SHELLTREE_ID: i32 = 32015;
/// 岛 HUD(NewSceneUserInfoLayer,UserInfoLayer 的子类)的 tag:-[GameNewScene addMainVillageLayer:] 0x23ee5a 以 tag 3 挂上,
/// 1084 臂 0x23e818 `movs r2, #0x3` 取它。
const ISLAND_HUD_TAG: i32 = 3;
/// 岛 1084 臂 0x23e870 `mov.w r2, #0x1000`(全二进制唯一传 0x1000 的调用点):checkConditions:itemId: 0x334c16 → checkAchieveVIP:。
const ACH_TYPE_VIP_ISLAND: i32 = 0x1000;
/// 主村 1084 臂 0x23a82 `mov.w r2, #0x800`(AchievementControl 唯一传 0x800 的调用点):checkConditions:itemId: 0x1f6d44 → checkAchieve_ReqVIP。
/// 岛上 -[NewSceneShop onAlarmFlagTouched] 0x31ff3a(r2 在 0x31ff2a 置 0x800)给 NewSceneAchievement checkConditions:itemId: 传的 0x800
/// 是岛成就的另一类型,与 VIP 无关(岛 VIP 是 0x1000)。
const ACH_TYPE_VIP_MAIN: i32 = 0x800;

/// [2026-09-25 第五轮遗留 V] 请一次「VIP 信息 1084 回包分发」:离线等价于原版发出 getVipInfo、等服务器回包。
/// 纯原子操作,不发消息、不碰寄存器,可以在任何栈上调用(含 drawScene / CCScheduler 帧栈与 SHELLHOOK)。
/// 调用方:getVipInfo 臂(原版所有入口)、进村补发(startgame_resend_offline,对应 0x19ca0)、假充值(mole_items::on_shells_purchased,
/// 对应 0x117dcc)。在线模式直接返回。
pub fn request_vip_info(env: &Environment, why: &str) {
    if env.options.network_access {
        return;
    }
    let first = !VIP_INFO_PENDING.swap(true, O);
    log!(
        "[ACTIVITY] VIP 信息 1084:{},{}(本轮运行循环末尾补回包分发,不走 parseVipInfo,不改本地 VIP 值)",
        why,
        if first { "已排队" } else { "本轮已排过,合并" }
    );
}

/// [2026-10-03 第六波] 黄金岛每日任务(E-03,参数 10 的 getDailyTaskListFromServerWithSceneId:)与岛限时折扣(K6,
/// getDiscountObjectsListFormServerWithMapId:10)的「待受理」标志。两个请求在岛上的调用点都在 -[HolidayVillageLayer onEnter]
/// (0x239484 / 0x23946a),跑在切场景的 drawScene 帧栈上;以前在那里直接发 performSelector:withObject:afterDelay:0 排回调,
/// 等于在帧栈上发宿主消息(第五轮 V 复核指出)。现在帧栈上只置标志,由运行循环受理点 run_loop_poll 在当前调用栈整个返回之后执行,
/// 时机与原 afterDelay:0 等价。
static ISLAND_DAILY_PENDING: AtomicBool = AtomicBool::new(false);
static ISLAND_DISCOUNT_PENDING: AtomicBool = AtomicBool::new(false);

/// [2026-10-03] 运行循环受理点是否有活要干。ns_run_loop::run_run_loop 主线程每轮都调,平时只有几次原子读。
/// 以前这里只看 VIP_INFO_PENDING,而岛日常/岛折扣是挂在 VIP 受理里顺带做的:进岛时三者同一帧排上所以碰巧能跑,
/// 单独排上岛日常或岛折扣时要等到下一次 1084 才会受理。现在每个标志都单独算。
pub fn run_loop_pending() -> bool {
    VIP_INFO_PENDING.load(O)
        || ISLAND_DAILY_PENDING.load(O)
        || ISLAND_DISCOUNT_PENDING.load(O)
        || STARTGAME_RESEND_PENDING.load(O)
        || REQUESTS_PENDING.load(O)
        || LOOPBACK_REQUESTED.load(O)
        || LOOPBACK_ARMED.load(O)
        || FIREWORK_WAITING.load(O)
}

/// 运行循环受理点。只由 ns_run_loop::run_run_loop 在主线程、本轮 perform 相位之后、「关键操作即时落盘」受理点
/// (mole_cheats::island_flush_now_poll)之前调用:这时本轮触摸、定时器(CADisplayLink → CCDirector mainLoop → drawScene →
/// CCScheduler)、perform 队列都已返回,栈上没有任何游戏方法体,可以自由发宿主消息;不在 intercept 里,不涉及 r0-r3 快照。
/// 照第五轮 FLUSH(置脏只排标志、运行循环统一受理)的写法,原因是规则「帧栈上不发宿主消息」(本仓血泪:帧栈里 msg_send
/// 曾饿死运行循环、触发调度器重入活锁)。
/// perform 相位不建自动释放池,这里自建一个包住本轮全部受理(updateUI4VIP 里的 stringWithFormat:/spriteFrameByName:、
/// 成就解锁发奖链、回包解析都会产生自动释放对象),当场 drain;原版 iOS 每轮运行循环都会 drain,两者等价。
/// 本轮顺序:
/// ① 上一轮入队的回包(以及到点的烟花重查)喂进原版解析链——等价原 afterDelay:0 排到下一轮的 moleActivityLoopback;
/// ② 离线进村补发(startGame: 臂只置了标志):发包方法经 sendPacket:commandId: 臂截下;
/// ③ 算截下的请求(含 ①② 里新截的)的应答并入队;
/// ④ 岛日常 / 岛折扣;⑤ VIP 信息 1084 分发(与以前一样在进村补发的同一轮,早于 1049/1058/1074 回包);
/// ⑥ 本轮新入队的回包留到下一轮 ① 再喂。
pub fn run_loop_poll(env: &mut Environment) {
    let feed = LOOPBACK_ARMED.swap(false, O) || firework_retry_due();
    let work = feed
        || STARTGAME_RESEND_PENDING.load(O)
        || REQUESTS_PENDING.load(O)
        || ISLAND_DAILY_PENDING.load(O)
        || ISLAND_DISCOUNT_PENDING.load(O)
        || VIP_INFO_PENDING.load(O);
    if work {
        let pool_cls = env.objc.get_known_class("NSAutoreleasePool", &mut env.mem);
        let new_s = sel_named(env, "new");
        let pool: id = msg_send(env, (pool_cls, new_s));
        if feed {
            loopback_feed_poll(env);
        }
        if STARTGAME_RESEND_PENDING.swap(false, O) {
            startgame_resend_poll(env);
        }
        if REQUESTS_PENDING.swap(false, O) {
            answer_pending_requests(env);
        }
        island_deferred_poll(env);
        vip_info_poll(env);
        let drain_s = sel_named(env, "drain");
        let _: () = msg_send(env, (pool, drain_s));
    }
    if LOOPBACK_REQUESTED.swap(false, O) {
        LOOPBACK_ARMED.store(true, O);
    }
}

/// [2026-10-03] ① 喂回包(原 moleActivityLoopback 回调臂做的事)。在线/岛上不回环:丢弃队列与等场景的烟花包。
fn loopback_feed_poll(env: &mut Environment) {
    if env.options.network_access || crate::mole_cheats::island_session_active() {
        if let Ok(mut q) = LOOPBACK_QUEUE.lock() {
            if !q.is_empty() {
                log!(
                    "[ACTIVITY] 回环:喂包时已在线或已进岛,丢弃 {} 个回包",
                    q.len()
                );
            }
            q.clear();
        }
        // [2026-09-16] A1-01 等场景的烟花包一并丢弃(已经进岛,村庄场景不会再挂 FireworkLayer)。
        firework_slot_put(None);
        FIREWORK_IN_FLIGHT.store(false, O);
        return;
    }
    let nm: id = Ptr::from_bits(LOOPBACK_NM.load(O));
    run_loopback(env, nm);
}

/// [2026-10-03] ② 离线进村补发(原 startGame: 臂就地做的事):1049(MOLE_DISCOUNT=off 时不发)→ startgame_resend_offline。
fn startgame_resend_poll(env: &mut Environment) {
    if env.options.network_access || crate::mole_cheats::island_session_active() {
        log!("[ACTIVITY] 进村补发:受理时已在线或已进岛,放弃");
        return;
    }
    let nm = singleton(env, "NetworkManager", "sharedInstance");
    if nm == nil {
        return;
    }
    if !discount_disabled() {
        let get_list = sel_named(env, "getDiscountListFromServer");
        let _: () = msg_send(env, (nm, get_list));
    }
    startgame_resend_offline(env, nm);
}

/// [2026-10-03] ③ 按截包顺序逐个算应答入队(下一轮喂)。截包之后已转在线或进岛的,整批放弃(与以前「回调到达时清队列」同口径)。
fn answer_pending_requests(env: &mut Environment) {
    let reqs: Vec<(u32, u32, Vec<u8>)> = match PENDING_REQUESTS.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    if reqs.is_empty() {
        return;
    }
    if env.options.network_access || crate::mole_cheats::island_session_active() {
        log!(
            "[ACTIVITY] 回环受理:截下 {} 个请求后已转在线或已进岛,放弃应答",
            reqs.len()
        );
        return;
    }
    for (nm_bits, cmd, req) in reqs {
        answer_request(env, Ptr::from_bits(nm_bits), cmd, &req);
    }
}

/// [2026-10-03 第六波] ④ 受理岛日常 / 岛折扣:先取走标志;不在岛上会话或已转在线时只记日志放弃(与原回调臂口径相同)。
fn island_deferred_poll(env: &mut Environment) {
    let daily = ISLAND_DAILY_PENDING.swap(false, O);
    let discount = ISLAND_DISCOUNT_PENDING.swap(false, O);
    if !daily && !discount {
        return;
    }
    if env.options.network_access || !crate::mole_cheats::island_session_active() {
        log!("[ACTIVITY] 黄金岛每日任务/折扣:受理时已不在岛上会话(或在线),放弃构造");
        return;
    }
    if daily {
        island_daily_quest_apply(env);
    }
    if discount {
        island_discount_apply(env);
    }
}

/// [2026-09-25 第五轮遗留 V] ⑤「VIP 信息 1084 回包分发」。调用点里 -[HolidayVillageLayer onEnter] 0x23949c 跑在切场景的 drawScene
/// 帧栈上,所以请求入口只置标志、在这里分发。时机上是同一轮运行循环末尾,原版是一次网络往返之后;分发臂开头都按 curSceneId
/// 自己判场景,与当时谁是代理无关。
fn vip_info_poll(env: &mut Environment) {
    if !VIP_INFO_PENDING.swap(false, O) {
        return;
    }
    if env.options.network_access {
        log!("[ACTIVITY] VIP 信息 1084:受理时已是在线模式,放弃分发");
        return;
    }
    vip_info_dispatch(env);
}

/// [2026-09-25 第五轮遗留 V] 照 parseData 公共尾 0xe6d9e..0xe6de4 的两次分发:原版先给 delegateGameData(GameManager)发
/// onCommandReceived:,再给 delegateNewSceneGameData(HolidayVillageLayer,全二进制唯一实现者)发 onNewSceneGameDataCommandReceived:;
/// 两条 1084 臂开头都判 [[SceneMannager sharedManager] curSceneId](主村臂 0x23a24 要 1,岛臂 0x23e7f2 要 10),
/// 所以这里按 curSceneId 分派与原版等价。岛上会话里真实值不是 1/10 时 mole_cheats 的 curSceneId 臂返回 10,与其它岛功能口径一致。
/// 修改器「强制 VIP」开着时跳过成就判定那一步(两边判定都读 [[GameData userVIPInfoData] vipLevelWithNewType],会读到强制等级、
/// 永久记下解锁并发奖),只刷新 HUD 与贝壳树。
fn vip_info_dispatch(env: &mut Environment) {
    let sm = singleton(env, "SceneMannager", "sharedManager");
    if sm == nil {
        log!("[ACTIVITY] VIP 信息 1084 分发:SceneMannager 单例还没建,原版两条分发臂都不做事");
        return;
    }
    let cur_sel = sel_named(env, "curSceneId");
    let cur: i32 = msg_send(env, (sm, cur_sel));
    let force_vip = crate::mole_cheats::is_on("force_vip");
    match cur {
        1 => vip_info_main_arm(env, force_vip),
        10 => vip_info_island_arm(env, force_vip),
        _ => {
            log!(
                "[ACTIVITY] VIP 信息 1084 分发:受理时 curSceneId={},原版两条分发臂(主村要 1、岛要 10)都不做事",
                cur
            );
        }
    }
}

/// [2026-09-25 第五轮遗留 V] 主村 1084 臂,照 -[GameManager onCommandReceived:] 0x239f6..0x23aaa 逐条执行:
/// [[GameManager sharedManager] userInfoLayer] 非 nil 时 isShowVIPFunctionsButton:YES(0x23a5e,v12@0:4c8)→
/// [[AchievementControl shareInstance] checkConditions:0x800](0x23a8e,v12@0:4i8)→ 重新取一次 userInfoLayer(0x23a92)发
/// updateUI4VIP(0x23aaa → 0x2265a,v8@0:4)。主村 updateUI4VIP 只有这条臂和好友村会调,以前离线主村 HUD 的 VIP 徽章从没刷新过。
/// 原版门照旧由原版自己判:checkConditions: 在 0x1f6c60/0x1f6c70 见 [[WrapperManager sharedManager] currentGameMode] 为 0/6 时
/// 静默返回,isShowVIPFunctionsButton:@0x592a0 在 currentGameMode!=1 时直接返回;日志里的 currentGameMode 只是诊断读数。
/// HUD 先用 object_has_method_named 判一下再发(原版直接发)。只在 vip_info_poll 里调用。
fn vip_info_main_arm(env: &mut Environment, force_vip: bool) {
    let wm = singleton(env, "WrapperManager", "sharedManager");
    let mode: i32 = if wm == nil {
        -1
    } else {
        let s = sel_named(env, "currentGameMode");
        msg_send(env, (wm, s))
    };
    let gm = singleton(env, "GameManager", "sharedManager");
    let uil_sel = sel_named(env, "userInfoLayer");
    let uil: id = if gm == nil {
        nil
    } else {
        msg_send(env, (gm, uil_sel))
    };
    if uil != nil
        && env
            .objc
            .object_has_method_named(&env.mem, uil, "isShowVIPFunctionsButton:")
    {
        let s = sel_named(env, "isShowVIPFunctionsButton:");
        let _: () = msg_send(env, (uil, s, true));
    }
    let ach_note = if force_vip {
        "强制 VIP 开着,已跳过(VIP 成就判定会读到强制等级并永久记下解锁、发奖;关掉强制 VIP 后下次触发再按真实等级判)"
    } else {
        let ac = singleton(env, "AchievementControl", "shareInstance");
        if ac == nil {
            "AchievementControl 单例不在,未判定"
        } else {
            let s = sel_named(env, "checkConditions:");
            let _: () = msg_send(env, (ac, s, ACH_TYPE_VIP_MAIN));
            "已交原版判定(原版在 currentGameMode 为 0/6 时静默跳过,0x1f6c60/0x1f6c70)"
        }
    };
    let uil2: id = if gm == nil {
        nil
    } else {
        msg_send(env, (gm, uil_sel))
    };
    let hud_done = uil2 != nil
        && env
            .objc
            .object_has_method_named(&env.mem, uil2, "updateUI4VIP");
    if hud_done {
        let s = sel_named(env, "updateUI4VIP");
        let _: () = msg_send(env, (uil2, s));
    }
    log!(
        "[ACTIVITY] VIP 信息 1084 分发(主村 curSceneId=1,currentGameMode={}):HUD {};AchievementControl checkConditions:0x800 {}",
        mode,
        if hud_done { "已刷新" } else { "没有 userInfoLayer" },
        ach_note
    );
}

/// [2026-09-25 第五轮遗留 V] 岛 1084 臂,照 -[HolidayVillageLayer onNewSceneGameDataCommandReceived:] 0x23e7f8..0x23e8f4 逐条执行:
/// [[GameNewScene scene] getChildByTag:3](0x23e822,@12@0:4i8)非 nil 时 isShowVIPFunctionsButton:YES(0x23e83a)与 updateUI4VIP
/// (0x23e84c)→ [[NewSceneAchievement shareInstance] checkConditions:0x1000](0x23e87c)→ [[ObjectManager sharedManager]
/// getUniqueObjectByObjectId:32015](0x23e8a2)非 nil 且 isKindOfClass:SuperShellTree(0x23e8da)时 rescheduleTree(0x23e8f4 →
/// 0x23df1c;全二进制唯一调用点:stopScheduler、removeFlag,再以 1.0 秒间隔重排 innerupdate:,旗子由约 1 秒后的 updateView 重建)。
/// 场景取单例槽 SLOT_GAME_NEW_SCENE,不发 +scene(槽空时它会新建场景);HUD 先用 object_has_method_named 判一下再发。
/// 原版门照旧由原版自己判:checkConditions:itemId: 在 0x3349c4/0x3349da 见 [[NewGameManager sharedManager] gameMode] 为 0/6 时早退;
/// 日志里的 gameMode 只是诊断读数。只在 vip_info_poll 里调用。
fn vip_info_island_arm(env: &mut Environment, force_vip: bool) {
    let ngm = singleton(env, "NewGameManager", "sharedManager");
    let mode: i32 = if ngm == nil {
        -1
    } else {
        let s = sel_named(env, "gameMode");
        msg_send(env, (ngm, s))
    };
    let slot: ConstPtr<u32> = Ptr::from_bits(SLOT_GAME_NEW_SCENE);
    let scene: id = Ptr::from_bits(env.mem.read(slot));
    let mut hud_done = false;
    if scene != nil {
        let tag_sel = sel_named(env, "getChildByTag:");
        let hud: id = msg_send(env, (scene, tag_sel, ISLAND_HUD_TAG));
        if hud != nil
            && env
                .objc
                .object_has_method_named(&env.mem, hud, "isShowVIPFunctionsButton:")
            && env
                .objc
                .object_has_method_named(&env.mem, hud, "updateUI4VIP")
        {
            let show_sel = sel_named(env, "isShowVIPFunctionsButton:");
            let _: () = msg_send(env, (hud, show_sel, true));
            let upd_sel = sel_named(env, "updateUI4VIP");
            let _: () = msg_send(env, (hud, upd_sel));
            hud_done = true;
        }
    }
    let ach_note = if force_vip {
        "强制 VIP 开着,已跳过(VIP 成就判定会读到强制等级并永久记下解锁、发奖;关掉强制 VIP 后下次触发再按真实等级判)"
    } else {
        let ach = singleton(env, "NewSceneAchievement", "shareInstance");
        if ach == nil {
            "NewSceneAchievement 单例不在,未判定"
        } else {
            let s = sel_named(env, "checkConditions:");
            let _: () = msg_send(env, (ach, s, ACH_TYPE_VIP_ISLAND));
            "已交原版判定(原版在 gameMode 为 0/6 时早退,0x3349c4/0x3349da)"
        }
    };
    let mut tree_done = false;
    let om = singleton(env, "ObjectManager", "sharedManager");
    let tree_cls = env.objc.get_known_class("SuperShellTree", &mut env.mem);
    if om != nil && tree_cls != nil {
        let g = sel_named(env, "getUniqueObjectByObjectId:");
        let obj: id = msg_send(env, (om, g, VIP_INFO_SHELLTREE_ID));
        if obj != nil {
            let isk = sel_named(env, "isKindOfClass:");
            let is_tree: bool = msg_send(env, (obj, isk, tree_cls));
            if is_tree {
                let r = sel_named(env, "rescheduleTree");
                let _: () = msg_send(env, (obj, r));
                tree_done = true;
            }
        }
    }
    log!(
        "[ACTIVITY] VIP 信息 1084 分发(黄金岛 curSceneId=10,gameMode={}):HUD {};NewSceneAchievement checkConditions:0x1000 {};贝壳树 32015 {}",
        mode,
        if hud_done { "已刷新" } else { "不在" },
        ach_note,
        if tree_done { "rescheduleTree" } else { "无活树" }
    );
}

// ─────────────────────────────── [2026-09-24 第四轮 K6 N-D4-1] 岛上 VIP 在线奖励离线提示 ───────────────────────────────

/// [2026-09-24 第四轮 K6 N-D4-1] OnlineTimeManager.reward_list_(@"NSMutableArray",编译期 +240;-[OnlineTimeManager init]@0x38a7d0 置 nil,
/// 唯一赋值在 setNextReward_time:NextReward_list:NextReward_level:@0x38aac8)。
const SLOT_OTM_REWARD_LIST: u32 = 0xb07fc0;
/// [2026-09-24 第四轮 K6 N-D4-1] OnlineTimeManager.last_rew_data_(@"OnlineRewardData",编译期 +248;showRewardLayer 0x38b0ec 读它)。
const SLOT_OTM_LAST_REW_DATA: u32 = 0xb07fc4;
/// [2026-09-24 第四轮 K6 N-D4-1] OnlineTimeManager 的实例大小(objc_meta 里 class_ro_t.instanceSize = 265)。
/// 从槽里现读的偏移为 0 或读 4 字节会越过它,就当槽没按预期初始化,放行原版。
const OTM_INSTANCE_SIZE: u32 = 265;

/// [2026-09-24 第四轮 K6 N-D4-1] 读 OnlineTimeManager 的一个对象型 ivar(偏移从 _OBJC_IVAR 槽现读,不写死 +240/+248)。
/// 偏移异常返回 None。只读内存、不发消息。
fn otm_ivar_id(env: &Environment, otm: id, slot: u32) -> Option<id> {
    if otm == nil {
        return None;
    }
    let off: u32 = env.mem.read(ConstPtr::<u32>::from_bits(slot));
    if off == 0 || off.saturating_add(4) > OTM_INSTANCE_SIZE {
        return None;
    }
    let bits: u32 = env
        .mem
        .read(ConstPtr::<u32>::from_bits(otm.to_bits().wrapping_add(off)));
    Some(Ptr::from_bits(bits))
}

/// [2026-09-24 第四轮 K6 N-D4-1] showRewardLayer 这次会不会建出空奖励板:last_rew_data_ 为 nil(原版 0x38b10a 走 0x38b15e 那一支)
/// 且 reward_list_ 为 nil 或 count 为 0(NewRewardsLayer type 2 在 rewardsList 为空时一个格子都不建)。偏移读不出一律按「不空」,放行原版。
/// reward_list_ 非 nil 时会发一次 count(宿主 msg_send),调用方负责恢复 r0-r3。
fn online_reward_board_would_be_empty(env: &mut Environment, otm: id) -> bool {
    let (Some(list), Some(last)) = (
        otm_ivar_id(env, otm, SLOT_OTM_REWARD_LIST),
        otm_ivar_id(env, otm, SLOT_OTM_LAST_REW_DATA),
    ) else {
        return false;
    };
    if last != nil {
        return false;
    }
    if list == nil {
        return true;
    }
    let count_sel = sel_named(env, "count");
    let n: GuestUSize = msg_send(env, (list, count_sel));
    n == 0
}

/// [2026-09-24 第四轮 K6 N-D4-1] 弹游戏自带的离线提示:文案 [[NSBundle mainBundle] localizedStringForKey:@"ACTION_CENTER_NETWARNING" value:@"" table:nil]
/// (「该功能需要联网才能使用哦!」,原版 -[UserInfoLayer onButtonActionFunctionsSelected:] 0x5a55a 等 37 处同一文案),
/// 框 `[[MessageBox sharedInstance] showWithTarget:nil selector:0 title:nil message:msg type:6 vipgold:0]`(type 6 只有「确定」,关框无回调)。
/// mole_cheats 的 game_localized_string / show_game_message_box 是私有的,这里照抄同一调用序列(本文件 block_paid_action_in_time_travel 同理):
/// 签名 v32@0:4@8:12@16@20i24i28,宿主 msg_send 只实现到「接收者+选择子+5 个参数」,type(低 32 位)与 vipgold(高 32 位,恒 0)
/// 合成一个 u64 落到 sp+8/sp+0xc,与分开传逐字节相同。返回是否真的发出了弹框消息。只在按钮回调栈上调用,会改写 r0-r3。
fn show_offline_net_warning_box(env: &mut Environment) -> bool {
    let bundle_cls = env.objc.get_known_class("NSBundle", &mut env.mem);
    let mb_cls = env.objc.get_known_class("MessageBox", &mut env.mem);
    if bundle_cls == nil || mb_cls == nil {
        return false;
    }
    let main_s = sel_named(env, "mainBundle");
    let bundle: id = msg_send(env, (bundle_cls, main_s));
    if bundle == nil {
        return false;
    }
    let key = ns_string::get_static_str(env, "ACTION_CENTER_NETWARNING");
    let empty = ns_string::get_static_str(env, "");
    let loc_s = sel_named(env, "localizedStringForKey:value:table:");
    let msg: id = msg_send(env, (bundle, loc_s, key, empty, nil));
    if msg == nil {
        return false;
    }
    let sh = sel_named(env, "sharedInstance");
    let mb: id = msg_send(env, (mb_cls, sh));
    if mb == nil {
        return false;
    }
    let show = sel_named(env, "showWithTarget:selector:title:message:type:vipgold:");
    let type_and_vipgold: u64 = 6; // 低 32 位 = type 6,高 32 位 = vipgold 0
    let _: () = msg_send(env, (mb, show, nil, SEL::null(), nil, msg, type_and_vipgold));
    true
}

// [2026-09-16] 包3 纯函数单测:旁路档 v=2 校验、每日任务选题映射、客户端日界口径。放在文件最末(clippy items_after_test_module)。
#[cfg(test)]
mod offline_server_tests {
    use super::*;

    #[test]
    fn divine_pool_each_round_sums_to_100() {
        // [2026-10-04 第八轮 R8-C2] generatePresentId 按 0..99 逐件减概率,每轮之和必须正好 100 才一定抽得中。
        for (round, items) in DIVINE_POOL {
            assert_eq!(
                items.iter().map(|x| x.2).sum::<u32>(),
                100,
                "轮次 {}",
                round
            );
            assert!(
                items.iter().all(|x| !(719..=724).contains(&x.0)),
                "轮次 {} 含要联网的编号",
                round
            );
            // 一局只扣 1 张券或 1 贝壳却能连开五轮,奖池里放贝壳就能无限刷贝壳。
            assert!(items.iter().all(|x| x.0 != 704), "轮次 {} 含贝壳", round);
        }
    }

    #[test]
    fn seabed_timestamp_sent_as_unix_seconds() {
        // [2026-10-07 第十一轮 R11-F-2] 客户端会减 978307200:新存的是 Unix 秒原样下发,旧档的 2001 纪元值换算,0 不变。
        let mut st = ActState::default();
        st.shells = vec![(1, 0), (2, 800_000_000), (3, 1_790_000_000), (1, 5), (2, 0)];
        let b = encode_seabed_info(&st);
        let word = |i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        // [1][pearl][5] 之后每个贝壳 3 个字:[序号][类型][时间]
        assert_eq!(word(3 + 2), 0);
        assert_eq!(word(3 + 5), 800_000_000 + CF_EPOCH_UNIX_SECS);
        assert_eq!(word(3 + 8), 1_790_000_000);
        assert_eq!(word(3 + 11), 5);
    }

    #[test]
    fn state_v2_roundtrip_truncation_and_legacy() {
        let st = ActState {
            sign_month: 202_609,
            sign_foot: 12,
            daily_day: 20_260_916,
            daily_vals: vec![3, 1, 2, 0, 2],
            ..ActState::default()
        };
        let text = st.serialize();
        let back = ActState::parse_checked(text.as_bytes()).expect("完整 v=2 档应可读");
        assert_eq!(back.sign_foot, 12);
        assert_eq!(back.daily_day, 20_260_916);
        assert_eq!(back.daily_vals, vec![3, 1, 2, 0, 2]);
        // 截掉末尾 sum 行(最常见的写残)→ 坏档
        let cut = &text[..text.rfind("sum=").unwrap()];
        assert!(ActState::parse_checked(cut.as_bytes()).is_err());
        // 改动正文 → 校验和不匹配
        let tampered = text.replace("sign_foot=12", "sign_foot=99");
        assert!(ActState::parse_checked(tampered.as_bytes()).is_err());
        // v=1 旧档宽松可读;空文件按默认;首行不认识的非空文件算坏档
        let legacy = ActState::parse_checked(b"v=1\nsign_foot=7\n").expect("v=1 旧档应可读");
        assert_eq!(legacy.sign_foot, 7);
        assert!(ActState::parse_checked(b"").is_ok());
        assert!(ActState::parse_checked(b"garbage\n").is_err());
    }

    #[test]
    fn daily_main_one_quest_per_type() {
        for ymd in [20_260_916u32, 20_260_917, 20_261_231, 20_270_101] {
            for level in [1, 3, 5, 10, 52] {
                let vals = pick_daily_main(ymd, level);
                assert!(daily_vals_valid(&vals, false));
                assert_eq!(vals, pick_daily_main(ymd, level), "同一天同等级必须确定");
                let ids = daily_ids(&vals, false);
                assert_eq!(ids.len(), 6);
                for (i, &quest) in ids.iter().enumerate() {
                    let (s, n) = DAILY_MAIN_SLOTS[i];
                    assert!(
                        quest >= s && quest < s + n,
                        "第 {} 条 ID {} 不在类型段内",
                        i,
                        quest
                    );
                }
            }
        }
        // 1 级:type 5 没有够得着的,退回等级要求最低的 26;其余都是 take_level 1 的那条
        assert_eq!(
            daily_ids(&pick_daily_main(20_260_916, 1), false),
            vec![1, 10, 17, 23, 26, 29]
        );
    }

    #[test]
    fn daily_main_offline_visit_is_one() {
        for ymd in 20_261_001u32..20_261_031 {
            for level in [1, 4, 5, 8, 9, 10, 30, 52] {
                let vals = pick_daily_main(ymd, level);
                let ids = daily_ids(&vals, false);
                assert_eq!(ids[5], 29, "第 6 条必须是「拜访 1 个推荐好友」");
                assert!([1, 7, 4].contains(&ids[0]));
                assert!(
                    i32::from(DAILY_MAIN_TAKE_LEVEL[(ids[0] - 1) as usize]) <= level,
                    "第 1 条 ID {} 超出 {} 级",
                    ids[0],
                    level
                );
            }
        }
    }

    #[test]
    fn daily_island_three_slots() {
        for level in [1, 18, 30] {
            let vals = pick_daily_island(20_260_916, level);
            assert!(daily_vals_valid(&vals, true));
            let ids = daily_ids(&vals, true);
            assert_eq!(ids.len(), 3);
            for (i, &quest) in ids.iter().enumerate() {
                let (s, n) = DAILY_HV_SLOTS[i];
                assert!(quest >= s && quest < s + n);
            }
        }
        // 1 级:第 1 段都够不着 → 取等级要求最低的 ID 1;第 2 段最低是 ID 10(9 级)
        let ids = daily_ids(&pick_daily_island(20_260_916, 1), true);
        assert_eq!(&ids[..2], &[1, 10]);
    }

    #[test]
    fn daily_day_key_uses_client_beijing_midnight() {
        // 北京时间 2026-09-16 00:00:00 = unix 1789488000
        let cf = (1_789_488_000i64 - 978_307_200) as u32;
        assert_eq!(daily_day_key(cf), 20_260_916);
        assert_eq!(daily_day_key(cf - 1), 20_260_915);
    }
}
