# 摩尔庄园 ObjC 类 / 方法三版对比报告(1.1.5 / 2.4.3 / 5.5.0)

> 纯静态符号表对比(类表 + 方法 IMP 表),只读分析,未修改任何游戏文件或 touchHLE 源码。
> 数据源:
> - 1.1.5 `v115_class_addr_map.tsv`(659 类)/ `v115_method_imp_map.tsv`(12278 方法)
> - 2.4.3 `v243_class_addr_map.tsv`(1398 类)/ `v243_method_imp_map.tsv`(26494 方法)
> - 5.5.0 `04-bridge/class_addr_map.tsv`(1968 类)/ `04-bridge/method_imp_map.tsv`(36127 方法)

## 0. 总量与三向集合规模

| 集合 | 数量 | 含义 |
|---|---:|---|
| 三版共有类 | 602 | 核心引擎 + 长青玩法 |
| A. 旧版有、5.5.0 无(被移除) | 230 | **重点**,但其中绝大多数是第三方 SDK |
| ─ A 中"游戏逻辑类"(剔除 SDK 前缀后) | 32 | 真正值得关注的删除 |
| B. 5.5.0 新增(1.1.5/2.4.3 都没有) | 751 | 其中 ~516 为游戏逻辑,~235 为新 SDK |
| ─ B 中季节/活动相关(广义) | 111 | Activity 前缀 46 + 各活动专名 65 |
| C1. 2.4.3 有、5.5.0 删(中途砍) | 181(游戏逻辑仅 ~15) | 商店旧版、Domob、刷分等 |
| C2. 1.1.5 有、2.4.3 已删(早期砍) | 49(游戏逻辑 ~18) | 社交/登录/SQLite |

> **结论先行**:三版之间的类数膨胀(659→1398→1968)几乎全部来自(a)第三方广告/分析/分享 SDK 的层层叠加,(b)5.5.0 大量"季节活动"专属类。**核心游戏逻辑层(GameData / NetworkManager / UserInfoData / WrapperManager / 商店 / 任务 / 成就 / 小游戏)在结构上高度连续**,被删的"功能逻辑"很少,主要是早期社交与登录模块。

---

## 1. 类名三向集合 diff

### A. 旧版有、5.5.0 没有的类(被移除)——按功能分组

#### A-0. 噪声:第三方 SDK(占被移除 230 类的 ~86%,对移植无价值)
这些是被替换/下线的广告联盟、分析、JSON 库、HTTP 库,与游戏玩法无关,移植时本就该剥离:
- **广告 SDK**:`DM*`(Domob,33 类)、`YM*`/`YMAD*`/`YouMi*`(有米,~90 类)、`IM*`(InMobi,~40 类)、`TJC*`(Tapjoy,~25 类)、`AtomAdapterDomob`、`immob*`。
- **分析 / 统计**:`Flurry*`、`GADMSharedContext`。
- **网络 / JSON / 加密基础库**:`TFASI*`/`TMST_ASI*`/`ASIA*`(三套 ASIHTTPRequest 拷贝)、`SBJsonStreamWriter*`(SBJSON,11 个状态机类)、`URLConnection`、`RKL*`(RegexKitLite)、`SSKeychain`、`IMCryptoExtension`/`IMGTMBase64`。
- **OAuth / 微博分享**:`OAuthController`、`OAuthEngine`、`WeiboClient`。

> 5.5.0 把这些换成了新一代 SDK(见 B 节:AppDriverChina ASI 变体、AdChina `ADC*`、TalkingData `TD*`、ShareKit `SHK*`、微信 `WX*`/`MM*`、AdMob `GAD*` 等)。**移植离线版应全部存根化(stub),不复活。**

#### A-1. 商店 / 购买(已被重写,见第 2 节方法 diff)
| 旧类 | 出现版本 | 5.5.0 去向 |
|---|---|---|
| `ShopLayer` / `ShopItems` / `StoreViewLayer` / `StoreItems` / `StoreCell` | 1.1.5 + 2.4.3 | **重写**为 `NewStyleStoreMainLayer` + `NewStyleStoreItemsView/Cell` + `NewStyleStoreMenuView` |
| `NewSceneStoreViewLayer` / `NewSceneStoreItems` | 仅 2.4.3(过渡版) | 同样被 NewStyle 系列取代 |

> 这是"重写"而非"移除"。1.1.5 的 `ShopLayer` 是 16 个方法的单层购买窗;5.5.0 `NewStyleStoreMainLayer` 有 70+ 方法,拆成主菜单 / 子菜单 / 物品视图三层,新增 VIP 金币购买(`gotoBuyVIPGold` / `onChooseBuyVipGold`)。**离线移植以 5.5.0 NewStyle 系列为准,旧商店类不需要。**

#### A-2. 登录 / 账号 / 密码(1.1.5 独有,2.4.3 起已删 — 早期砍)
`ApplyIDDialog`、`SetPasswordDialog`、`PasswordModifyDialog`、`LoginHTTPConnectManager`、`User`、`UserIDListView`、`SSKeychain`。
> 1.1.5 时代是淘米通用账号体系的客户端实现(申请 ID / 改密 / 多账号列表 / Keychain 存凭证)。2.4.3 起整体重构为新的登录 SDK 流程(5.5.0 里见 `TMALoginViewController`、`TMASetPasswordView`、`TMAPasswordModifyView`、`TMAPasswordRetrieveView`、`TMAccountManagerView`、`Login`/`LoginDialog`)。**对纯离线移植无价值(本就跳过登录)。**

#### A-3. 站内信 / 评论 / 私信社交(1.1.5 独有 — 早期砍)
`Comment`、`DirectMessage`、`Draft`、`ComposeViewController`、`DialogViewController`(MonoTouch.Dialog 风格)。
> 1.1.5 内置了一套类微博/留言板的 UGC 社交(评论、私信、草稿、撰写界面)。2.4.3 整体下线,5.5.0 改为活动驱动的轻社交(漂流瓶 `DriftBottleMessage*`、送礼留言 `GiftAndMessageLayer`、微信分享)。**这是一块"被砍掉的完整功能",但依赖服务器,离线无复活价值。**

#### A-4. 数据持久化 / SQLite(1.1.5 独有 — 早期砍)★
`DBConnection`、`Statement`(SQLite 封装)、`Status`、`Stopwatch`、`DummyGapStatus`、`URLConnection`、`OneAccountRecord`/`OneDataRecord`/`OneStatusRecord`(单条记录模型,后两类残留到 2.4.3)。
> **1.1.5 用 SQLite 做本地持久化**(`DBConnection`/`Statement` = FMDB 风格游标)。2.4.3 起改为 plist/归档 + AES 加密的 `.dat` 文件方案(即 5.5.0 现行的 `WrapperManager writeToFileWithEncrypted:fileName:md5Key:` / `getDataFromFileWithDecrypted:md5Key:`)。**存储路线已彻底换代,不复活 SQLite。**

#### A-5. 设备检测 / 视频播放(残留到 2.4.3,5.5.0 删 — 中途砍)
`TaomeeDeviceChecker`(2.4.3,5.5.0 删→换成 `TMDeviceChecker`)、`VideoViewController`、`DomobViewController`、`PunchBox`、`TaskListResponse`。
> `VideoViewController`/`DomobViewController` 是广告插播视频壳;`TaskListResponse` 是任务列表的网络响应 DTO(5.5.0 任务系统换了协议);`PunchBox` 疑似旧打卡/拳击小互动。均与服务器/广告耦合,无离线价值。

---

### B. 5.5.0 新增的类(1.1.5/2.4.3 都没有)——按功能分组

> 751 个新增类,~235 个是新一代第三方 SDK(`ADC*` AdChina、`TD*`/`TDGA*` TalkingData、`SHK*` ShareKit、`WX*`/`MM*` 微信、`GAD*` AdMob、`MA*`/`TJ*`/`TJA*`/`TJM*` 新广告墙、各种 `*_AppDriverChina` ASI 变体、`PLCrash*` 崩溃上报)。以下只列 ~516 个游戏逻辑类的代表性分组。

#### B-1. 季节 / 活动类(共 ~111,5.5.0 的最大新增板块)★服务器功能
**`Activity*` 前缀 46 个 + 各活动专名 65 个**。按活动主题:
- **加勒比 / 黄金岛**:`ActivityCaribbean*`(AllSpeed/BasePop/Rule)、`SeabedSeekingTreasure*`、`GetItemRewardFromHaiwangLayer`(海王)、`ActivityTreasureHuntLayer`、`TreasureRewardLayer/Sprite`。
- **圣诞 Xmas**:`ActivityXmasBasePopLayer`、`Activity_Xmas_{Result,Reward,RoundResult,Rule,Vote}Layer`、`XmasActivityData`、`XmasActivityPlayerData`、`CommonChristmasFatherGiftLayer`(此类三版都在,见共有)。
- **万圣节 Halloween**:`HalloweenMainLayer`、`Halloween{BlackCat,Corpse,Ghost,PumpkinHead}Sprite`、`Activity_Halloween_RewardLayer`、`ActivityHalloweenBasePopLayer`。
- **IP 联动活动**:`Activity_Alice_*`(爱丽丝,8 个)、`Activity_Shrek_*`/`ShrekActivityData`(史瑞克)、`Activity_Totoro_*`/`TotoroActivityData`(龙猫)、`Activity_IceCream_*`/`IceSummer*`(冰淇淋/夏日)、`Activity_FlameWars_*`/`FlameActivityState`(火焰之战阵营 PK)。
- **节庆**:`AnniversaryMainLayer`/`SubLayer`(周年庆)、`EasterEgg*`(复活节彩蛋)、`SpringPoem*`/`NaramSpring*`(春日诗会/娜拉姆春)、`FlyKite*`(放风筝)、`GreenRiceBall*`(青团/清明)、`MusicChapterActivityInfoData`。
- **活动框架**:`ActivityBulletinControl/Layer`(活动公告)、`ActivityForecastLayer/SecondLayer`(活动预告)、`ActivityCenterInfoData`、`ShowActivityRuleLayer`、`ActivityGiftData`。
> **这些是 5.5.0 相对旧版最大的内容增量,全部强依赖服务器下发活动配置 + 时间窗。离线移植要么存根关闭,要么硬编码一份活动配置才能激活。**

#### B-2. VIP 体系(5.5.0 全新核心商业化模块)★
`VIPLayer`、`VIPCell`、`VIPItems`、`VIPFunctionsLayer`、`VIPDailyReward`、`UserVIPInfoData`、`VipInfoData`、`VipStory`/`VipStoryLayer`、`VipQuest`/`VipQuestLayer`/`VipQuestDataInMap`、`LoginReward`、`FirstChargeGift*`(首充礼)、`PromoteSalesMainLayer`/`PromoteShowItemsLayer`(促销)、`CouponsItems`、`DailySign*`(每日签到)。
> 旧版几乎没有 VIP 概念;5.5.0 围绕 VIP 金币(vipGold)建了完整付费墙。**这解释了 `UserInfoData`/`WrapperManager` 里新增的一大批 `vipGold`/`encryptVipGold`/`addVipGold:` 方法(见第 2 节)。**

#### B-3. 轻社交 / 分享(替代被砍的 1.1.5 UGC)
`DriftBottleMessage`/`DriftBottleMessageLayer`(漂流瓶)、`GiftAndMessageLayer`、`ReceiveGiftLayer`、`CrowPriestMessageLayer`、`TeamRewardData`/`TeamStatusData`/`TeamTargetLayer`(组队活动)、`JionKiteTeamData`、`VerifyInviteCodeLayer`/`RequestCodeLayer`(邀请码)、微信互通 `WeChatApiUtil`/`{Send,Get,Show}Message{To,From}WXReq/Resp`。

#### B-4. 新玩法 / 小游戏扩展
`PopularItemsPK*`(人气物品 PK 投票:Main/Advance/Vote/Data)、`FlyKiteMainLayer`(放风筝小游戏)、`WaterTowerReward*`(水塔)、`TMMapData{RewardBox,WaterTower,YellowDuck}`(地图新装置)。

#### B-5. 反作弊 / 校验(注意:**并非 5.5.0 独有**,见下方勘误)
新增独立类:`AdWallMd5Maker`、`TaomeeAdWallMd5`、`NSStringMD5Addition_Local`、`NRSecureUDID`、`Md5Handler`(`getVerifySign:`)。
> ⚠️ **勘误抓手**:任务抓手称"5.5.0 新增反作弊 `isHackData`/`checkUserinfoMd5:`"——**实测不准确**。这两个 selector **在 2.4.3 就已存在**(`GameData isHackData`/`checkUserinfoMd5:`/`CheckUserInfoData:`、`NewSceneData`、`NewSceneUserInfoData setIsHackData:`),只是 1.1.5 时还没有。所以反作弊 MD5 校验是 **1.1.5→2.4.3 之间引入**,5.5.0 继承沿用。5.5.0 真正新增的是把这套校验扩展到了 `curLevel`/`vipGold` 的 XOR 混淆(见第 2 节 `encryptCurLevel`/`encryptVipGold`/`setNewVipGold:`)。

---

### C. 中途砍 / 早期砍

#### C1. 2.4.3 有、5.5.0 删(中途砍,游戏逻辑仅 ~15 个)
`ShopLayer`/`ShopItems`/`StoreViewLayer`/`StoreItems`/`StoreCell`/`NewSceneStoreViewLayer`/`NewSceneStoreItems`(旧商店,被 NewStyle 取代)、`DomobViewController`、`VideoViewController`、`TaomeeDeviceChecker`、`TaskListResponse`、`PunchBox`、`OneAccountRecord`/`OneDataRecord`/`OneStatusRecord`。其余 166 个全是被换代的旧广告 SDK(Domob/有米早期/Tapjoy 部分)。

#### C2. 1.1.5 有、2.4.3 已删(早期砍,49 个)
游戏逻辑:`DBConnection`/`Statement`(SQLite→.dat)、`User`/`UserIDListView`/`ApplyIDDialog`/`SetPasswordDialog`/`PasswordModifyDialog`/`LoginHTTPConnectManager`(旧账号体系→新登录 SDK)、`Comment`/`DirectMessage`/`Draft`/`ComposeViewController`/`DialogViewController`(UGC 社交,整块下线)、`Status`/`Stopwatch`/`DummyGapStatus`/`URLConnection`。SDK 残骸:`OAuthController`/`OAuthEngine`/`WeiboClient`/`SBJsonStreamWriter*`/`RKL*`/`TFASI*`/`TMST_ASI*`/`ASIAutorotatingViewController`/`SSKeychain`。

---

## 2. 关键共有类的方法级 diff

> 列出"旧版有、5.5.0 删的方法"和"5.5.0 新增方法"。方法总数演进见下表。

| 类 | 1.1.5 | 2.4.3 | 5.5.0 | 备注 |
|---|---:|---:|---:|---|
| `GameData` | 237 | 363 | **813** | 中央数据/逻辑总线,持续膨胀 |
| `NetworkManager` | 133 | 237 | **541** | 协议持续扩充 |
| `WrapperManager` | (无) | 52 | **191** | 2.4.3 新增的"逻辑门面",5.5.0 暴涨 |
| `UserInfoData` | 88 | 97 | **100** | 玩家数据模型,稳定 |
| `MiniGameManager` | 25 | 26 | 28 | 几乎不变 |
| `AchievementControl` | 26 | 27 | 28 | 几乎不变 |
| `TestLayer` | 24 | 24 | **34** | 开发者作弊面板,5.5.0 加了 Food/Tickets/VipQuest |

### 2.1 `WrapperManager`(2.4.3 新增的逻辑门面,移植最关键)★
- **2.4.3→5.5.0 删除**:`queryScore:`(旧排行榜查分,服务器功能)。
- **5.5.0 新增(节选 cheat / VIP / 活动注入点)**:
  - 资源注入(★移植作弊金矿,与 MEMORY 一致):`addXp:`、`addGold:`、`addVipGold:`、`addBuildValue:`、`addRewardTickets:`、`addInvisibleReward:num:`(+`showEffect:` 变体)。
  - 活动奖励发放:`add{Alice,Halloween,IceCream,Shrek,Totoro,Xmas}ActivityRewardToMap:num:`、`addAnniversaryRewardToMap`、`addAutumnFinalRewardToMap`、`addFirstChargeGiftWithObjectData:`、`addActionCenterReward:`、`onAdd{ActivityReward,DailySignExchange,ExchangeReward,FirstChargeGift,IceCreamReward,WaterTowerReward}ToMap:`。
  - VIP 逻辑:`checkIsVipUser`、`checkCanAccecptVipQuest`、`checkCanStartVipOnlineRewardFunc`、`onChooseBuyVipGold`、`onChooseUseVipToAccelerate{Building,Crop,Flower,Fruit}`。
  - 其它:`getLocalVersion`、`setGameMode:`、`setAnimationIntervalForCCScene`/`ForUIView`、`setIsActiveObjectsVisible:`、`addAnalyticsEvent:eventName:`。
> 移植要点:**`WrapperManager` 是所有"加资源"的统一入口**(经验链 `WrapperManager addXp: → UserInfoData addXp:`)。离线作弊只需 hook 这一层。无 `test/demo/debug/old/offline/unused/deprecated` 命名方法。

### 2.2 `UserInfoData`(玩家数据模型 — XOR 混淆的真相)★
- **1.1.5→5.5.0 删除**:`setVipGold:`(被新写法取代)。
- **5.5.0 新增**:
  - **混淆相关(对应 MEMORY 里"curLevel XOR 混淆"真因)**:`encryptCurLevel`、`setNewCurLevel:`、`encryptVipGold`、`setNewVipGold:`、`vipGoldWithNewType`。
  - 新玩法字段:`addShellTreeHarvestTimes:`/`getShellTreeHarvestTimes`(贝壳树,对应 MEMORY"贝壳用 addVipGold:")、`addRecircleSaleTimes:withSceneId:`/`currentRecircleSaleTimesWithSceneId:`/`resetRecircleSaleTimesWithSceneId:`(回收/再售)、`setDiscoverShipSailingTime:`/`getLastDiscoverShipSailingTime`(探险船)、`getLastDiscoverShipSailingTime`、`removeOneNpc:`。
> **关键结论**:旧版 `setVipGold:` 是明文写值;5.5.0 改为 `setNewVipGold:`+`encryptVipGold`(写值时做 XOR/类型混淆)、`setNewCurLevel:`+`encryptCurLevel`(等级混淆)。这正是 MEMORY 记录的"5.5.0 等级不涨真因 = curLevel XOR 混淆逻辑(非数据)"——**逻辑层证据已在方法名层面坐实**。

### 2.3 `GameData`(中央逻辑总线)
- **1.1.5→5.5.0 删除**:`getBuildingsIds`、`getDecratorsIds`(旧的建筑/装饰 ID 列表 getter,5.5.0 用别的数据结构)。
- 反作弊方法(`isHackData`/`checkUserinfoMd5:`/`CheckUserInfoData:`/`md5Check:md5:`)**2.4.3 已有**,5.5.0 沿用。
> 813 个方法,新增主要是各活动/玩法的数据存取(数量巨大,非"砍功能"信号,不逐一列举)。

### 2.4 `NetworkManager`
- **1.1.5→5.5.0 删除**:`sendNickNameToServer`(改昵称上报)、`showBindUserWarningBox`(绑定账号警告框)。
> 二者都是账号体系相关,与 A-2 的登录重构一致。无 test/offline 命名残留。

### 2.5 `MiniGameManager`(小游戏管理,基本未动)
- 删除:无。
- **5.5.0 新增**:`enterDivineGameAchivement`、`onGetDivineDataListFinished`、`onGetDivineDataListFailed`(新增"占卜/神谕"小游戏的成就与数据拉取)。

### 2.6 `AchievementControl`(成就,基本未动)
- **删除**:`checkAchieve_ReqPlant:`(旧"种植需求"成就检查)。
- **5.5.0 新增**:`checkAchieve_ReqFlowerTypeNum:`(花种类数)、`checkAchieve_ReqVIP`(VIP 成就)、`checkExistBouquetsOk:itemId:`(花束)。
> 注意 MEMORY 记录过"成就 void 坑";此处仅方法集差异,逻辑实现需另查。

### 2.7 `TestLayer` / `NewSceneTestLayer`(★开发者作弊面板,移植高价值)
- `TestLayer` **5.5.0 新增**:`onButtonFood{Plus,Minus}:`、`onButtonTickets{Plus,Minus}:`、`onButtonVipQuest{Plus,Minus,Touched}:`、`onButtonVipValue{Plus,Minus}:`、`getVipValueChangeValue`。
- `TestLayer` 5.5.0 **完整方法集**:XP / Gold / VipGold / VipValue / Time / Quest / TimeQuest / VipQuest / Food / Tickets 的 `Plus`/`Minus`/`Touched` 按钮 + `onRecieveMessage:`。**这是一个功能完整的"数值速成/作弊菜单"。**
- `NewSceneTestLayer`(2.4.3 起新增,新场景版):build值 / Quest / XP / VipGold / VipValue 的 +/- 按钮。
> **移植价值极高**:这两个类是官方留下的现成调试作弊面板,离线版只要找到入口(或 hook 一个手势/按钮 push 它)即可一键加 XP/金币/VIP/门票/食物,无需自己写 UI。建议优先复活 `TestLayer`。

---

## 3. 孤儿 / 弃用类分析(尽力而为 — 数据受限说明)

**方法学限制(必须如实说明)**:本仓库现有产物 `classref_map.tsv` 只含**外部/系统类引用**(AVAudioPlayer、CALayer、CTTelephony… 与已定义类零交集),`02-ida/out/functions.txt` 只是**函数符号-地址清单(无指令操作数、无调用 xref)**。因此**无法从静态符号表做真正的"定义但无人调用"调用图分析**。以下仅给出**弱启发式候选**,不能作为确证。

- **骨架类候选(方法集 ≤2,仅生命周期)**:5.5.0 有 231 个类方法数 ≤2,但逐一核查后**绝大多数是第三方 SDK 辅助类或 framework 分类**(`*_AppDriverChina`、`GAD*`、`SHK*`、`TJ*`、`YMAD*`、`UIViewController`/`UIWebView` 等系统类分类)。游戏逻辑里方法极少的(如 `TopMenuLayer` 2 个)基本是**继承父类**而非弃用。
- **疑似弃用方向(需后续用带 xref 的反编译核实)**:`TJCVideoViewDummy`(Tapjoy 视频占位,SDK 已基本下线)、`MASpotAdItem`/`MATask*`(新广告墙 DTO,离线必然不触发)、各 `*_AppDriverChina` ASI 拷贝(多套 HTTP 库并存,运行期只会用其一)。这些"定义了但离线不会跑"的更准确说法是**"离线环境下的死代码"**,而非源码层弃用。

> **建议**:若需精确孤儿清单,后续应在 IDA/Ghidra 里对 `__objc_classrefs` / `objc_msgSend` 调用点做 xref 统计,本批 TSV 产物不足以支撑。

---

## 4. 开发者遗留痕迹

| 类型 | 1.1.5 | 2.4.3 | 5.5.0 | 说明 |
|---|---|---|---|---|
| **cocos2d 模板残留** | `HelloWorldLayer` | `HelloWorldLayer` | `HelloWorldLayer` | 三版都在。1.1.5 仅 `init/dealloc/scene`(空模板);5.5.0 多了 `onButtonGoSelected`(被改造成某个真实入口/跳转层,非纯模板)。 |
| **作弊/调试面板** | `TestLayer`(24) | `TestLayer`(24) + `NewSceneTestLayer` | `TestLayer`(34) + `NewSceneTestLayer` | 见 2.7。官方一直保留数值作弊面板,5.5.0 还在扩充。**最有价值的遗留物。** |
| **Test/Debug 命名类** | 仅 `TestLayer` | `TestLayer`/`NewSceneTestLayer` | `TestLayer`/`NewSceneTestLayer`/`TJCVideoViewDummy` | 无 `*Demo*`、无 `*Example*`、无 `Sample` 类。 |
| **Test/Debug 命名方法**(5.5.0) | — | — | `AvatarLayer test`、`MainMenu testAnimation`、`Farm testhireWorker`、`Map debugDraw`、`InAppPurchaseManager debugTransactionInfo:`、`AtomView testDarts` 等 | 散落在真实类上的开发期测试钩子(雇工测试、飞镖测试、调试绘制、IAP 调试)。 |
| **抓手 `xiaotulv`(丝尔特 demo)** | 未出现 | 未出现 | 未出现 | **三版二进制的类名/方法名里都查不到 `xiaotulv`**。MEMORY 记录的"丝尔特 demo"可能是资源/图集命名或更早版本,非本三版 ObjC 符号。5.5.0 的"史瑞克"相关是正式活动类 `Activity_Shrek_*`/`ShrekActivityData`,非 demo。 |

---

## 附:对离线移植的"复活价值"评级

| 项 | 复活价值 | 理由 |
|---|---|---|
| `TestLayer` / `NewSceneTestLayer`(作弊面板) | ★★★★★ | 现成 UI,一键加 XP/金币/VIP/门票/食物,直接 push 即可 |
| `WrapperManager` 的 `add*` 系列 | ★★★★★ | 所有加资源的统一入口,作弊只 hook 这层 |
| `UserInfoData` 混淆方法(`setNewVipGold:`/`encryptCurLevel`) | ★★★★☆ | 解释等级/VIP 不涨,必须理解才能正确改存档 |
| 季节活动类 `Activity*`(111) | ★★☆☆☆ | 强依赖服务器配置+时间窗,需硬编码配置才能开 |
| 旧 SQLite `DBConnection`/`Statement` | ☆☆☆☆☆ | 存储已换 .dat 加密方案,不复活 |
| 旧商店 `ShopLayer`/`StoreViewLayer` | ☆☆☆☆☆ | 已被 NewStyle 系列重写,用新的 |
| 旧 UGC 社交 `Comment`/`DirectMessage` | ☆☆☆☆☆ | 1.1.5 已下线且依赖服务器 |
| 登录/账号类(全版本) | ☆☆☆☆☆ | 离线本就跳过 |
| 全部第三方广告/分析 SDK | ☆☆☆☆☆ | 应全部 stub |
