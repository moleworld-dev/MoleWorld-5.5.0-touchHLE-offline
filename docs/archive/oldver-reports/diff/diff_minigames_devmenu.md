# 摩尔庄园三版对比:小游戏 + 开发者调试菜单
对比版本:**1.1.5(armv7,2012)** / **2.4.3** / **5.5.0(目标移植版)**
数据来源:三版 `method_imp_map.tsv` + `class_addr_map.tsv`(方法表/类表),辅以三版可执行文件 `strings`。
分析方式:`grep "^类名\t" 方法表 | cut -f2 | sort` 取方法集做 `comm` 三向 diff,只读,未修改任何文件。

> **一句话结论**:**没有任何核心小游戏被删除**。6 个经典街机小游戏(钓鱼/挖矿/涂鸦/犁地/切水果/捉虫)三版方法集**逐字节相同**。变化全是**增量**:2.4.3 新增"洗澡间(WashRoom)"+"神算子(Divine)"两个真小游戏;5.5.0 把神算子重构为联网扭蛋、新增一批**节日活动小游戏**和一个 PunchBox"更多游戏"广告导流位。**开发者调试菜单 5.5.0 最全**(TestLayer 24→34 方法)。

---

## Part 1:小游戏(MiniGame)

### 1.1 三版小游戏类清单对照

| 小游戏 / 入口类 | 1.1.5 | 2.4.3 | 5.5.0 | 说明 |
|---|:--:|:--:|:--:|---|
| **FishingGame**(钓鱼)+ LevelChoose + Achivement | ✅ | ✅ | ✅ | 方法集三版完全相同(52/16/6) |
| **MinerGame**(挖矿)+ LevelChoose + Achivement | ✅ | ✅ | ✅ | 37/15/6,完全相同 |
| **PaintingGame**(涂鸦)+ LevelChoose + Achivement | ✅ | ✅ | ✅ | 23/12/4,完全相同 |
| **Plow**(犁地/种植)+ LevelChoose + Achivement | ✅ | ✅ | ✅ | 36/17/4,完全相同 |
| **CutFruit**(切水果)+ LevelChoose + Achivement | ✅ | ✅ | ✅ | 49/17/4,完全相同 |
| **BugGame**(捉虫)+ LevelChoose/Base/Object/Achivement | ✅ | ✅ | ✅ | 23/17/3/24/4,完全相同 |
| **WashRoomGame**(洗澡间)+ Actor/LevelChoose/Achievement | ❌ | ✅ | ✅ | **2.4.3 新增**,沿用到 5.5.0(51/24/12/4) |
| **DivineGame**(神算子)+ DivineObject | ❌ | ✅(27) | ✅(37) | **2.4.3 新增**,**5.5.0 大改**(见 1.3) |
| **DivineIntroduceLayer**(神算子玩法介绍页) | ❌ | ❌ | ✅(3) | **5.5.0 新增** |
| **PBMoreGame / Manager / Net / View**("更多游戏") | ❌ | ❌ | ✅ | **5.5.0 新增,但不是小游戏**(见 1.5) |

经典 6 小游戏方法集 `comm` 校验(v115 vs v550):全部 `v115-only={} v550-only={}`,**零差异**。

#### 五大区分:洗澡间小游戏(WashRoom)是什么
`WashRoomGame` 方法显示这是**重力感应 + 手势分拣**玩法:`accelerateWithX:withY:withZ:`(加速度计)、`checkCommanderGesture`、`moveCommanderToLeft/Right/Up`、`checkActorIsFemale:`、`goToFemaleRoom`、`getRandomPathType`、`updateTop3Record`(排行榜)。即"指挥摩尔把男/女顾客分拣进对应澡堂"。2.4.3 引入,5.5.0 原样保留。

### 1.2 有没有被删 / 新增的小游戏?

- **被删的小游戏:无。** 1.1.5 里的全部 6 个小游戏在 5.5.0 中一个不少、方法不动。
- **2.4.3 相对 1.1.5 新增:** WashRoomGame(洗澡间)、DivineGame(神算子)。
- **5.5.0 相对 2.4.3 新增:**
  - **DivineIntroduceLayer**(神算子玩法介绍)
  - **PBMoreGame 套件**(PunchBox 第三方"更多游戏"导流广告,非自研玩法)
  - **一批联网"节日活动"小游戏**(老版本完全没有):
    | 活动小游戏类 | 方法数 | 推测玩法 |
    |---|:--:|---|
    | `SeabedSeekingTreasureMainLayer` (+Data/Rule/ExchageReward) | 35 | 海底寻宝 |
    | `GuessWorldCupMainLayer` | 23 | 竞猜世界杯(2014 限时活动) |
    | `EasterEggMainLayer` (+GetRewardLayer/ActivityInfoData) | 23 | 复活节彩蛋活动 |
    | `Activity_Alice_DiceLayer` / `Activity_Alice_TreasureLayer` (爱丽丝掷骰/寻宝) | 9 / 6 | 爱丽丝主题掷骰+寻宝 |
    | `ActivityTreasureHuntLayer` / `OpenTreasureChestMainLayer` / `TreasureHuntPopLayer` 等一大批 `Treasure*` | 11 / 11 | 寻宝/开宝箱 |

  > 这些 `Activity_*` / `*Treasure*` / `GuessWorldCup` / `EasterEgg` 都是**强联网的限时运营活动**(方法里满是 `onCommandReceived:`、`checkNetWork`、`showNetWorkError`、`gotoGetRewardWithId:`),不是常驻街机小游戏。**对离线移植价值低**(需要服务器下发活动数据)。

### 1.3 神算子(DivineGame)的版本演化 —— 5.5.0 做了什么

`DivineObject` 三版一致(扭蛋奖品对象:`objectId / num / posibility`(概率)),说明神算子=**水晶球概率抽奖**。但 `DivineGame` 本体 2.4.3(27 法)→ 5.5.0(37 法)**重写了交互与计费**:

- **5.5.0 新增**:`costGoldToDivine`、`firstCostPlay`、`nextContinuePlay`、`confirmRandomGift`、`keepRandomGift`、`confirmObject`、`generatePresentId`、`getRollingObjectId`、`resetRewardObjectOnCrystal`、`showRewardOnTheCrystal:`、`showLuckRewardOnMessageBoard`、`putAllGiftOnMap`、`showNetworkErrorMessage`、`removeNewGamerBoard`、`closeGameWithCheck`、`returnToActionCenter` 等。
- **2.4.3 有、5.5.0 删**:`generatePresent:`、`getGiftSprite:isBeforeDivine:`、`hideMessageBox`、`isHugeBuilding:`、`onChooseUseVipGold`、`playAnimationBeforeDivine`、`playMagicAnimation:`、`showGenerateGiftOnMessageBox:` 等(老的"消息框出礼物"流程)。
- **MiniGameManager 配套**:2.4.3 加 `enterDivineGameAchivement`;5.5.0 再加 `onGetDivineDataListFinished` / `onGetDivineDataListFailed`(**神算子奖品概率表改为联网下发**)。

> **移植提示**:5.5.0 神算子靠服务器下发奖品概率表(`onGetDivineDataList*`)。离线复活需**伪造一份本地奖品/概率表**喂给 `onGetDivineDataListFinished`,否则进游戏会走 `showNetworkErrorMessage`。

### 1.4 MiniGameManager 三版方法 diff(小游戏总入口)

入口签名三版一致:`startMiniGame:playType:callbackTarget:select:`;关卡 `enterMiniGame:stage:`;成就 `enterAchivement:`/`exitAchivement:`。

| 方法 | 1.1.5 | 2.4.3 | 5.5.0 |
|---|:--:|:--:|:--:|
| 基础 23 法(start/end/enter/exit/pause/resume/curGameId/curLevel…) | ✅ | ✅ | ✅ |
| `enterDivineGameAchivement` | ❌ | ✅ | ✅ |
| `onGetDivineDataListFinished` / `onGetDivineDataListFailed` | ❌ | ❌ | ✅ |
| **总方法数** | **25** | **26** | **28** |

`GameManager` 同步膨胀:**91 → 104 → 124** 法(承载新增小游戏调度 + 联网逻辑),但旧方法未删。

### 1.5 PBMoreGame ≠ 小游戏(澄清)

`PBMoreGameManager`(57 法)全是 `placementID`、`registerMoreGame:`、`pbMoreGameView:clickAd:`、`loadMoreGameData:`、`sendMoreGameClickLog…`、`baseURLLog`、`batchLogSending`。这是 **PunchBox(拍拍/PB)第三方"更多游戏"广告导流 SDK**,展示其它 App 列表赚分成,**不是可玩小游戏**。它在 5.5.0 顶替了 2.4.3 时代的一堆广告 SDK(Domob `DM*`、YouMi `YM*`、InMobi `IM*`、Tapjoy `TJC*`、Flurry),属于商业化模块换代,移植可直接忽略/桩掉。

### 1.6 关卡 / 难度 / 奖励配置

- 每个小游戏都有独立 `XxxLevelChoose`(选关)+ `XxxAchivement`(成就/奖励)类,**三版类结构与方法数完全一致**(见 1.1 表),说明关卡框架未变。
- 具体关卡/奖励数值不在二进制里,而在 **`.dat`/`.plist` 资源**(strings 见 `cutfruitiPhone.plist`、`cutfruit%d.png` 等按游戏命名的图集);Divine 的概率表 5.5.0 改为**联网下发**。
- 小游戏的中文名(钓鱼/挖矿…)也不在二进制硬编码,而在资源/服务器文案中。

---

## Part 2:开发者调试菜单(开发者留下来的菜单)

### 2.1 TestLayer 三版方法逐项 diff(主调试菜单 ±按钮面板)

`TestLayer` 是开发者作弊面板:一排 加/减 按钮直接改数值。`onButtonXxxPlus:` / `onButtonXxxMinus:` 成对出现,`getXxxChangeValue` 读步进。

| 资源项(±按钮) | 1.1.5 | 2.4.3 | 5.5.0 |
|---|:--:|:--:|:--:|
| **XP**(经验) Plus/Minus | ✅ | ✅ | ✅ |
| **Gold**(金币) Plus/Minus | ✅ | ✅ | ✅ |
| **VipGold**(贝壳/钻) Plus/Minus | ✅ | ✅ | ✅ |
| **Time**(时间/加速,带 Touched) | ✅ | ✅ | ✅ |
| **Quest**(任务,带 Touched) | ✅ | ✅ | ✅ |
| **TimeQuest**(限时任务,带 Touched) | ✅ | ✅ | ✅ |
| **VipValue**(VIP 成长值) Plus/Minus | ❌ | ❌ | ✅ **新增** |
| **VipQuest**(VIP 任务,带 Touched) | ❌ | ❌ | ✅ **新增** |
| **Food**(食物) Plus/Minus | ❌ | ❌ | ✅ **新增** |
| **Tickets**(门票/券) Plus/Minus | ❌ | ❌ | ✅ **新增** |
| getVipValueChangeValue | ❌ | ❌ | ✅ **新增** |
| 通用:`init/dealloc/updateUI/updateTime/onRecieveMessage:` | ✅ | ✅ | ✅ |
| **方法总数** | **24** | **24** | **34** |

**关键结论:**
- **1.1.5 与 2.4.3 的 TestLayer 完全相同(24 法,逐字节)。**
- **5.5.0 最全(34 法)**,比老版**多 10 个方法 / 多 4 类可改资源**:VipValue、VipQuest、Food、Tickets(对应 5.5.0 新增的 VIP 体系、餐厅食物、门票经济)。
- **没有任何调试项被删**——纯增量。老版有的 5.5.0 全有。

> **移植高价值**:5.5.0 `TestLayer` 的 18 对 ±按钮(XP/Gold/VipGold/VipValue/Time/Quest/TimeQuest/VipQuest/Food/Tickets)就是现成的"作弊金手指"。touchHLE 里只要能弹出它,就能任意改数值,验证升级表/经济系统极方便。入口见 2.3。

### 2.2 NewSceneTestLayer 方法 diff(新场景/庄园建造调试菜单)

`NewSceneTestLayer` 是"新场景"(NewScene 庄园引擎)专用调试面板,**1.1.5 没有**(NewScene 引擎 2.4.3 才上),2.4.3 与 5.5.0 都有但内容不同:

| 方法 | 1.1.5 | 2.4.3 | 5.5.0 |
|---|:--:|:--:|:--:|
| `init` / `dealloc` / `updateUI` | ❌ | ✅ | ✅ |
| Quest:`onButtonQuestPlus:/Minus:/Touched:` | ❌ | ✅ | ✅ |
| XP:`onButtonXPPlus:` | ❌ | ✅ | ✅(只剩 Plus) |
| buildValue(建造值):`onButtonbuildValuePlus:` | ❌ | ✅(含 Minus) | ✅(只剩 Plus) |
| `getbuildValueChangeValue` | ❌ | ✅ | ✅ |
| **`addCafeShopOnMap`**(在地图上刷一个咖啡馆) | ❌ | ✅ **有** | ❌ **被删** |
| `onButtonXPMinus:` / `onButtonbuildValueMinus:` | ❌ | ✅ | ❌ **被删** |
| **VipGold**:`onButtonVipGoldPlus:/Minus:` | ❌ | ❌ | ✅ **新增** |
| **VipValue**:`onButtonVipValuePlus:` + `getVipValueChangeValue` | ❌ | ❌ | ✅ **新增** |
| **方法总数** | **0** | **12** | **13** |

**这是唯一"旧版有、5.5.0 删掉"的调试功能**:2.4.3 `NewSceneTestLayer.addCafeShopOnMap`(一键在庄园地图刷咖啡馆建筑)在 5.5.0 被移除;同时 5.5.0 去掉了 XP/buildValue 的 Minus 按钮(只留加),转而加上 VipGold/VipValue 调试。整体 5.5.0 偏向"只加不减"的运营友好型调试。

### 2.3 调试菜单的入口(开发者怎么调出来)—— 移植关键

- **5.5.0 独有**:`UserInfoLayer`(玩家信息面板,点头像进入)里有 **`onButtonHideTestLayerSelected:`**,且二进制有字符串 **`addTestLayerButton_`**。即 **5.5.0 把 TestLayer 接到了"玩家信息面板"上,带一个隐藏/显示开关按钮**。
  - `UserInfoLayer` 的按钮族:`onButtonAvatarSelected: / onButtonAchieveSelected: / onButtonActionFunctionsSelected: / onButtonExchangeLayerSelected: / **onButtonHideTestLayerSelected:**` …
- **1.1.5 / 2.4.3**:`UserInfoLayer` 里**没有** `*TestLayer*` 方法,二进制也无 `addTestLayerButton_`。老版 TestLayer 的弹出走的是另一条(开发期编译开关 / 别处触发),正式包里更隐蔽。

> **移植高价值**:在 touchHLE 里 hook/调用 `UserInfoLayer` 的 `addTestLayerButton_`(或直接 `new` 一个 `TestLayer` push 进场景),即可召出 5.5.0 全套金手指面板。这是召出调试菜单**最干净的已知入口**。

### 2.4 其它"开发者遗留"入口 / cheat / 模板残留(三版扫描)

按方法名扫 `test/cheat/debug/secret/hidden` 并剔除广告 SDK / 网络库噪声后,真正的游戏逻辑遗留物:

| 遗留物 | 1.1.5 | 2.4.3 | 5.5.0 | 性质 / 移植价值 |
|---|:--:|:--:|:--:|---|
| **`HelloWorldLayer`**(cocos2d 模板残留:init/scene/dealloc + `onButtonGoSelected`) | ✅(3) | ✅(3) | ✅(4) | 模板残留,5.5.0 多一个 `onButtonGoSelected`(疑似启动跳转按钮)。**可作为最小可跑场景**做移植冒烟测试 |
| **`Farm.testhireWorker`**(免费雇农场工人) | ✅ | ✅ | ✅ | **作弊**,三版常驻。可直接调 |
| **`MinerGame.regenarateStone(s)`**(刷新矿石) | ✅ | ✅ | ✅ | 挖矿调试/刷资源 |
| **`MainMenu.testAnimation`** | ✅ | ✅ | ✅ | 主菜单动画测试 |
| **`Map.debugDraw`**(画碰撞/网格) | ✅ | ✅ | ✅ | 调试可视化,移植排查走位/碰撞很有用 |
| **`GameManager.showDebugInfo:`** | ✅ | ✅ | ✅ | 屏幕调试信息(FPS/状态) |
| **`BugGame.firstTest:`** | ✅ | ✅ | ✅ | 捉虫小游戏测试 |
| **`InAppPurchaseManager.debugTransactionInfo:`** | ✅ | ✅ | ✅ | 内购调试 |
| **`AvatarLayer.test`** | ❌ | ❌ | ✅ | **5.5.0 新增**形象测试 |
| **`NewSceneVillageMenuLayer.testEnterShop`**(直接进商店) | ❌ | ✅ | ❌ | **仅 2.4.3** 有,5.5.0 删 |
| **`TMALoginViewController.usingLoaclServerForTestOnly`**(本地测试服开关,注意拼写 Loacl) | ❌ | ✅ | ✅ | **测试服后门**,2.4.3 起常驻。**移植可借此把网络指向本地** |
| **`GuessWorldCupMainLayer.onGetRewardClickTest:`** | ❌ | ❌ | ✅ | 世界杯活动里的测试领奖钩子 |

**反作弊(非入口,顺带记录,三版都有):** 字符串 `CHEAT_WARNING` / `CHEAT_WARNING_BOX` / `FOUND_TIME_CHEAT_MESSAGE`(改系统时间作弊检测)/ `IAP free cheating userId`;5.5.0 方法 `WrapperManager.showCheatWarningMessage` + `iMoleVillageAppDelegate.showCheatWarningMessage` + `UMANUtil.deviceIsDebugging`(越狱检测)/ `appPirateString`(盗版检测)。**用 TestLayer 改时间/资源时可能触发"作弊警告",移植时若弹警告需把这些方法桩成空。**

**项目元信息(strings 泄露):** 原始 Xcode 工程路径 `**/TestProject/iMoleVillage/src/...**`,工程代号 **iMoleVillage**(与 `iMoleVillageAppDelegate` 一致)。

`EasterEggMainLayer.lightOpenSecretButton` 虽含 "Secret",但全类是复活节活动 UI(`onSureBuyEasterEgg`/`gotoGetRewardWithId:`/`showNewFindEasterEgg`),**是活动玩法不是开发者后门**,已归入 1.2 活动小游戏。

---

## 总结(给移植决策)

### ① 小游戏:有无被删 / 新增
- **被删:无。** 经典 6 小游戏(钓鱼 Fishing / 挖矿 Miner / 涂鸦 Painting / 犁地 Plow / 切水果 CutFruit / 捉虫 Bug)**三版方法集逐字节相同**,5.5.0 一个不少。
- **2.4.3 新增**:洗澡间 **WashRoomGame**(重力感应分拣)、神算子 **DivineGame**(概率扭蛋)。
- **5.5.0 新增**:神算子介绍页 `DivineIntroduceLayer`;**神算子重构为联网扭蛋**(奖品概率表服务器下发 `onGetDivineDataList*`);一批**联网节日活动小游戏**(海底寻宝 / 世界杯竞猜 / 复活节彩蛋 / 爱丽丝掷骰寻宝 / 开宝箱);以及 **PunchBox "更多游戏"广告位(PBMoreGame,非真小游戏)**。
- **移植优先级**:经典 6 + 洗澡间 = **纯本地玩法,直接可复活**;神算子需伪造本地概率表;`Activity_*/Treasure*/GuessWorldCup/EasterEgg` 强依赖运营服务器,**移植价值低**。

### ② 开发者调试菜单三版差异
- **谁更全:5.5.0 最全。** 主菜单 `TestLayer` **24(1.1.5)= 24(2.4.3)→ 34(5.5.0)**,5.5.0 多出 **VipValue / VipQuest / Food / Tickets** 四类可改资源(18 对 ±按钮)。
- **加了什么**:5.5.0 给 TestLayer 加 VipValue/VipQuest/Food/Tickets;给 NewSceneTestLayer 加 VipGold/VipValue;新增 `AvatarLayer.test`;并把 TestLayer 接入玩家信息面板(`UserInfoLayer.onButtonHideTestLayerSelected:` + `addTestLayerButton_`)。
- **删了什么(唯一一处)**:2.4.3 的 **`NewSceneTestLayer.addCafeShopOnMap`(一键刷咖啡馆)在 5.5.0 被删**,同时砍掉 NewScene 的 XP/buildValue 减号按钮;2.4.3 的 `NewSceneVillageMenuLayer.testEnterShop` 也在 5.5.0 消失。主 `TestLayer` 无删项。
- **移植高价值**:5.5.0 的 `TestLayer` = 现成全资源金手指;**入口**:在 touchHLE 调 `UserInfoLayer` 的 `addTestLayerButton_` 或直接实例化 `TestLayer` 入栈即可召出。

### ③ 其它开发者遗留入口
- **模板残留**:`HelloWorldLayer`(三版都在,cocos2d 默认模板)——可当最小冒烟场景。
- **常驻 cheat 方法(三版)**:`Farm.testhireWorker`(免费雇工)、`MinerGame.regenarateStone(s)`(刷矿)、`Map.debugDraw`(画碰撞)、`GameManager.showDebugInfo:`、`MainMenu.testAnimation`、`BugGame.firstTest:`、`InAppPurchaseManager.debugTransactionInfo:`。
- **测试服后门**:`TMALoginViewController.usingLoaclServerForTestOnly`(2.4.3 起)——**可把网络指向本地服**,对离线移植有用。
- **反作弊需注意**:`CHEAT_WARNING`/`FOUND_TIME_CHEAT_MESSAGE` + 5.5.0 `*.showCheatWarningMessage`/`UMANUtil.deviceIsDebugging`——用金手指时可能弹警告,移植时按需桩空。
- **工程代号**:`iMoleVillage`(路径 `/TestProject/iMoleVillage/src/`)。
