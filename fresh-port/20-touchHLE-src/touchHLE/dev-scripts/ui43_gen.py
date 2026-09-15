#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
ui43_gen.py —— [MoleWorld 宽屏适配·UI 4:3 虚拟化] UI43 三张名单的离线生成器与自检 [2026-09-16]

生成 src/mole_cheats.rs 里的三张表:
  · UI43_CALLSITES      需要喂 1024x768 的 [CCDirector winSize] 调用点返回地址(LR,已清 Thumb 位);
  · UI43_OFFSET_CLASSES 需要整体右移居中的 UI 根层类名(按字节序,供 binary_search);
  · UI43_CODE_RANGES    白名单类(含子类)全部方法的代码区间 [start,end)(升序、已合并),
                        调用者 LR 落在区间内 = "白名单代码在问",触摸/世界坐标按虚拟世界 ±off 换算。

为什么入库:当初的名单由草稿区脚本生成,草稿区会被清空,之后补类只能手工改地址数组,
容易漏补区间,或者把非白名单函数(main@0xe890)吞进区间。本脚本自包含,只依赖 Python 3 与
capstone(python3 -m pip install capstone),输入是仓库里的 5.5.0 主二进制。

用法(在 touchHLE 目录下执行):
  python3 dev-scripts/ui43_gen.py            生成 + 自检 + 与 mole_cheats.rs 现有三张表逐项比对
  python3 dev-scripts/ui43_gen.py --emit     另把三张 Rust 数组打印到标准输出,可整段替换
  python3 dev-scripts/ui43_gen.py --report   另打印按类统计、布局表候选类等明细
  可选 --bin <MoleWorld 主二进制> --src <mole_cheats.rs>;诊断信息走标准错误。
退出码:0 = 自检全过且与源码一致;1 = 自检失败或与源码不一致;2 = 环境问题(缺 capstone、找不到文件)。

改名单的正确姿势:只改下面「一、手工维护的分类数据」,重跑本脚本,确认自检全过,再用 --emit 的输出
替换 mole_cheats.rs 里对应数组(或按比对结果手工增删),最后再跑一次,比对应显示「完全一致」。

生成规则:
  ① 全二进制反汇编(capstone,按 LC_FUNCTION_STARTS 逐函数线性扫描,寄存器常量跟踪):
     objc_msgSend_stret 的选择子在 r2(r0 = 返回缓冲,r1 = self),objc_msgSend 的在 r1;
     Thumb-2 `blx` 是 4 字节,LR = 指令地址 + 4。
  ② imp → 类.方法 直接遍历 __objc_classlist(含元类)与 __objc_catlist 的 baseMethods,
     ★不能靠 `otool -ov` 文本行大小写猜类名(会把 app delegate 的方法记到 CommonChristmasFatherGiftLayer 名下)。
  ③ 方法结尾用 LC_FUNCTION_STARTS 的下一个函数起点截断,
     ★不能拿"下一个 imp"当结尾:那会把方法之间的非 ObjC 代码(含 main@0xe890)吞进区间;
     宿主发消息时 LR 正是 main 里 `blx _UIApplicationMain` 的返回地址,一旦落在区间内,
     UIKit 控件的触摸坐标也会被错扣 off。
  ④ UI43_CALLSITES = UI_LAYOUT_CLASSES 里全部 winSize(stret)调用点 ∪ AUX_CALLSITES ∪ LEGACY_DEAD_CALLSITES;
     UI43_OFFSET_CLASSES = UI_LAYOUT_CLASSES 去掉子节点/非节点类(NON_ROOT_RE) ∪ LAYOUT_TABLE_CLASSES;
     UI43_CODE_RANGES = 父类链(自身 + 5 层,与运行时 ui43_class_hit 一致)命中
                        「UI_LAYOUT_CLASSES 去掉 NON_ROOT_RE ∪ AUX_RANGE_CLASSES」的类的全部方法。

自检项(任一失败 → 退出码 1):
  · 扫描器盲区:逐函数「从 selref 读出 winSize 的次数」不能多于解析出的调用点数,winSize 选择子不能被
    str/push/stm 写进内存(扫描器只跟踪寄存器,经栈中转就会漏)——下一条的前提是调用点一个没漏;[2026-09-16]
  · 每个发 winSize 的 ObjC 类恰好归入 UI_LAYOUT_CLASSES / KEEP_REAL_WIDTH_CLASSES 之一;
    名单里的类必须真的发 winSize(防过期条目);winSize 调用点不能落在非 ObjC 方法的函数里;
  · AUX_CALLSITES 每项必须是所写方法里的 winSize 调用点,且所属类不在 UI_LAYOUT_CLASSES(否则冗余);
  · LEGACY_DEAD_CALLSITES 每项必须确实【不是】winSize 调用点、在所写方法里、且是 bl/blx 到非 stret 的
    objc_msgSend 桩的返回地址(证明它永远匹配不上 winSize,留着不改变行为);
  · LAYOUT_TABLE_CLASSES 每类:二进制里存在、不发 winSize、至少发一次 getPoint:/getSize:、
    不发 intercept_fast 换算的 7 个触摸/世界坐标选择子(否则它需要进 UI43_CODE_RANGES,不能只补居中名单);
    它们的子类(运行时按父类链同样会被居中)也不能发这 7 个选择子;[2026-09-16]
  · AUX_RANGE_CLASSES 每类在二进制里存在;
  · 白名单方法入口都必须是 LC_FUNCTION_STARTS 里的函数起点;
  · 生成的区间与 mole_cheats.rs 里现有区间:段首是函数起点、段尾是函数起点或 __text 末尾、
    段内非白名单函数起点数为 0、main@0xe890 在区间外;
  · 生成的与源码里的三张表:严格升序(类名按字节序)、无重复、区间不重叠;
  · 生成结果与源码三张表逐项一致(不一致时逐项列出多出/缺少的条目及其归属方法)。
"""

from __future__ import print_function

import argparse
import bisect
import hashlib
import os
import re
import struct
import sys
from collections import defaultdict

# ---------------------------------------------------------------------------
# 一、手工维护的分类数据(改名单只改这里,然后重跑本脚本)
# ---------------------------------------------------------------------------

# 1) 纳入 4:3 的 UI 类(类级):这些类里的 winSize 调用点【全部】进 UI43_CALLSITES。
#    都是多元素复杂布局 UI,不喂设计尺寸就会被 Δ=164pt 拉散(实证:商店网格散架、捉虫结算 "TOTAL" 截断、
#    切水果卡片末项裁切):商店全套、8 类小游戏及其选关/成就面板、各节日活动弹窗、好友/礼物/任务/VIP/兑换等面板。
UI_LAYOUT_CLASSES = frozenset([
    "AcceptFriendsLayer", "AccountBindingLayer", "AchieveSystemLayer", "AchievementItems",
    "AchivementLayer", "ActionCenterControl", "ActionCenterLayer", "ActionCodeLayer",
    "ActionLevelLayer", "ActivityBulletinLayer", "ActivityCaribbeanBasePopLayer",
    "ActivityFlameWarsSelectLayer", "ActivityForecastLayer", "ActivityForecastSecondLayer",
    "ActivityHalloweenBasePopLayer", "ActivityXmasBasePopLayer", "Activity_Alice_BasePopLayer",
    "Activity_FlameWars_BasePopLayer", "Activity_FlameWars_MainLayer",
    "Activity_IceCream_BasePopLayer", "Activity_Shrek_BasePopLayer",
    "Activity_Totoro_BasePopLayer", "AnimalsRecyclerView", "AnniversaryMainLayer",
    "AnniversarySubLayer", "ApartmentView", "ApplyHongKongTourLayer", "AroundTheWorldMainLayer",
    "AutumnMainLayer", "AvatarLayer", "BouquetTradeItem", "BugAchivement", "BugGame",
    "BugLevelBase", "BugLevelChoose", "CafeQuestItem", "CafeShopLayer", "CandyhouseLayer",
    "CaribbeanMainLayer", "ChangeRewardLayer", "ChooseVillageHelp", "ChooseVillageLayer",
    "ChoosingPagesMainLayer", "CommonChristmasFatherGiftLayer", "CouponsItems", "CropInfoView",
    "CrowPriestMessageLayer", "CustomerServiceLayer", "CutFruit", "CutFruitAchivement",
    "CutFruitLevelChoose", "DailyQuestLayer", "DailySignLayer", "DecorateRoomLayer",
    "DiscountInfoLayer", "DivineGame", "DriftBottleMessageLayer", "EasterEggGetRewardLayer",
    "EasterEggMainLayer", "ExchangeCenterLayer", "FinalRewardAnimation", "FirstChargeGiftsLayer",
    "FishingAchivement", "FishingGame", "FishingLevelChoose", "FlowerStudioItem",
    "FlyKiteGetRewardLayer", "FlyKiteIntroductionsLayer", "FlyKiteMainLayer",
    "FriendsViewController", "FruitItem", "FuncIntroLayer", "GameDataCompareLayer",
    "GamePlayGoView", "GetItemRewardFromHaiwangLayer", "GetLastRewardLayer", "GiftAndMessageLayer",
    "GiftLayer", "GiftViewLayer", "GoodsViewLayer", "GreenRiceBallMainLayer", "GreenhouseLayer",
    "GuessWorldCupMainLayer", "HalloweenMainLayer", "HelpLayer", "HouseRecyclerView",
    "IceSummerMainLayer", "InviteFriendsLayer", "JunkShopLayer", "LeaveMessageLayer",
    "LeoAdvanceLayer", "Level1", "Level2", "Level3", "Level4", "LevelChooseLayer", "LevelUpLayer",
    "MagicNumberView", "MessageBox", "MessageBoxGift", "MessageViewController", "MessagesLayer",
    "MinerAchivement", "MinerGame", "MinerLevelChoose", "MiniBase", "MusicHallLayer",
    "NaramGetTodayRewardLayer", "NaramSpringIntroduceLayer", "NaramSpringMainLayer",
    "NewRewardsLayer", "NewSceneLevelUp", "NewSceneQuestLayer", "NewSceneTestLayer",
    "NewStyleStoreItemsView", "NewStyleStoreMainLayer", "NewStyleStoreMenuView",
    "NoticeBoardLayer", "OpenTreasureChestMainLayer", "OptionLayer", "PaintingAchivement",
    "PaintingGame", "PaintingLevelChoose", "PaybackObjectsTableLayer", "PersonalTargetLayer",
    "Plow", "PlowAchivement", "PlowLevelChoose", "PopularItemsPKAdvanceLayer",
    "PopularItemsPKMainLayer", "PopularItemsPKVoteLayer", "PromoteSalesMainLayer",
    "PromoteShowItemsLayer", "QiXiAdvanceLayer", "QuestLayer", "QuestionnaireLayer",
    "ReceiveGiftLayer", "RegisterView", "RequestCodeLayer", "RestaurantView", "RewardLayer",
    "SeabedSeekingTreasureExchageRewardLayer", "SeabedSeekingTreasureMainLayer",
    "SeabedSeekingTreasureRuleLayer", "SealExchangeLayer", "SeekViewController", "ShopItemsLayer",
    "ShoppingView", "ShowActivityRuleLayer", "ShowFreeShellsLayer", "ShowMoreFriendsLayer",
    "ShowRuleLayer", "SpringPoemGetRewardLayer", "SpringPoemIntroduceLayer", "SpringPoemMainLayer",
    "SpringPoemPageLayer", "TeamTargetLayer", "TestLayer", "TourLineLayer", "TreasureHuntPopLayer",
    "TreasureRewardLayer", "VIPFunctionsLayer", "VIPItems", "VIPLayer", "VerifyInviteCodeLayer",
    "WashRoomAchievement", "WashRoomGame", "WashRoomLevelChoose", "WaterTowerRewardView",
    "XmasMainLayer",
])

# 2) 保持真实宽度的类(类级):这些类的 winSize 调用点一律不进名单。
#    分组只是注释,方便阅读;"其它"组按类名归组,没有逐个复核用途,改归属前请先反汇编确认。
KEEP_REAL_WIDTH_CLASSES = frozenset([
    # 世界场景、相机与世界内建筑视图:必须拿真实宽,Hor+ 才能显示更多海洋、相机边界才对
    "CameraLayer", "FriendsVillageLayer", "InGameLayer", "MoveLayer", "NewSceneMoveLayer",
    "VillageLayer", "BuildingView", "ChrismasTreeView", "CropViewLayer", "DiscoveryShipView",
    "FlowerCropViewLayer", "FruitCropViewLayer", "SuperShellTreeView", "WaterTowerView",
    "ButterFlyLayer", "ButterFlyObject",
    # 贴边 HUD 与菜单条:必须拿真实宽才贴得住屏幕边
    "TopMenuLayer", "VillageMenuLayer", "NewSceneVillageMenuLayer",
    # 全屏画面(启动、加载、主菜单):已按宽版底图处理,不动
    "HelloWorldLayer", "LogoLayer", "TaomeeLogoLayer", "MainMenu", "MainMenuScene",
    "LoadingLayer", "LoadingNewLayer", "LoadingScene", "CommonLoadingLayer",
    "iMoleVillageAppDelegate",
    # 世界内移动对象与飘字
    "ActorManager", "Porter", "NewScenePorter", "GoldSprite", "XPSprite", "BuildValueSprite",
    "PopularityValueSprite", "MovableIcon", "RewardTicketsIcon",
    # 天气粒子与全屏特效
    "CCParticalFirefly", "ParticalManager", "TMEffectLayer", "TMFireFlyLayer", "TMRainLayer",
    "TMSnowLayer", "WipeSnowLayer", "WipeWaterLayer", "FireworkLayer", "LevelUpEffectLayer",
    # 其它:管理器、广告与弹出中心、截图等
    "GameManager", "NewGameManager", "WrapperManager", "VipQuest", "Screenshot",
    "AdViewForMoleCart", "ShowAdwallBoardLayer", "AutoPopZhongXinLayer",
    "OnTouchPopZhongXinLayer",
    # cocos2d 内部
    "CCDirectorIOS", "CCFollow", "CCLayer", "CCLayerColor", "CCMenu", "CCScene", "CCTableView",
    "CCVideoPlayerImpliOS", "CCParticleExplosion", "CCParticleFire", "CCParticleFireworks",
    "CCParticleFlower", "CCParticleGalaxy", "CCParticleMeteor", "CCParticleRain",
    "CCParticleSmoke", "CCParticleSnow", "CCParticleSpiral", "CCParticleSun",
    "CCTransitionCrossFade", "CCTransitionFadeTR", "CCTransitionJumpZoom", "CCTransitionMoveInB",
    "CCTransitionMoveInL", "CCTransitionMoveInR", "CCTransitionMoveInT", "CCTransitionPageTurn",
    "CCTransitionRadialCCW", "CCTransitionSlideInB", "CCTransitionSlideInL",
    "CCTransitionSlideInR", "CCTransitionSlideInT", "CCTransitionTurnOffTiles",
    # 白名单小游戏的子对象:类级保持真实宽度,个别调用点另见 AUX_CALLSITES
    "BugObject", "FishObject", "Fruit", "WashRoomActor",
])

# 3) UI_LAYOUT_CLASSES 里不当"根层"的类:子节点(条目/单元格/精灵/对象)或非节点(控制器/管理器)。
#    它们随所在根层整体平移,自己再右移就会偏 2 倍;它们的方法也不进 UI43_CODE_RANGES。
NON_ROOT_RE = re.compile(r"(Item|Items|Cell|Sprite|Object|Control|Manager)$")

# 4) 布局表类:不调 winSize、全部坐标来自 [ResourceManager getPoint:/getSize:] 的 1024 设计布局表,
#    按 winSize 调用点生成的名单天然漏掉 → 宽屏下贴左不居中。只补进 UI43_OFFSET_CLASSES,
#    不进 UI43_CODE_RANGES(自检保证它们不发触摸/世界坐标换算选择子;发了就必须另议)。
LAYOUT_TABLE_CLASSES = frozenset([
    # [2026-09-16] 共用任务框布局表(quest_box 等,npcdialogback.png 底图)的弹框(9926083)
    "WiltWarningLayer", "HelpQuestLayer", "TimeQuestLayer", "VipQuestLayer", "OscarDialogueLayer",
    # [2026-09-16] 剧情对话层,按 getPoint:@"story_*" 摆放(310eeee)
    "StoryLayer", "TimeStoryLayer", "VipStoryLayer", "NewSceneStoryLayer",
])

# 5) 辅助区间类:不在居中名单里、但只在白名单小游戏里用、自己读触摸坐标的辅助类,
#    全部方法并入 UI43_CODE_RANGES(当作白名单代码),不进居中名单。(310eeee)
AUX_RANGE_CLASSES = frozenset([
    "TouchTrailLayer",   # CutFruit 把触摸转发给它,0x143c30/0x143e58 调 locationInView: 比对水果虚拟坐标
    "BackgroundSprite",  # 捉虫 Level1-4:ccTouchEnded:withEvent: 0x17d9b0 取 locationInView: 摆拍打特效
])

# 6) 辅助调用点:类级保持真实宽度、但个别方法按所在白名单小游戏的 1024 虚拟坐标算方向/边界/出生点。
#    (LR, 类, 实例方法选择子)。四个类都只由白名单小游戏创建。(310eeee)
AUX_CALLSITES = (
    (0x147280, "Fruit", "initWithType:type:parentNode:initPos:maxTime:minTime:"),
    (0x1a3f84, "FishObject", "setFishPosition:isLeft:"),
    (0x1af8c2, "BugObject", "initwithFile:"),
    (0x35e6ac, "WashRoomActor", "initWithIndex:type:parentNode:pathType:"),
)

# 7) 历史死条目:已验收的 UI43_CALLSITES 里有、但并不是 winSize 调用点的地址。[2026-09-16]
#    0x1fffd6 在 -[NoticeBoardLayer showWithTarget:selector:] 里,是 0x1fffd2 `blx _objc_msgSend`
#    ([CCDirector sharedDirector])的返回地址;同一方法真正的两处 winSize 是 0x1fffb6 与 0x1fffec,都按规则生成。
#    intercept 只在 sel == "winSize" 时查这张表,而 winSize 返回 CGSize 必走 _stret,这个 LR 永远匹配不上,
#    留着不改变任何行为;当初统计的「170 类 240 处」里就含它,怎么混进来的未能复现。
#    为保证与已验收数组逐项一致暂时保留;以后要删,把这里和 mole_cheats.rs 里的地址一起删,再重跑本脚本。
LEGACY_DEAD_CALLSITES = (
    (0x1fffd6, "NoticeBoardLayer", "showWithTarget:selector:"),
)

# 已知必须落在区间外的非 ObjC 函数(见文件头 ③)。
KNOWN_OUTSIDE = ((0xe890, "main"),)

# intercept_fast 里按调用者 LR 换算的触摸/世界坐标选择子(与 mole_cheats.rs 的 Ui43FastSels / ui43_fast_sels
# 里 loc_in_view..to_ui 这 7 个保持一致;Ui43Sels 是居中用的 position/anchorPoint 等,不是这张表)。[2026-09-16]
TOUCH_CONV_SELS = (
    "locationInView:", "previousLocationInView:", "convertToWorldSpace:",
    "convertToWorldSpaceAR:", "convertToNodeSpace:", "convertToNodeSpaceAR:", "convertToUI:",
)
LAYOUT_SELS = ("getPoint:", "getSize:")
WANT_SELS = frozenset(("winSize",) + TOUCH_CONV_SELS + LAYOUT_SELS)

# 运行时 ui43_class_hit 查父类链的层数(自身 + 5 层父类)。
CHAIN_DEPTH = 6

# ---------------------------------------------------------------------------
# 二、路径与常量
# ---------------------------------------------------------------------------

TOOL_DIR = os.path.dirname(os.path.abspath(__file__))
TOUCHHLE_DIR = os.path.dirname(TOOL_DIR)
DEFAULT_SRC = os.path.join(TOUCHHLE_DIR, "src", "mole_cheats.rs")
DEFAULT_BIN = os.path.normpath(os.path.join(
    TOUCHHLE_DIR, os.pardir, os.pardir, "01-cracked", "Payload", "MoleWorld.app", "MoleWorld"))
# 生成名单时所用主二进制(5.5.0 香草脱壳版)的 SHA-1;不一致时地址很可能全部对不上。
EXPECTED_SHA1 = "e28cb6e1f1feeb35f3cf84ee0b81cc11314a2f86"

LC_SEGMENT = 0x1
LC_SYMTAB = 0x2
LC_DYSYMTAB = 0xB
LC_FUNCTION_STARTS = 0x26
S_SYMBOL_STUBS = 0x8
INDIRECT_SYMBOL_LOCAL = 0x80000000
INDIRECT_SYMBOL_ABS = 0x40000000

# 选择子所在寄存器:_stret 变体 r0 是返回缓冲,选择子顺延到 r2。
MSGSEND_SEL_REG = {
    "_objc_msgSend": "r1",
    "_objc_msgSendSuper2": "r1",
    "_objc_msgSend_stret": "r2",
    "_objc_msgSendSuper2_stret": "r2",
}


def log(msg=""):
    print(msg, file=sys.stderr)


# ---------------------------------------------------------------------------
# 三、Mach-O(32 位 ARM)解析
# ---------------------------------------------------------------------------

class MachO(object):
    def __init__(self, path):
        raw = open(path, "rb").read()
        self.sha1 = hashlib.sha1(raw).hexdigest()
        data = raw
        if struct.unpack_from(">I", raw, 0)[0] == 0xCAFEBABE:
            nfat = struct.unpack_from(">I", raw, 4)[0]
            pick = None
            for i in range(nfat):
                cputype, cpusub, off, size, _align = struct.unpack_from(">5I", raw, 8 + 20 * i)
                if cputype == 12 and (pick is None or cpusub == 9):
                    pick = (off, size)
            if pick is None:
                raise ValueError("胖二进制里没有 ARM 切片")
            data = raw[pick[0]:pick[0] + pick[1]]
        if struct.unpack_from("<I", data, 0)[0] != 0xFEEDFACE:
            raise ValueError("不是 32 位小端 Mach-O")
        if struct.unpack_from("<i", data, 4)[0] != 12:
            raise ValueError("不是 ARM 二进制")
        self.data = data
        self.segments = []
        self.sections = []
        self.symtab = None
        self.dysymtab = None
        self.fstarts_cmd = None
        ncmds = struct.unpack_from("<I", data, 16)[0]
        pos = 28
        for _ in range(ncmds):
            cmd, cmdsize = struct.unpack_from("<II", data, pos)
            if cmd == LC_SEGMENT:
                segname = data[pos + 8:pos + 24].split(b"\0", 1)[0].decode()
                vmaddr, vmsize, fileoff, filesize, _mp, _ip, nsects, _fl = struct.unpack_from(
                    "<8I", data, pos + 24)
                self.segments.append((segname, vmaddr, vmsize, fileoff, filesize))
                sp = pos + 56
                for _ in range(nsects):
                    sectname = data[sp:sp + 16].split(b"\0", 1)[0].decode()
                    sseg = data[sp + 16:sp + 32].split(b"\0", 1)[0].decode()
                    addr, size, offset, _al, _ro, _nr, sflags, res1, res2 = struct.unpack_from(
                        "<9I", data, sp + 32)
                    self.sections.append({
                        "sect": sectname, "seg": sseg, "addr": addr, "size": size,
                        "offset": offset, "flags": sflags, "reserved1": res1, "reserved2": res2,
                    })
                    sp += 68
            elif cmd == LC_SYMTAB:
                self.symtab = struct.unpack_from("<4I", data, pos + 8)
            elif cmd == LC_DYSYMTAB:
                self.dysymtab = struct.unpack_from("<18I", data, pos + 8)
            elif cmd == LC_FUNCTION_STARTS:
                self.fstarts_cmd = struct.unpack_from("<2I", data, pos + 8)
            pos += cmdsize

    def section(self, sect, seg=None):
        for s in self.sections:
            if s["sect"] == sect and (seg is None or s["seg"] == seg):
                return s
        return None

    def vread(self, addr, n):
        if addr is None:
            return None
        for _name, vmaddr, _vmsize, fileoff, filesize in self.segments:
            if vmaddr <= addr < vmaddr + filesize:
                off = fileoff + addr - vmaddr
                return self.data[off:off + min(n, vmaddr + filesize - addr)]
        return None

    def read32(self, addr):
        b = self.vread(addr, 4)
        return struct.unpack("<I", b)[0] if b is not None and len(b) == 4 else None

    def cstr(self, addr, limit=1024):
        if not addr:
            return None
        b = self.vread(addr, limit)
        if not b:
            return None
        i = b.find(b"\0")
        return b[:i if i >= 0 else len(b)].decode("utf-8", "replace")

    def stub_names(self):
        """符号桩地址 → 导入符号名(经间接符号表)。"""
        out = {}
        if self.symtab is None or self.dysymtab is None:
            return out
        symoff, nsyms, stroff, _strsize = self.symtab
        indoff, nind = self.dysymtab[12], self.dysymtab[13]
        for s in self.sections:
            if (s["flags"] & 0xFF) != S_SYMBOL_STUBS or not s["reserved2"]:
                continue
            for i in range(s["size"] // s["reserved2"]):
                k = s["reserved1"] + i
                if k >= nind:
                    break
                symidx = struct.unpack_from("<I", self.data, indoff + 4 * k)[0]
                if symidx & (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS) or symidx >= nsyms:
                    continue
                strx = struct.unpack_from("<I", self.data, symoff + 12 * symidx)[0]
                end = self.data.find(b"\0", stroff + strx)
                out[s["addr"] + i * s["reserved2"]] = self.data[stroff + strx:end].decode(
                    "utf-8", "replace")
        return out

    def function_starts(self):
        """LC_FUNCTION_STARTS → (升序函数起点列表(已清 Thumb 位), {起点: 是否 Thumb})。"""
        if self.fstarts_cmd is None:
            raise ValueError("二进制没有 LC_FUNCTION_STARTS")
        dataoff, datasize = self.fstarts_cmd
        blob = self.data[dataoff:dataoff + datasize]
        text_seg = [s for s in self.segments if s[0] == "__TEXT"]
        if not text_seg:
            raise ValueError("找不到 __TEXT 段")
        addr = text_seg[0][1]
        thumb = {}
        i = 0
        while i < len(blob):
            v = 0
            sh = 0
            while True:
                c = blob[i]
                i += 1
                v |= (c & 0x7F) << sh
                sh += 7
                if not c & 0x80:
                    break
            if v == 0:
                break
            # ld64 编码时 Thumb 函数地址带 bit0,增量按带位的地址累加,输出时再清位。
            addr += v
            thumb[addr & ~1] = bool(addr & 1)
        return sorted(thumb), thumb


# ---------------------------------------------------------------------------
# 四、ObjC 元数据:imp → 类.方法、父类表、selref 表
# ---------------------------------------------------------------------------

class Method(object):
    __slots__ = ("imp", "cls", "meta", "sel", "cat")

    def __init__(self, imp, cls, meta, sel, cat):
        self.imp = imp
        self.cls = cls
        self.meta = meta
        self.sel = sel
        self.cat = cat

    def desc(self):
        c = self.cls if self.cls is not None else "?"
        if self.cat:
            c = "%s(%s)" % (c, self.cat)
        return "%s[%s %s]" % ("+" if self.meta else "-", c, self.sel)


def read_method_list(mo, ml):
    out = []
    if not ml:
        return out
    head = mo.read32(ml)
    count = mo.read32(ml + 4)
    if head is None or count is None:
        return out
    entsize = head & ~3
    if entsize < 12 or count > 100000:
        return out
    for j in range(count):
        e = ml + 8 + j * entsize
        sel = mo.cstr(mo.read32(e))
        imp = mo.read32(e + 8)
        if sel is None or not imp:
            continue
        out.append((sel, imp))
    return out


def parse_objc(mo):
    """返回 (类名集合, 父类表{类: 父类或 None}, 方法列表[Method], selref 表{地址: 选择子})。"""
    cls_names = {}
    class_ptrs = []
    cl = mo.section("__objc_classlist")
    if cl is None:
        raise ValueError("找不到 __objc_classlist")
    for i in range(cl["size"] // 4):
        p = mo.read32(cl["addr"] + 4 * i)
        if p:
            class_ptrs.append(p)

    def ro_of(cls_ptr):
        d = mo.read32(cls_ptr + 16) if cls_ptr else None
        return (d & ~3) if d else None

    for p in class_ptrs:
        ro = ro_of(p)
        name = mo.cstr(mo.read32(ro + 16)) if ro else None
        if name:
            cls_names[p] = name

    supers = {}
    methods = []
    for p in class_ptrs:
        name = cls_names.get(p)
        if not name:
            continue
        # 外部父类(NSObject/UIViewController 等由 dyld 绑定,文件里是 0)记 None,父类链到此为止。
        supers[name] = cls_names.get(mo.read32(p + 4))
        ro = ro_of(p)
        for sel, imp in read_method_list(mo, mo.read32(ro + 20)):
            methods.append(Method(imp & ~1, name, False, sel, None))
        mro = ro_of(mo.read32(p))
        if mro:
            for sel, imp in read_method_list(mo, mo.read32(mro + 20)):
                methods.append(Method(imp & ~1, name, True, sel, None))

    cat = mo.section("__objc_catlist")
    if cat is not None:
        for i in range(cat["size"] // 4):
            c = mo.read32(cat["addr"] + 4 * i)
            if not c:
                continue
            cat_name = mo.cstr(mo.read32(c)) or "?"
            cls_name = cls_names.get(mo.read32(c + 4))  # 外部类的分类记 None
            for off, meta in ((8, False), (12, True)):
                for sel, imp in read_method_list(mo, mo.read32(c + off)):
                    methods.append(Method(imp & ~1, cls_name, meta, sel, cat_name))

    selrefs = {}
    sr = mo.section("__objc_selrefs")
    if sr is not None:
        for i in range(sr["size"] // 4):
            a = sr["addr"] + 4 * i
            s = mo.cstr(mo.read32(a))
            if s:
                selrefs[a] = s
    return set(cls_names.values()), supers, methods, selrefs


# ---------------------------------------------------------------------------
# 五、反汇编扫描:找 objc_msgSend 家族的调用点并解析选择子
# ---------------------------------------------------------------------------

REG_ALIAS = {"sb": "r9", "sl": "r10", "fp": "r11", "ip": "r12"}
GPRS = frozenset(["r%d" % i for i in range(13)] + ["sp", "lr", "pc"])
COND_BRANCHES = frozenset("b" + c for c in (
    "eq", "ne", "cs", "hs", "cc", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le"))
CALL_CLOBBER = ("r0", "r1", "r2", "r3", "r12", "lr")
TWO_DEST = frozenset(("umull", "smull", "umlal", "smlal", "ldrd", "vmov", "ldrexd"))
LDR_RE = re.compile(r"^(\w+), \[(\w+)(?:, #(-?(?:0x[0-9a-f]+|\d+)))?\](!?)$")
POSTIDX_RE = re.compile(r"\[(\w+)\], ")


def reg_of(tok):
    t = tok.strip()
    t = REG_ALIAS.get(t, t)
    return t if t in GPRS else None


def imm_of(tok):
    t = tok.strip()
    if not t.startswith("#"):
        return None
    try:
        return int(t[1:], 0)
    except ValueError:
        return None


def no_write(m):
    """第一个操作数不是被写寄存器的指令(比较、存储、分支、IT 等)。"""
    return (m in ("cmp", "cmn", "tst", "teq", "cbz", "cbnz", "nop", "svc", "bkpt", "udf",
                  "pld", "pli", "vstr")
            or m in COND_BRANCHES
            or (m.startswith("str") and not m.startswith("strex"))
            or m.startswith("push") or m.startswith("stm") or m.startswith("vpush")
            or m.startswith("vstm")
            or (m.startswith("it") and len(m) <= 5 and all(ch in "te" for ch in m[2:])))


def scan_msgsends(mo, starts, thumb, stubs, selrefs, text_lo, text_hi, probe=None):
    """逐函数线性反汇编,返回 [(LR, 函数起点, 桩名, 选择子, 种类)];种类 call = bl/blx,tail = 尾调用 b。

    probe 给 dict 时另记两类扫描器盲区证据(自检用)[2026-09-16]:
      probe["ws_loads"]:每次从 selref 读出 winSize 选择子的 (指令地址, 函数起点);
      probe["ws_stash"]:持有 winSize 选择子的寄存器被 str/push/stm 写进内存的 (指令地址, 函数起点, 指令)。
    本扫描器只跟踪寄存器,选择子一旦先存栈再取出来用就解析不到(全二进制约 1900 处 _stret 调用是这种情况),
    所以要靠这两项证明 winSize 没有漏:加载次数要与解析出的调用点数逐函数对上,且从不写进内存。
    """
    import capstone
    md_t = capstone.Cs(capstone.CS_ARCH_ARM, capstone.CS_MODE_THUMB)
    md_a = capstone.Cs(capstone.CS_ARCH_ARM, capstone.CS_MODE_ARM)
    md_t.skipdata = True
    md_a.skipdata = True
    sites = []
    bounds = [s for s in starts if text_lo <= s < text_hi]
    if not bounds or bounds[0] != text_lo:
        bounds.insert(0, text_lo)
    for idx, fs in enumerate(bounds):
        fe = bounds[idx + 1] if idx + 1 < len(bounds) else text_hi
        is_thumb = thumb.get(fs, True)
        code = mo.vread(fs, fe - fs)
        if not code:
            continue
        pcd = 4 if is_thumb else 8
        md = md_t if is_thumb else md_a
        regs = {}
        for addr, size, mn, ops in md.disasm_lite(code, fs):
            m = mn[:-2] if mn.endswith(".w") else mn
            if m.startswith("."):  # skipdata 数据字节:之后的状态不可信
                regs.clear()
                continue
            if m in ("movw", "mov", "movs"):
                opl = ops.split(", ")
                rd = reg_of(opl[0])
                if rd is None:
                    continue
                if rd == "pc":
                    regs.clear()
                    continue
                v = imm_of(opl[1]) if len(opl) == 2 else None
                if v is not None:
                    regs[rd] = ("c", v & (0xFFFF if m == "movw" else 0xFFFFFFFF))
                elif len(opl) == 2 and reg_of(opl[1]) in regs:
                    regs[rd] = regs[reg_of(opl[1])]
                else:
                    regs.pop(rd, None)
                continue
            if m == "movt":
                opl = ops.split(", ")
                rd = reg_of(opl[0])
                if rd is None:
                    continue
                cur = regs.get(rd)
                v = imm_of(opl[1]) if len(opl) == 2 else None
                if cur is not None and cur[0] == "c" and v is not None:
                    regs[rd] = ("c", (cur[1] & 0xFFFF) | ((v & 0xFFFF) << 16))
                else:
                    regs.pop(rd, None)
                continue
            if m in ("add", "adds", "addw"):
                opl = ops.split(", ")
                rd = reg_of(opl[0])
                if rd is None:
                    continue
                if rd == "pc":
                    regs.clear()
                    continue
                if len(opl) == 2:
                    x, y = opl[0], opl[1]
                elif len(opl) == 3:
                    x, y = opl[1], opl[2]
                else:
                    regs.pop(rd, None)
                    continue
                vals = []
                for tok in (x, y):
                    r = reg_of(tok)
                    if r == "pc":
                        # 寄存器形式 add rd, pc 读到的 PC 不对齐;add rd, pc, #imm(ADR)按 4 对齐。
                        base = addr + pcd
                        vals.append((base & ~3) if imm_of(y) is not None else base)
                    elif r is not None:
                        cv = regs.get(r)
                        vals.append(cv[1] if cv is not None and cv[0] == "c" else None)
                    else:
                        vals.append(imm_of(tok))
                if vals[0] is None or vals[1] is None:
                    regs.pop(rd, None)
                else:
                    regs[rd] = ("c", (vals[0] + vals[1]) & 0xFFFFFFFF)
                continue
            if m == "adr":
                opl = ops.split(", ")
                rd = reg_of(opl[0])
                v = imm_of(opl[1]) if len(opl) == 2 else None
                if rd is not None:
                    if v is None:
                        regs.pop(rd, None)
                    else:
                        regs[rd] = ("c", (((addr + pcd) & ~3) + v) & 0xFFFFFFFF)
                continue
            if m == "ldr":
                mt = LDR_RE.match(ops)
                if mt:
                    rt = reg_of(mt.group(1))
                    base = reg_of(mt.group(2))
                    off = int(mt.group(3), 0) if mt.group(3) else 0
                    if base == "pc":
                        ea = (((addr + pcd) & ~3) + off) & 0xFFFFFFFF
                    else:
                        bv = regs.get(base)
                        ea = ((bv[1] + off) & 0xFFFFFFFF) if bv is not None and bv[0] == "c" else None
                    if mt.group(4) and base is not None:
                        regs.pop(base, None)
                    if rt is None:
                        continue
                    if rt == "pc":
                        regs.clear()
                        continue
                    if ea is None:
                        regs.pop(rt, None)
                    elif ea in selrefs:
                        regs[rt] = ("s", selrefs[ea])
                        if probe is not None and selrefs[ea] == "winSize":
                            probe.setdefault("ws_loads", []).append((addr, fs))
                    else:
                        v = mo.read32(ea)
                        if v is None:
                            regs.pop(rt, None)
                        else:
                            regs[rt] = ("c", v)
                else:
                    rt = reg_of(ops.split(",", 1)[0])
                    if rt == "pc":
                        regs.clear()
                    elif rt is not None:
                        regs.pop(rt, None)
                    mb = POSTIDX_RE.search(ops)
                    if mb and reg_of(mb.group(1)):
                        regs.pop(reg_of(mb.group(1)), None)
                continue
            if m in ("bl", "blx", "b"):
                target = imm_of(ops)
                if target is None and m != "b":
                    cv = regs.get(reg_of(ops))
                    if cv is not None and cv[0] == "c":
                        target = cv[1] & ~1
                name = stubs.get(target) if target is not None else None
                selreg = MSGSEND_SEL_REG.get(name)
                if selreg is not None:
                    sv = regs.get(selreg)
                    if sv is not None and sv[0] == "s" and sv[1] in WANT_SELS:
                        sites.append((addr + size, fs, name, sv[1], "tail" if m == "b" else "call"))
                if m == "b":
                    regs.clear()
                else:
                    for r in CALL_CLOBBER:
                        regs.pop(r, None)
                continue
            if m in ("bx", "tbb", "tbh"):
                regs.clear()
                continue
            if m.startswith("pop") or m.startswith("ldm"):
                lb = ops.find("{")
                rb = ops.find("}")
                listed = [reg_of(t) for t in ops[lb + 1:rb].split(",")] if lb >= 0 and rb > lb else []
                if "pc" in listed:
                    regs.clear()
                else:
                    for r in listed:
                        if r is not None:
                            regs.pop(r, None)
                continue
            if probe is not None and (m.startswith("str") or m.startswith("push") or m.startswith("stm")):
                held = [r for r, v in regs.items() if v[0] == "s" and v[1] == "winSize"]
                if held and any(reg_of(tok) in held for tok in re.split(r"[,{}\[\]!]", ops)):
                    probe.setdefault("ws_stash", []).append((addr, fs, "%s %s" % (mn, ops)))
            if no_write(m):
                continue
            opl = ops.split(",")
            r = reg_of(opl[0])
            if r == "pc":
                regs.clear()
                continue
            if r is not None:
                regs.pop(r, None)
            if m in TWO_DEST and len(opl) > 1:
                r2 = reg_of(opl[1])
                if r2 is not None:
                    regs.pop(r2, None)
    return sites


# ---------------------------------------------------------------------------
# 六、mole_cheats.rs 三张表的读取与输出
# ---------------------------------------------------------------------------

def parse_src_tables(path):
    text = open(path, encoding="utf-8").read()

    def body(name):
        mt = re.search(r"const " + re.escape(name) + r"\s*:[^=]*=\s*&\[(.*?)\n\];", text, re.S)
        if not mt:
            raise ValueError("mole_cheats.rs 里找不到 const %s" % name)
        return "\n".join(line.split("//", 1)[0] for line in mt.group(1).splitlines())

    cs = [int(x, 16) for x in re.findall(r"0x[0-9a-fA-F]+", body("UI43_CALLSITES"))]
    oc = re.findall(r'"([^"]*)"', body("UI43_OFFSET_CLASSES"))
    rg = [(int(a, 16), int(b, 16)) for a, b in re.findall(
        r"\(\s*(0x[0-9a-fA-F]+)\s*,\s*(0x[0-9a-fA-F]+)\s*\)", body("UI43_CODE_RANGES"))]
    return cs, oc, rg


def emit_rust(callsites, classes, ranges):
    out = ["const UI43_CALLSITES: &[u32] = &["]
    for i in range(0, len(callsites), 8):
        out.append("    " + ", ".join("0x%x" % a for a in callsites[i:i + 8]) + ",")
    out.append("];")
    out.append("")
    out.append("const UI43_OFFSET_CLASSES: &[&str] = &[")
    for i in range(0, len(classes), 4):
        out.append("    " + ", ".join('"%s"' % c for c in classes[i:i + 4]) + ",")
    out.append("];")
    out.append("")
    out.append("const UI43_CODE_RANGES: &[(u32, u32)] = &[")
    for i in range(0, len(ranges), 4):
        out.append("    " + ", ".join("(0x%x, 0x%x)" % r for r in ranges[i:i + 4]) + ",")
    out.append("];")
    return "\n".join(out)


def order_problems(tag, cs, oc, rg):
    probs = []
    for a, b in zip(cs, cs[1:]):
        if not a < b:
            probs.append("%s UI43_CALLSITES 不是严格升序(或有重复):0x%x 后面是 0x%x" % (tag, a, b))
    for a, b in zip(oc, oc[1:]):
        if not a.encode() < b.encode():
            probs.append("%s UI43_OFFSET_CLASSES 不是字节序严格升序(或有重复):%s 后面是 %s" % (tag, a, b))
    for s, e in rg:
        if not s < e:
            probs.append("%s UI43_CODE_RANGES 空段或反段:(0x%x, 0x%x)" % (tag, s, e))
    for (s1, e1), (s2, e2) in zip(rg, rg[1:]):
        if not (s1 < s2 and e1 <= s2):
            probs.append("%s UI43_CODE_RANGES 乱序或重叠:(0x%x, 0x%x) 与 (0x%x, 0x%x)" % (tag, s1, e1, s2, e2))
    return probs


# ---------------------------------------------------------------------------
# 七、主流程
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="UI43 三张名单离线生成器与自检(说明见文件头)")
    ap.add_argument("--bin", default=DEFAULT_BIN, help="MoleWorld 主二进制路径")
    ap.add_argument("--src", default=DEFAULT_SRC, help="mole_cheats.rs 路径(比对用)")
    ap.add_argument("--emit", action="store_true", help="把生成的三张 Rust 数组打印到标准输出")
    ap.add_argument("--report", action="store_true", help="打印按类统计与布局表候选类明细")
    ap.add_argument("--no-src", action="store_true", help="不读 mole_cheats.rs,只生成与自检")
    args = ap.parse_args()

    try:
        import capstone  # noqa: F401
    except ImportError:
        log("缺少 capstone:python3 -m pip install capstone")
        return 2
    if not os.path.isfile(args.bin):
        log("找不到主二进制:%s(用 --bin 指定)" % args.bin)
        return 2
    if not args.no_src and not os.path.isfile(args.src):
        log("找不到 mole_cheats.rs:%s(用 --src 指定,或加 --no-src)" % args.src)
        return 2

    errors = []
    warns = []

    mo = MachO(args.bin)
    if mo.sha1 != EXPECTED_SHA1:
        warns.append("主二进制 SHA-1 = %s,与生成名单时的 %s 不同,地址可能全部对不上" % (mo.sha1, EXPECTED_SHA1))
    text = mo.section("__text", "__TEXT")
    if text is None:
        log("找不到 __TEXT,__text")
        return 2
    text_lo, text_hi = text["addr"], text["addr"] + text["size"]
    starts, thumb = mo.function_starts()
    start_set = set(starts)
    stubs = mo.stub_names()
    if "_objc_msgSend_stret" not in stubs.values():
        errors.append("符号桩里找不到 _objc_msgSend_stret")
    all_classes, supers, methods, selrefs = parse_objc(mo)

    imp_owner = defaultdict(list)
    for mth in methods:
        imp_owner[mth.imp].append(mth)

    def func_of(addr):
        i = bisect.bisect_right(starts, addr) - 1
        return starts[i] if i >= 0 else None

    def owners_desc(fs):
        owners = imp_owner.get(fs)
        if not owners:
            return "非 ObjC 方法函数 0x%x" % fs
        return " / ".join(o.desc() for o in owners)

    def lr_desc(lr):
        fs = func_of(lr - 2)
        return owners_desc(fs) if fs is not None else "?"

    log("扫描 %s(%d 个函数起点,%d 个 ObjC 方法)……" % (args.bin, len(starts), len(methods)))
    probe = {}
    sites = scan_msgsends(mo, starts, thumb, stubs, selrefs, text_lo, text_hi, probe)

    # --- winSize 调用点归类 ---
    ws_all = [s for s in sites if s[3] == "winSize"]

    # [2026-09-16] 扫描器盲区自检:本扫描器只跟踪寄存器,选择子经栈中转就解析不到,
    # 下面「每个发 winSize 的类恰好归入一张表」的前提是 winSize 调用点一个没漏。
    # 逐函数比对「从 selref 读出 winSize 的次数」与「解析出的 winSize 调用点数」,并要求它从不被写进内存。
    for addr, fs, insn in probe.get("ws_stash", []):
        errors.append("winSize 选择子被写进内存(0x%x %s,%s),扫描器跟不到后续取用,可能漏调用点" % (
            addr, insn, owners_desc(fs)))
    ws_load_cnt = defaultdict(int)
    for _addr, fs in probe.get("ws_loads", []):
        ws_load_cnt[fs] += 1
    ws_site_cnt = defaultdict(int)
    for s in ws_all:
        ws_site_cnt[s[1]] += 1
    for fs in sorted(ws_load_cnt):
        if ws_load_cnt[fs] > ws_site_cnt[fs]:
            errors.append("函数 0x%x 读出 winSize 选择子 %d 次、只解析到 %d 处调用点,可能漏调用点:%s" % (
                fs, ws_load_cnt[fs], ws_site_cnt[fs], owners_desc(fs)))
    ws_stret = [s for s in ws_all if s[2].endswith("_stret") and s[4] == "call"]
    for s in ws_all:
        if s not in ws_stret:
            warns.append("winSize 以 %s 发送(%s),LR 无法定位或返回约定不同,未计入:0x%x %s" % (
                s[2], "尾调用" if s[4] == "tail" else "非 stret", s[0], owners_desc(s[1])))
    ws_by_class = defaultdict(list)
    ws_by_lr = {}
    for s in ws_stret:
        ws_by_lr[s[0]] = s
        owners = imp_owner.get(s[1])
        if not owners:
            errors.append("winSize 调用点落在非 ObjC 方法的函数里,无法按类归属:LR 0x%x(函数 0x%x)" % (s[0], s[1]))
            continue
        for c in set(o.cls for o in owners):
            ws_by_class[c].append(s)

    both = UI_LAYOUT_CLASSES & KEEP_REAL_WIDTH_CLASSES
    for c in sorted(both):
        errors.append("类同时在 UI_LAYOUT_CLASSES 与 KEEP_REAL_WIDTH_CLASSES:%s" % c)
    for c in sorted(set(ws_by_class) - UI_LAYOUT_CLASSES - KEEP_REAL_WIDTH_CLASSES, key=str):
        errors.append("未归类的 winSize 调用类:%s(%d 处,如 LR 0x%x %s)——请放进 UI_LAYOUT_CLASSES 或 "
                      "KEEP_REAL_WIDTH_CLASSES" % (c, len(ws_by_class[c]), ws_by_class[c][0][0],
                                                   owners_desc(ws_by_class[c][0][1])))
    for c in sorted((UI_LAYOUT_CLASSES | KEEP_REAL_WIDTH_CLASSES) - set(ws_by_class)):
        errors.append("名单里的类在二进制里没有 winSize 调用点(过期条目或拼写错误):%s" % c)

    # --- 辅助调用点 ---
    aux_lrs = set()
    for lr, cls, sel in AUX_CALLSITES:
        s = ws_by_lr.get(lr)
        if s is None:
            errors.append("AUX_CALLSITES 0x%x 不是 winSize(stret)调用点(%s)" % (lr, lr_desc(lr)))
            continue
        if not any(o.cls == cls and o.sel == sel and not o.meta for o in imp_owner.get(s[1], [])):
            errors.append("AUX_CALLSITES 0x%x 不在 -[%s %s] 里,实际在 %s" % (lr, cls, sel, owners_desc(s[1])))
        if cls in UI_LAYOUT_CLASSES:
            errors.append("AUX_CALLSITES 0x%x 冗余:%s 已整类纳入 UI_LAYOUT_CLASSES" % (lr, cls))
        aux_lrs.add(lr)

    # --- 历史死条目:确认它确实不是 winSize 调用点、确实匹配不上 ---
    dead_lrs = set()
    notes = []
    md_chk = capstone.Cs(capstone.CS_ARCH_ARM, capstone.CS_MODE_THUMB)
    for lr, cls, sel in LEGACY_DEAD_CALLSITES:
        dead_lrs.add(lr)
        if lr in ws_by_lr:
            errors.append("LEGACY_DEAD_CALLSITES 0x%x 其实是 winSize 调用点(%s),应从历史死条目表删掉" % (
                lr, lr_desc(lr)))
        fs = func_of(lr - 2)
        if fs is None or not any(o.cls == cls and o.sel == sel and not o.meta for o in imp_owner.get(fs, [])):
            errors.append("LEGACY_DEAD_CALLSITES 0x%x 不在 -[%s %s] 里,实际在 %s" % (lr, cls, sel, lr_desc(lr)))
        insn = next(iter(md_chk.disasm_lite(mo.vread(lr - 4, 4) or b"", lr - 4)), None)
        callee = None
        if insn is not None and insn[1] == 4 and insn[2] in ("bl", "blx"):
            callee = stubs.get(imm_of(insn[3]))
        if callee not in MSGSEND_SEL_REG or callee.endswith("_stret"):
            errors.append("LEGACY_DEAD_CALLSITES 0x%x 前一条不是 bl/blx 到非 stret 的 objc_msgSend 桩(实际 %s),"
                          "无法证明它匹配不上 winSize" % (lr, "%s %s" % (insn[2], insn[3]) if insn else "解码失败"))
        else:
            notes.append("保留历史死条目 0x%x:-[%s %s] 里 %s 的返回地址,不是 winSize 调用点,查表永远匹配不上"
                         "(见 LEGACY_DEAD_CALLSITES)" % (lr, cls, sel, callee))

    # --- 各类发送的关键选择子计数(布局表类自检与候选报告用) ---
    sends_by_class = defaultdict(lambda: defaultdict(int))
    for s in sites:
        if s[4] != "call":
            continue
        for c in set(o.cls for o in imp_owner.get(s[1], [])):
            sends_by_class[c][s[3]] += 1

    for c in sorted(LAYOUT_TABLE_CLASSES):
        if c not in all_classes:
            errors.append("LAYOUT_TABLE_CLASSES 里的类在二进制里不存在:%s" % c)
            continue
        if c in UI_LAYOUT_CLASSES or c in KEEP_REAL_WIDTH_CLASSES:
            errors.append("LAYOUT_TABLE_CLASSES 与 winSize 分类表重复:%s" % c)
        cnt = sends_by_class.get(c, {})
        if cnt.get("winSize"):
            errors.append("布局表类 %s 发了 winSize(%d 次),应改按调用点归类" % (c, cnt["winSize"]))
        if not any(cnt.get(x) for x in LAYOUT_SELS):
            errors.append("布局表类 %s 没发 getPoint:/getSize:,前提不成立" % c)
        conv = dict((x, cnt[x]) for x in TOUCH_CONV_SELS if cnt.get(x))
        if conv:
            errors.append("布局表类 %s 发了触摸/世界坐标换算选择子 %s:只补居中名单不够,需另议是否进 UI43_CODE_RANGES"
                          % (c, conv))
    for c in sorted(AUX_RANGE_CLASSES):
        if c not in all_classes:
            errors.append("AUX_RANGE_CLASSES 里的类在二进制里不存在:%s" % c)

    # --- 生成三张表 ---
    offset_base = set(c for c in UI_LAYOUT_CLASSES if not NON_ROOT_RE.search(c))
    non_root = sorted(UI_LAYOUT_CLASSES - offset_base)
    real_lrs = set(s[0] for c in UI_LAYOUT_CLASSES for s in ws_by_class.get(c, []))
    gen_cs = sorted(real_lrs | aux_lrs | dead_lrs)
    gen_oc = sorted(offset_base | LAYOUT_TABLE_CLASSES, key=lambda x: x.encode())

    range_roots = offset_base | AUX_RANGE_CLASSES

    def chain_hits(c):
        for _ in range(CHAIN_DEPTH):
            if c is None:
                return False
            if c in range_roots:
                return True
            c = supers.get(c)
        return False

    range_classes = set(c for c in all_classes if chain_hits(c))

    # [2026-09-16] 运行时 ui43_class_hit 按父类链查 UI43_OFFSET_CLASSES,布局表类的【子类】也会被整体右移,
    # 但它们的方法不在 UI43_CODE_RANGES 里:与布局表类本身一样,不能发触摸/世界坐标换算选择子。
    def layout_chain_hit(c):
        for _ in range(CHAIN_DEPTH):
            if c is None:
                return False
            if c in LAYOUT_TABLE_CLASSES:
                return True
            c = supers.get(c)
        return False

    for c in sorted(all_classes - range_classes - LAYOUT_TABLE_CLASSES):
        if not layout_chain_hit(c):
            continue
        cnt = sends_by_class.get(c, {})
        conv = dict((x, cnt[x]) for x in TOUCH_CONV_SELS if cnt.get(x))
        if conv:
            errors.append("布局表类的子类 %s 发了触摸/世界坐标换算选择子 %s:它会被居中但不在代码区间,需另议"
                          % (c, conv))

    ivs = []
    range_imps = 0
    for imp, owners in imp_owner.items():
        if not any(o.cls in range_classes for o in owners):
            continue
        range_imps += 1
        if imp not in start_set:
            errors.append("白名单方法入口不是 LC_FUNCTION_STARTS 函数起点:0x%x %s" % (imp, owners_desc(imp)))
            continue
        j = bisect.bisect_right(starts, imp)
        end = starts[j] if j < len(starts) else text_hi
        ivs.append((imp, min(end, text_hi)))
    ivs.sort()
    merged = []
    for s, e in ivs:
        if merged and s <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], e)
        else:
            merged.append([s, e])
    gen_rg = [tuple(x) for x in merged]

    def range_problems(tag, rg):
        probs = []
        rs = [s for s, _ in rg]

        def inside(a):
            i = bisect.bisect_right(rs, a)
            return i > 0 and a < rg[i - 1][1]

        for s, e in rg:
            if s not in start_set:
                probs.append("%s 区间 (0x%x, 0x%x) 段首不是函数起点" % (tag, s, e))
            if e not in start_set and e != text_hi:
                probs.append("%s 区间 (0x%x, 0x%x) 段尾不是函数起点" % (tag, s, e))
            i = bisect.bisect_left(starts, s)
            while i < len(starts) and starts[i] < e:
                fs = starts[i]
                if not any(o.cls in range_classes for o in imp_owner.get(fs, [])):
                    probs.append("%s 区间 (0x%x, 0x%x) 吞进了非白名单函数 0x%x %s" % (
                        tag, s, e, fs, owners_desc(fs)))
                i += 1
        for a, nm in KNOWN_OUTSIDE:
            if inside(a):
                probs.append("%s 区间包含了 %s@0x%x" % (tag, nm, a))
        return probs

    for a, nm in KNOWN_OUTSIDE:
        if a not in start_set or imp_owner.get(a):
            warns.append("%s@0x%x 不是非 ObjC 函数起点(二进制不同?)" % (nm, a))
    errors.extend(order_problems("生成的", gen_cs, gen_oc, gen_rg))
    errors.extend(range_problems("生成的", gen_rg))

    # --- 与源码比对 ---
    same = None
    if not args.no_src:
        src_cs, src_oc, src_rg = parse_src_tables(args.src)
        errors.extend(order_problems("源码里", src_cs, src_oc, src_rg))
        errors.extend(range_problems("源码里", src_rg))
        diffs = []
        for lr in sorted(set(gen_cs) - set(src_cs)):
            diffs.append("UI43_CALLSITES 源码缺少 0x%x  %s" % (lr, lr_desc(lr)))
        for lr in sorted(set(src_cs) - set(gen_cs)):
            diffs.append("UI43_CALLSITES 源码多出 0x%x  %s" % (lr, lr_desc(lr)))
        for c in sorted(set(gen_oc) - set(src_oc)):
            diffs.append("UI43_OFFSET_CLASSES 源码缺少 %s" % c)
        for c in sorted(set(src_oc) - set(gen_oc)):
            diffs.append("UI43_OFFSET_CLASSES 源码多出 %s" % c)
        for r in sorted(set(gen_rg) - set(src_rg)):
            diffs.append("UI43_CODE_RANGES 源码缺少 (0x%x, 0x%x)" % r)
        for r in sorted(set(src_rg) - set(gen_rg)):
            diffs.append("UI43_CODE_RANGES 源码多出 (0x%x, 0x%x)" % r)
        if not diffs and (len(src_cs), len(src_oc), len(src_rg)) != (len(gen_cs), len(gen_oc), len(gen_rg)):
            diffs.append("三张表集合相同但项数不同(源码里有重复项)")
        same = not diffs
        errors.extend(diffs)

    # --- 报告 ---
    if args.report:
        log("")
        log("== winSize(stret)调用点按类 ==")
        for c in sorted(ws_by_class, key=lambda x: (x is None, str(x))):
            tag = "纳入" if c in UI_LAYOUT_CLASSES else ("保持" if c in KEEP_REAL_WIDTH_CLASSES else "未归类")
            meths = sorted(set(owners_desc(s[1]) for s in ws_by_class[c]))
            log("  %-4s %-40s %3d 处  %s" % (tag, c, len(ws_by_class[c]), "; ".join(meths)[:160]))
        log("")
        log("== 不当根层的纳入类(NON_ROOT_RE)== %s" % ", ".join(non_root))
        log("")
        log("== 布局表候选类(发 getPoint:/getSize:、不发 winSize、不在任何名单、父类链不命中居中名单)==")
        oc_set = set(gen_oc)
        listed = UI_LAYOUT_CLASSES | KEEP_REAL_WIDTH_CLASSES | LAYOUT_TABLE_CLASSES | AUX_RANGE_CLASSES
        for c in sorted((k for k in sends_by_class if k is not None), key=str):
            cnt = sends_by_class[c]
            if cnt.get("winSize") or c in listed or not any(cnt.get(x) for x in LAYOUT_SELS):
                continue
            chain = []
            k = c
            while k is not None and len(chain) < 8:
                chain.append(k)
                k = supers.get(k)
            if any(x in oc_set for x in chain[:CHAIN_DEPTH]):
                continue
            conv = sum(cnt.get(x, 0) for x in TOUCH_CONV_SELS)
            log("  %-40s getPoint:%-3d getSize:%-3d 坐标换算:%-3d 父类链 %s" % (
                c, cnt.get("getPoint:", 0), cnt.get("getSize:", 0), conv, "<".join(chain[:4])))
        log("")
        log("== 代码区间类(%d 个)== %s" % (len(range_classes), ", ".join(sorted(range_classes))))

    log("")
    log("winSize 调用点:共 %d 处(stret 调用 %d 处),分布在 %d 个类;纳入 4:3 %d 类,保持真实宽度 %d 类" % (
        len(ws_all), len(ws_stret), len(ws_by_class), len(UI_LAYOUT_CLASSES), len(KEEP_REAL_WIDTH_CLASSES)))
    log("UI43_CALLSITES      %d 项(纳入类调用点 %d + 辅助调用点 %d + 历史死条目 %d)" % (
        len(gen_cs), len(real_lrs), len(aux_lrs), len(dead_lrs)))
    log("UI43_OFFSET_CLASSES %d 项(纳入类 %d - 非根层 %d + 布局表类 %d)" % (
        len(gen_oc), len(UI_LAYOUT_CLASSES), len(non_root), len(LAYOUT_TABLE_CLASSES)))
    log("UI43_CODE_RANGES    %d 段(%d 类 %d 个方法入口,含辅助区间类 %d 个)" % (
        len(gen_rg), len(range_classes), range_imps, len(AUX_RANGE_CLASSES)))
    for n in notes:
        log("提示:" + n)
    for w in warns:
        log("警告:" + w)
    for e in errors:
        log("失败:" + e)
    if same is not None:
        log("与 %s 三张表比对:%s" % (args.src, "完全一致" if same else "不一致(见上面「失败」行)"))
    log("自检:%s" % ("全部通过" if not errors else "有 %d 项失败" % len(errors)))

    if args.emit:
        print(emit_rust(gen_cs, gen_oc, gen_rg))
    return 0 if not errors else 1


if __name__ == "__main__":
    sys.exit(main())
