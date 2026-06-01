# 摩尔庄园三版(1.1.5 / 2.4.3 / 5.5.0)差异总览 — 菜单·逻辑·物品·场景·小游戏 + 移除/弃用清单

综合 4 份分节报告(同目录 `diff_classes_methods.md` / `diff_items_data.md` / `diff_scenes_activities.md` / `diff_minigames_devmenu.md`)。三版均已脱壳,AES key 通用 `39653543fa0d66aa`。**纯调研,未改任何代码/游戏资源。**

## 规模对照
| | 1.1.5(夏·初版) | 2.4.3(冬) | 5.5.0(海洋·终版) |
|---|---|---|---|
| ObjC 类 | 659 | 1398 | 1968 |
| 方法 | 12278 | 26494 | 36127 |
| 物品(property) | 310 | 479 | 1063 |
| 主线任务(farmquest) | 203 | 362 | 386 |
| 玩家等级上限(114_0) | 37 | 46 | 52 |
| 季节活动类 | 0 | 1(圣诞) | 116(17个独立活动) |
| 网络层(NetworkManager 方法) | 133 | 237 | 541 |

## 核心结论:5.5.0 对"离线相关内容"几乎是旧版的超集

**几乎没有可玩内容被删。** 类数膨胀 86% 是第三方广告/分析/分享 SDK 噪声 + 季节活动;真正被砍的游戏逻辑类只有约 32 个,且都是"重写"或"服务器依赖"而非"玩法丢失":
- 经典 6 小游戏(钓鱼/挖矿/涂鸦/犁地/切水果/捉虫)三版**方法集逐字节相同**,一个没删。
- 物品**无常驻商品被删**,只删了约 23 个限时节日装饰(烟花/鞭炮/冰雕/门松/元宵/饺子…)。
- 主线任务从不删(203→362→386)。
- 旧版的场景/玩法 5.5.0 全部保留。

## ★离线可复活候选(本轮最大价值)

### 1. 丝尔特试玩村 xiaotulv（★★★★★ 已复核确认,5.5.0 自带、离线可进)
- **纠正旧认知**:xiaotulv **不是 1.1.5 独有**,三版全有,5.5.0 还多冬季皮肤 `xiaotulv_winter_map`。
- 是选村界面 `ChooseVillageLayer` 的**内置试玩村**:`onChooseYesLocal`(离线分支,不联网,读内置文件)vs `onChooseYesRemote`(联网)。入口 `WrapperManager showXiaoTuLvVillage`。三版逻辑一字不差。
- 数据已实测:`xiaotulv_map` = **明文 bplist**(NSKeyedArchiver,~1638 对象:176 建筑+70 农场+Spacials,类 `TMMapDataBase/Building/Farm/Spacials`);`xiaotulv_userinfo` = 同 AES key 可解的玩家存档(demo 玩家名 `Saturday`,含等级/金币/经验/NPC/任务)。
- **离线最易点亮的完整可逛村庄**:把选村的 Local 分支接到内置文件即可,不依赖服务器。touchHLE 复活价值最高。

### 2. 官方完整作弊面板 TestLayer（★★★★★ 已自建等价物,可换官方版)
- 三版都在;5.5.0 扩到 34 法 / 18 对 ±按钮(XP/Gold/VipGold/VipValue/Time/Quest/TimeQuest/VipQuest/Food/Tickets),主面板纯增量无删项。
- **5.5.0 召出入口**:`UserInfoLayer onButtonHideTestLayerSelected:` + 二进制串 `addTestLayerButton_`;或直接 `new TestLayer` 入栈。
- 我们已自建 mole_menu,可继续用;但若想用官方原版数值面板,这是现成的。

### 3. 洗澡间 WashRoomGame + 本地小游戏（★★★ 纯本地可复活）
- 2.4.3 新增、5.5.0 保留,重力感应+手势分拣,**纯本地**。我们 mole_menu 的"洗澡"已覆盖。
- 神算子 DivineGame:5.5.0 重构成**联网扭蛋**(概率表服务器下发 `onGetDivineDataListFinished/Failed`),离线需伪造本地概率表喂回调,否则报网络错(我们菜单的"占卜"要注意这点)。

## 必须服务器、离线无解(理解即可,不强求)
- **17 个 5.5.0 季节活动**(全联网,`checkNetWork`+`onCommandReceived:`+`NetworkManager parseXxxActivityStatus:pos:len:` 拉服务器二进制包,美术多为服务器下载):加勒比/黄金岛、爱丽丝、火焰之战、圣诞、万圣节、冰爽夏日、秋季、周年庆、放风筝、世界杯竞猜、海底寻宝、开宝箱、青团、复活节彩蛋、那拉姆春日、春日诗词、环游世界。+ 史莱克/龙猫/冰淇淋等活动中心子层。**旧版没有任何活动被 5.5.0 删**(2.4.3 圣诞类 5.5.0 仍在并扩充)。
- UGC 社交(评论/私信/草稿/撰写,1.1.5 独有,2.4.3 全砍)、旧登录账号体系、排行榜查分 `queryScore:`(2.4.3→5.5.0 删)——全依赖服务器。

## 被"重写/换代"的(非玩法损失)
- **商店**:1.1.5 单层 `ShopLayer`(16法)→ 5.5.0 三层 `NewStyleStoreMainLayer`(70+法)。旧商店类 5.5.0 已删,用新的。
- **存储**:1.1.5 用 SQLite(`DBConnection`/`Statement`),2.4.3 起换成 plist+AES `.dat`(`WrapperManager writeToFileWithEncrypted:fileName:md5Key:`)。
- **WrapperManager**:2.4.3 才有的逻辑门面(1.1.5 无),5.5.0 涨到 191 法,集中所有 `addXp:/addGold:/addVipGold:/addRewardTickets:` 资源注入点——hook 价值最高层。

## 等级不涨真因(三方再次印证,非数据)
- 114_0.dat 等级 1–46 级 XP 值三版逐级完全一致;property/levelupHV 等表三版同格式。
- 方法名层面坐实:5.5.0 删 `setVipGold:` → 改 `setNewVipGold:`+`encryptVipGold`,新增 `encryptCurLevel`/`setNewCurLevel:`。旧版明文写值,**5.5.0 写值时做 XOR 混淆**——这才是 curLevel 不涨/数值不生效的根。

## 开发者遗留物 & 后门(三版)
- 模板残留 `HelloWorldLayer`(cocos2d 默认,可当最小冒烟场景)。
- 常驻 cheat 方法:`Farm testhireWorker`(免费雇工)、`MinerGame regenarateStones`(刷矿)、`Map debugDraw`(碰撞网格)、`GameManager showDebugInfo:`、`BugGame firstTest:`。
- **测试服后门**:`TMALoginViewController usingLoaclServerForTestOnly`(2.4.3 起,拼写 "Loacl")——可把网络指向本地服,对伪造/离线有用。
- 反作弊弹窗:`CHEAT_WARNING`/`FOUND_TIME_CHEAT_MESSAGE` + `showCheatWarningMessage`、`UMANUtil deviceIsDebugging`(越狱检测)——改时间/资源可能弹警告,需要时桩空(memory 已记)。
- 工程代号 `iMoleVillage`(路径 `/TestProject/iMoleVillage/src/`)。

## 勘误(更新到记忆)
1. **反作弊 `isHackData`/`checkUserinfoMd5:` 是 2.4.3 引入(v115=0/v243=7/v550=7),非"5.5.0 才加"**。5.5.0 真正新增的是 curLevel/vipGold 的 XOR 混淆。
2. **xiaotulv 三版都有且 5.5.0 离线可进**,非"1.1.5 独有的救不了的 demo"。
3. `WrapperManager` 是 2.4.3 新增(1.1.5 无)。

## 后续可独立开任务(本轮不实现)
- **复活丝尔特村**:touchHLE 里走 `ChooseVillageLayer onChooseYesLocal` / `showXiaoTuLvVillage`,验证内置 map/userinfo 能加载出一个可逛村庄。
- **真修等级不涨**:理顺 curLevel/encryptCurLevel/setNewCurLevel XOR 混淆口径。
- 清理 5.5.0 包里误入的 `*.decoded.plist`(无害但应清)。
