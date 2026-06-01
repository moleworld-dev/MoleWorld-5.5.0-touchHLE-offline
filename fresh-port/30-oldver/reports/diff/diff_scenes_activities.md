# 摩尔庄园 场景 / 地图 / 活动玩法 三版对比(1.1.5 → 2.4.3 → 5.5.0)

> 只读分析。数据源:三版 `class_addr_map.tsv` / `method_imp_map.tsv` / `files_*.txt`,以及直接解密/解析三版 `.app` 内的地图 `.dat`。
> AES-128-ECB key = ASCII `39653543fa0d66aa`(hex `33393635333534336661306436366161`)。

---

## 0. 核心结论(先看这里)

1. **没有任何"可玩场景 / 单机玩法"被删除。** 老版本(1.1.5/2.4.3)拥有的场景类、迷你游戏类,在 5.5.0 里**全部保留**。1.1.5→5.5.0 唯一消失的类全是**广告 SDK / 旧网络库 / 旧商店 UI**(ASIHTTPRequest、Tapjoy `TJC*`、有米 `YMAD*`、`ShopLayer`/`StoreViewLayer`),零游戏内容损失。

2. **丝尔特(xiaotulv)demo 村三版都在,且可离线复活。** `XIAOTULV_VILLAGE` 常量 + `xiaotulv_map` + `xiaotulv_userinfo` 在 1.1.5/2.4.3/5.5.0 **全部存在**;5.5.0 还多了一份冬季皮肤 `xiaotulv_winter_map`。地图本体是**明文 bplist(NSKeyedArchiver)**,userinfo 是 **AES 加密的同款存档结构**。`ChooseVillageLayer` 的 "Local" 分支(`onButtonUseLocalSelected:`/`onChooseYesLocal`)就是加载这份内置数据,**不连服务器**——这是天然的离线试玩村。

3. **季节活动几乎是 5.5.0 时代的产物,且全部联网,离线必死。** 活动相关类计数:**1.1.5 = 0,2.4.3 = 1(仅圣诞),5.5.0 = 116**。5.5.0 有 **17 个季节限时活动**(各一个 `*MainLayer`),每个都带 `checkNetWork`/`showNetWorkError`/`onCommandReceived:`/`onStateChangedTo:`,数据靠 `NetworkManager parseXxxActivityStatus:pos:len:` 从服务器二进制协议拉取。**离线全部打不开。**

4. **场景演化大势:1.1.5 是"农场 + 迷你游戏"雏形 → 2.4.3 补齐"村庄社交"骨架(主村庄渲染、公寓、咖啡馆、餐厅、节日村)→ 5.5.0 在此之上堆砌大量服务器季节活动 + 地图扩展皮肤**。底层地图引擎(`TMMapData*` 系列、`GameData loadMapdataFromResource:`)三版一脉相承,从未重写。

---

## 1. 场景 / 层类 三向对比

### 1.1 Scene 类(`*Scene`)

| Scene 类 | 1.1.5 | 2.4.3 | 5.5.0 | 说明 |
|---|:--:|:--:|:--:|---|
| `InGameScene` / `LoadingScene` / `MainMenuScene` | ✅ | ✅ | ✅ | 三版通用骨架 |
| `GameNewScene` | ❌ | ✅ | ✅ | 2.4.3 新场景框架 |
| `NewScene*` 全家桶(共 ~30 个,见下) | ❌ | ✅ | ✅ | 2.4.3 引入的"新场景"架构 |
| `NewSceneStoreItems` / `NewSceneStoreViewLayer` | ❌ | ✅ | ❌ | **2.4.3 有、5.5.0 删**(被 `NewStyleStoreMainLayer` 取代,见 §6) |

> `NewScene*` 是 2.4.3 的核心重构:`NewSceneApartment`(公寓)、`NewSceneRestaurant`(餐厅)、`NewSceneShop`(商店)、`NewSceneMapBase`、`NewSceneVillageMenuLayer`、`NewSceneQuest`、`NewSceneStory`、`NewSceneUserInfoLayer`、`NewSceneTestLayer`(调试菜单)等。5.5.0 完整继承,仅把旧商店 `NewSceneStoreItems/StoreViewLayer` 换成了新商店 MainLayer。

### 1.2 村庄 / 地图 / 子场景类(`Village`/`Farm`/`Cafe`/`Apartment`/`Map`)

| 场景 / 类 | 1.1.5 | 2.4.3 | 5.5.0 | 出现时间 |
|---|:--:|:--:|:--:|---|
| 农场 `Farm` / `FlowerFarm`(花田) | ✅ | ✅ | ✅ | 1.1.5 就有 |
| `FruitFarm`(果园) | ❌ | ✅ | ✅ | **2.4.3 加** |
| 主村庄渲染 `mainvillage_asprite`(图集) | ❌ | ✅ | ✅ | **2.4.3 加**(1.1.5 用另一套村庄渲染,无此命名) |
| `LoadingMainVillage` | ❌ | ✅ | ✅ | **2.4.3 加** |
| 公寓 `NewSceneApartment` / `ApartmentView` / `TMMapDataApartment` | ❌ | ✅ | ✅ | **2.4.3 加** |
| 咖啡馆 `CafeShop` / `CafeShopLayer` / `CafeQuest*` / `TMMapDataCafeShop` | ❌ | ✅ | ✅ | **2.4.3 加** |
| 餐厅 `NewSceneRestaurant` / `RestaurantView` / `TMMapDataRestaurant` | ❌ | ✅ | ✅ | **2.4.3 加** |
| 节日村 `HolidayVillageLayer` / `HolidayVillageMap`(`holiday_asprite` 图集) | ❌ | ✅ | ✅ | **2.4.3 加** |
| 节日村 2 `holiday2_asprite` + 主村庄扩展 `mainvillage2/3/4_asprite` | ❌ | ❌ | ✅ | **5.5.0 加**(地图扩展 / 季节皮肤) |
| 音乐厅 `MusicHallLayer` | ❌ | ❌ | ✅ | **5.5.0 加**(`checkIsUnlockMusic:` 解锁制) |
| 好友村 `FriendsVillageLayer` / `ChooseVillageLayer` | ✅ | ✅ | ✅ | 三版通用 |

### 1.3 地图建筑数据类(`TMMapData*`,反映地图里能放什么)

| 建筑类型 | 1.1.5 | 2.4.3 | 5.5.0 |
|---|:--:|:--:|:--:|
| Base / Building / Farm / Spacials / Firework | ✅ | ✅ | ✅ |
| `Ship`(船) / `SuperShellTree`(超级贝壳树) / `Shop` / `CafeShop` / `Restaurant` / `Apartment` / `TransObject` | ❌ | ✅ | ✅ |
| `WaterTower`(水塔) / `YellowDuck`(黄鸭) / `RewardBox`(奖励箱) | ❌ | ❌ | ✅ |

> 地图建筑种类持续增加,但**地图文件格式与加载入口未变**:三版都是 `GameData loadMapdataFromResource:`(注意拼写 `Mapdata`,小写 d)+ `GameManager/NewGameManager loadMapFromData:`。这是离线移植可以复用的稳定 API。

### 1.4 迷你游戏(单机可玩,全程不联网)

| 迷你游戏 | 1.1.5 | 2.4.3 | 5.5.0 | 备注 |
|---|:--:|:--:|:--:|---|
| 钓鱼 `FishingGame` / `FishLayer` | ✅ | ✅ | ✅ | 1.1.5 即有 |
| 切水果 `CutFruit` | ✅ | ✅ | ✅ | 带 `needLevel:`/`unlockLevel:` 等级解锁 |
| 矿工 `MinerGame` | ✅ | ✅ | ✅ | 同上 |
| 耕地 `Plow` | ✅ | ✅ | ✅ | 同上 |
| 捉虫 `BugGame` | ✅ | ✅ | ✅ | 同上 |
| 绘画 `PaintingGame` | ✅ | ✅ | ✅ | 同上 |
| 占卜 `DivineGame` / `DivineObject` | ❌ | ✅ | ✅ | **2.4.3 加**;5.5.0 的 `DivineGame` 数据列表走服务器(`MiniGameManager onGetDivineDataListFinished`),核心玩法可单机 |
| 洗澡间 `WashRoomLevelChoose` | ❌ | ✅ | ✅ | **2.4.3 加** |

> 迷你游戏全部数据驱动(`MiniGameManager startMiniGame:playType:`),等级解锁靠本地 `needLevel:`。**6 个核心小游戏 1.1.5 就齐了,后续只增不减。**

---

## 2. 活动玩法对比

### 2.1 三版活动规模

| 版本 | 活动相关类数量 | 季节活动 | 联网依赖 |
|---|:--:|---|---|
| **1.1.5** | **0** | 无 | —— |
| **2.4.3** | **1** | 仅圣诞老人送礼(`CommonChristmasFatherGiftLayer`)+ `ChristmasVisitedStatisticsInfo` | 服务器(`getChristmasEventFlagFromServer`、好友互送礼物) |
| **5.5.0** | **116** | 17 个季节限时活动(见 §2.2) | 全部服务器,离线死 |

> `NetworkManager` 方法数 **133 → 237 → 541**,直观反映"越来越依赖服务器"。活动入口是活动中心 `ActionCenterLayer` / `ActionCenterControl`,数据载体 `ActivityCenterInfoData`(字段:`beginTime`/`endTime`/`requirements`/`gifts`/`switchedViewId`,全由服务器下发)。

### 2.2 ★ 5.5.0 独有的季节活动(全部联网,离线必死)——共 17 个

每个活动 = 一个 `*MainLayer`,且都带 `checkNetWork` / `showNetWorkError` / `onCommandReceived:` / `onStateChangedTo:`,数据靠 `NetworkManager parse*ActivityStatus:pos:len:` 解析服务器二进制包。**没有任何一个能离线打开。**

| # | 活动(类) | 主题 / 玩法线索 | 关键服务器方法 |
|---|---|---|---|
| 1 | `CaribbeanMainLayer` | 加勒比/黄金岛航海寻宝(开船加速、`TreasureHunt`) | `delegateCaribbeanActivity`、`parseSeabedSeekingTreasure...` |
| 2 | `Activity_Alice_MainLayer` | 爱丽丝(掷骰子/兑换/回收/强化/寻宝) | `getAliceActivityStatus`、`parseAliceActivityStatus` |
| 3 | `Activity_FlameWars_MainLayer` | 火焰之战(每日捐赠/排行榜/升级) | `getFlameActivityState`、`parseFlameActivityState` |
| 4 | `XmasMainLayer` | 圣诞(投票/抽奖/轮次结算) | `applyForXmasActivity`、`voteXmasActivityUser:by:`、`parseXmasActivity...`(多达 4 个 parse) |
| 5 | `HalloweenMainLayer` | 万圣节(黑猫/尸体/鬼魂/南瓜头精灵打怪) | `delegateHalloweenActivity` |
| 6 | `IceSummerMainLayer` | 冰爽夏日(每日捐赠/奖励) | `delegateIceActivity`、`IceSummerDonationData` |
| 7 | `AutumnMainLayer` | 秋季活动 | `delegateAutumnActivity`、`hasGotAutumnActivityReward` |
| 8 | `AnniversaryMainLayer` | 周年庆(有内置图集 `AnniversaryiPad`,但仍联网) | `playAnniversaryDailyActivity` |
| 9 | `FlyKiteMainLayer` | 放风筝(组队 `JionKiteTeamData`/排行) | `jionOnTeamInFlyKiteActivity`、`parseJionFlyKiteActivityFlag` |
| 10 | `GuessWorldCupMainLayer` | 竞猜世界杯 | `getGuessWorldCupActivityInfo` |
| 11 | `SeabedSeekingTreasureMainLayer` | 海底寻宝(规则/兑换奖励) | `getSeabedSeekingTreasureActivityInfo` |
| 12 | `OpenTreasureChestMainLayer` | 开宝箱(`TreasureChestData`/`TreasureRabbit`) | `getOpenBoxActivitySwitchFlag` |
| 13 | `GreenRiceBallMainLayer` | 青团(清明,排行) | `getGreenRiceBallActivityInfo` |
| 14 | `EasterEggMainLayer` | 复活节彩蛋 | `getEasterEggActivityInfo`/`Reward` |
| 15 | `NaramSpringMainLayer` | 那拉姆春日(每日奖励/倒计时) | `getNaramSpringActivityInfo` |
| 16 | `SpringPoemMainLayer` | 春日诗词(翻页答题) | `delegateSpringPoemActivityMainLayer` |
| 17 | `AroundTheWorldMainLayer` | 环游世界(分步推进,含香港游 `isCanApplyOneDayHongKongTour`) | `getAroundTheWorldActivityInfo`、`parseGetAroundTheWorldActivityFlag...` |

**另有但属"商城/促销 UI"而非季节玩法(同样联网):** `Activity_Shrek_*`(史莱克,有 `ShrekActivityData` 但无独立 MainLayer,挂在活动中心子层)、`Activity_Totoro_*`(龙猫,同上)、`Activity_IceCream_*`(冰淇淋捐赠子层)、`MusicChapterActivityInfoData`(春日音乐章)。这些是活动中心里的子玩法层,无法离线。

**非季节的 MainLayer(商店/促销,不算活动):** `NewStyleStoreMainLayer`(新商店)、`PromoteSalesMainLayer`(促销)、`PopularItemsPKMainLayer`(热门道具 PK)、`ChoosingPagesMainLayer`(分页选择)。

### 2.3 旧版有、5.5.0 删的活动

- **无。** 2.4.3 唯一的活动(圣诞老人送礼 `CommonChristmasFatherGiftLayer`)在 5.5.0 **依然存在**(类名相同),并被扩充成完整的圣诞投票活动体系。没有任何活动被移除。

---

## 3. 被移除 / 旧版独有的玩法

### 3.1 丝尔特(xiaotulv)demo 村——**三版都在,可离线复活,非独有也非被删**

任务里"1.1.5 独有"的判断需要修正:**`xiaotulv` 在三版全部存在**。

| 资源 | 1.1.5 | 2.4.3 | 5.5.0 |
|---|---|---|---|
| 地图 | `xiaotulv_map.dat`(138 KB) | `xiaotulv_map`(76 KB) | `xiaotulv_map`(73 KB)+ `xiaotulv_winter_map`(128 KB,冬季皮肤,5.5.0 新增) |
| 用户存档 | `xiaotulv_userinfo.dat`(1.5 KB) | `xiaotulv_userinfo`(2 KB) | `xiaotulv_userinfo`(3 KB)+ `xiaotulv_winter_userinfo`(5.5.0 新增) |

**它是什么玩法:** 是登录前/选村界面(`ChooseVillageLayer`)里的**内置试玩村(本地 demo)**。`ChooseVillageLayer` 三版方法完全一致,提供两条路:
- `onButtonPreviewLocalSelected:` / `onButtonUseLocalSelected:` / `onChooseYesLocal` → **加载内置 xiaotulv(本地,不联网)**
- `onButtonPreviewRemoteSelected:` / `onButtonUseRemoteSelected:` / `onChooseYesRemote` → 连服务器拉自己的村庄

二进制里有明确常量串:`XIAOTULV_VILLAGE`、`MY_VILLAGE`、`ONLINE_VILLAGE`、`RANDOM_VILLAGE`、`SAVED_VILLAGE`、`CHOOSE_VILLAGE`。

**数据结构(已实测解析):**
- `xiaotulv_map` = **明文 bplist**,NSKeyedArchiver,根是 `NSMutableDictionary`,值为 `TMMapDataBase` / `TMMapDataBuilding`(176 个,带 `buildingState`/`coolingTime`/`gameCoolTime`)/ `TMMapDataFarm`(70 个,带 `farmState`/`cropId`)/ `TMMapDataSpacials`。共 1638 个地图对象,每个有 `objectId`/`baseTile`/`isFlip`。
- `xiaotulv_userinfo` = **AES-128-ECB 加密**(同款 key 可解),解出后是标准玩家存档:`username`(demo 玩家名 = `Saturday`)、`curLevel`(1.1.5=37 / 5.5.0=46)、`gold`、`vipGold`、`xp`、`npcs`(`NpcData` 数组)、`achieveUnlock`、`curQuestId`/`nextQuestId`/`nextStorySectionId`、`totalRooms`/`totalWorkers`。

**离线复活可行性:★ 高。** 地图明文直接可读,userinfo 用已知 key 可解,加载路径(`onChooseYesLocal` → `loadMapdataFromResource:`)不触网。这是离线移植里**最容易点亮的一个完整可逛村庄**——把 Local 分支接到内置文件即可,无需任何服务器。`showXiaoTuLvVillage`(`WrapperManager`,5.5.0)是直接入口。

### 3.2 其它旧版独有可玩玩法?

- **没有发现其它"旧版独有且被删的可玩玩法"。** 反向 diff(1.1.5/2.4.3 有、5.5.0 无)命中的全是基础设施:
  - 网络库:`ASIHTTPRequest` / `TFASIHTTPRequest` / `TFASIFormDataRequest`(被新 `AFNetworking` 取代)
  - 广告 SDK:Tapjoy `TJCAdRequestHandler`/`TJCVideoRequestHandler`/… 、有米 `YMAD*`/`YMTK*`、多盟 `DMAdRequester`/`DMAppUpdateRemind`
  - 旧 UI:`ShopLayer` / `StoreViewLayer` / `ShopItems` / `NewSceneStoreViewLayer`(商店换皮)、`IMVideoPlayer` / `IMMoviePlayerViewController`(视频播放)
  - **以上均无游戏玩法价值,删除属正常瘦身。**

### 3.3 可加载的本地地图资源(loadMapdataFromResource: 线索)

`GameData loadMapdataFromResource:` 三版都在。能从 bundle 直接加载的明文/可解地图资源:
- `xiaotulv_map`(+ 5.5.0 `xiaotulv_winter_map`)——demo 村,**明文 bplist**,确认可离线加载。
- 主村庄/节日村等运行时地图走 `RemoteMapData`(服务器下发),非 bundle 内置,离线需要造数据。

---

## 4. 场景图集资源 文件级佐证

`files_*.txt` 关键词命中数(印证场景增删):

| 关键词 | 1.1.5 | 2.4.3 | 5.5.0 | 解读 |
|---|:--:|:--:|:--:|---|
| `scene` | 8 | 36 | 44 | 2.4.3 大扩张(NewScene 架构) |
| `village` | 0 | 8 | 10 | 主村庄/节日村图集 2.4.3 引入,5.5.0 增 holiday2 |
| `cafe` | 0 | 4 | 4 | 咖啡馆 2.4.3 加 |
| `farm` | 7 | 14 | 14 | 果园 2.4.3 加 |
| `minigame` | 9 | 13 | 8 | 小游戏图集(5.5.0 部分并入图集,数量变化非玩法删减) |

村庄图集明细:
- **2.4.3 新增**:`mainvillage_asprite` / `mainvillage_npc_asprite` / `holiday_asprite` / `holiday_npc_asprite`(主村庄 + 节日村 + 各自 NPC)。
- **5.5.0 再增**:`mainvillage2/3/4_asprite`(主村庄分区/扩展)+ `holiday2_asprite`(第二节日村)。
- 季节活动美术多数打进纹理图集,文件名不单独出现(只零星见 `anniversary_shining`、`christmas_star_*`、`seabedseekingtreasure`、`AnniversaryiPad`),所以**活动增删以类表为准,文件清单仅作辅证**。

---

## 5. 场景开放等级 / 解锁条件变化(尽力而为)

- **迷你游戏**:`CutFruitLevelChoose unlockLevel:` / `needLevel:miniGameLevel:`(矿工/耕地/捉虫/绘画同构)——逐关等级解锁,**本地判定**,逻辑三版一致。
- **任务**:`QuestData initWithDict:needLevel:` —— 任务按等级开放,本地数据(`farmquest.dat`/`property.dat`)。
- **音乐厅(5.5.0 新增)**:`MusicHallLayer checkIsUnlockMusic:` —— 曲目解锁制。
- **咖啡馆/餐厅(2.4.3 新增场景)**:`CafeShopLayer displayFrame:level:` / `currentLevel` —— 与玩家等级挂钩展示。
- 未发现明确的"某主场景开放等级在版本间变化"的硬编码常量;场景解锁更多由服务器任务进度(`nextQuestId`/`nextStorySectionId`)和 `mapExtend` 驱动。**离线移植时这些等级门槛都是本地可读/可改的。**

---

## 6. 一句话演化史

- **1.1.5(2012)**:农场 + 6 个迷你游戏 + 好友村 + 丝尔特试玩村。**零季节活动。** 用 ASIHTTPRequest + 早期广告 SDK。
- **2.4.3**:`NewScene*` 架构重构,补齐**主村庄 / 公寓 / 咖啡馆 / 餐厅 / 节日村 / 果园**,加占卜/洗澡间小游戏,加**第一个(也是唯一一个)季节活动:圣诞**。地图建筑种类翻倍(船/贝壳树/各类建筑)。
- **5.5.0**:在 2.4.3 骨架上**堆季节活动(17 个限时事件,全联网)** + 地图扩展皮肤(mainvillage2/3/4、holiday2、xiaotulv 冬季)+ 音乐厅。`NetworkManager` 膨胀到 541 方法。**所有新增"玩法"几乎都是服务器活动,离线全灭;但所有老的单机内容原封不动保留。**

---

## 附:离线移植优先级提示(基于本次对比)

| 内容 | 离线可行性 | 理由 |
|---|---|---|
| 丝尔特 demo 村(可逛完整村庄) | ★★★ 高 | 明文 bplist 地图 + 可解 userinfo + Local 分支不触网 |
| 6 + 2 个迷你游戏 | ★★★ 高 | 数据驱动 + 本地等级解锁,从不联网 |
| 农场/果园/咖啡馆/餐厅/公寓主场景 | ★★ 中 | 场景类与加载 API 在,但运行时地图走 `RemoteMapData`,需造本地存档喂数据 |
| 17 个季节活动 | ☆ 极低 | 全部 `checkNetWork` + 服务器二进制协议,无本地数据兜底,逐个离线复活成本极高 |
