# 摩尔庄园 5.4.0 ↔ 5.5.0 代码 micro-diff 报告

> 任务:对比 5.5.0(末版补丁 / 夏季海洋更新)与其前一版 5.4.0,确认末版到底改了哪些类/方法,**重点核实等级/数值混淆逻辑在末版有没有被动过**。
> 方法:纯静态、只读。类/方法身份对比用已生成的符号表;关键链逐函数 `otool -arch armv7 -tV` 反汇编后做"指令骨架 + 逻辑立即数 + 机器码字节"三重对比。
> 结论速览:**末版补丁 = 纯内容增量(海底寻宝活动),等级/数值混淆链一行未改。**

数据源:
- 5.4.0:`30-oldver/reports/v540_class_addr_map.tsv`(1963 类)、`v540_method_imp_map.tsv`(36250 行)、二进制 `30-oldver/v5.4.0/Payload/MoleWorld.app/MoleWorld`(armv7)
- 5.5.0:`04-bridge/class_addr_map.tsv`(1968 类)、`method_imp_map.tsv`(36127 行)、二进制 `01-cracked/Payload/MoleWorld.app/MoleWorld`(armv7)

> 表格细节注意:5.4.0 方法表 36250 行里有重复(同一 category 方法挂多个宿主),去重后 35961 个唯一 `类+sel`;5.5.0 36127 行去重后 35870。Category(类名形如 `(JSONKitSerializing)`)在两版表里的"宿主归属"统计方式不同,会制造大量**伪增删**,下文已全部剔除。

---

## 1. 类集合 diff(1963 vs 1968,差 5)

`cut -f2 表 | sort -u` 后 `comm`:

### 5.5.0 新增的类(5 个,全部 = 海底寻宝 / 海王活动)

| 新增类 | 推断功能 |
|---|---|
| `SeabedSeekingTreasureData` | 海底寻宝数据模型(珍珠坐标 pearlPos / 珍珠类型 pearlType / 时间戳 lastTimestamp) |
| `SeabedSeekingTreasureMainLayer` | 海底寻宝主界面(挖贝壳、刷新、倒计时、提交米米号、兑换入口) |
| `SeabedSeekingTreasureExchageRewardLayer` | 珍珠兑换奖励界面(注意官方拼写 "Exchage" 漏 n) |
| `SeabedSeekingTreasureRuleLayer` | 海底寻宝活动规则说明界面 |
| `GetItemRewardFromHaiwangLayer` | "海王(Haiwang)"领奖界面 |

### 5.4.0 有、5.5.0 删的类

**零。** 5.4.0 的全部 1963 类在 5.5.0 中悉数保留。

**判定:** 5.4.0 = 海洋更新前的版本完全坐实——末版只在原有基础上**追加** 5 个活动类,没有任何下线/重构。海洋更新引入的玩法就是"海底寻宝 / 海王领奖"这一套联网活动。

---

## 2. 方法集合 diff(按 `类+selector+±` 去重后 comm)

剔除 category 噪声后(类名以 `(` 开头的全部丢弃,因为 category 在两版表里宿主归属统计方式不同 = 伪增删),**真实游戏类的方法增删**:

### 2a. NS*/UI* 高计数"新增" = 解析伪影,已排除

`NSString`(60)、`UIColor`(34)、`NSDictionary`(30)、`NSData`(30)、`UIDevice`(25)、`NSArray`(16)… 这些"新增"经逐 selector 回查证明**全部是 category 方法被重新归属**:5.4.0 表里它们以 `(NSString_DMOWJSON)`、`(immobViewAESCrypt)`、`(JSONKitSerializing)` 等 category 形式登记,5.5.0 表把同样的方法归到了宿主类名下。例:

| selector | 5.4.0 归属 | 5.5.0 归属 |
|---|---|---|
| `DMOWJSONValue` | `(NSString_DMOWJSON)` | `NSString` |
| `HloveyRC4:key:` | `(immobViewAESCrypt)` | `NSString` |
| `JSONString` / `JSONData` | `(JSONKitSerializing)` 等 | `NSString` |

→ 这些不是新代码。**5.4.0 删除方法剔除 category 后 = 0 条真实游戏类删除。**

### 2b. 真正有意义的代码增量(全部围绕海底寻宝)

**`GameData`(+20,纯海底寻宝状态字段的 getter/setter):**

```
userCurrentOwnPearlCount / setUserCurrentOwnPearlCount:   当前拥有珍珠数
diggedShellsArray / setDiggedShellsArray:                 已挖贝壳数组
bDigShellPlayers / setBDigShellPlayers:                   挖贝壳玩家数
seabedSeekingTreasureActivityFlag / set…                  活动开关
isExchangeSeabedSeekingReward / set…                      是否已兑换
isGetDigHaiwangShellReward / set…                         是否已领海王贝壳奖
isOpenGreatRewardLayer / set…                             是否开大奖界面
refreshNewShellType / set…                                刷新出的新贝壳类型
exChangeRewardButtonTag / set…                            兑换按钮 tag
bExchangeReward / setBExchangeReward:                     可否兑换
```

**`NetworkManager`(+14,海底寻宝联网协议收发/解析):**

```
getSeabedSeekingTreasureActivityInfo                          拉取活动信息
parseSeabedSeekingTreasureActivityInfo:pos:len:              解析活动信息
seabedSeekingTreasureDigShellWith:shellType:pearlCount:      上报挖贝壳
seabedSeekingTreasureDigShellToGainMimiCoinWith:coinCount:   挖贝壳换米米币
parseSeabedSeekingTreasureDigShell:pos:len:                  解析挖贝壳结果
seabedSeekingTreasureRefreshShells                           请求刷新贝壳
parseSeabedSeekingTreasureRefreshShells:pos:len:             解析刷新结果
seabedSeekingTreasureExchangeRewardWithPearlCount:           珍珠兑换奖励
getFinalGreatReward                                          领最终大奖
parseStatisticExchangePlayersCount:pos:len:                  统计兑换人数
seabedSeekingTreasureActivityResponder / set…               活动回调 responder
getWetherShowUpdateService                                   是否展示"停服更新"
parseStopUpdateServiceState:pos:len:                         解析停服状态
```

**5 个新活动类自身的方法**(界面/触摸/网络错误/文本输入等,详见各类,均为标准 cocos2d Layer 实现)。

**零散 3 条**(与海洋更新配套的小改动):

| 类 | 新增 selector | 说明 |
|---|---|---|
| `AutoPopZhongXinLayer` | `closeGreatRewardLayer` | 关大奖层(海底寻宝大奖联动) |
| `AutoPopZhongXinLayer` | `detach` / `onReceiveThousandVipgold` | 自动弹窗/收千贝壳推送 |
| `DiscountInfoLayer` | `onButtonLinkToItunesSiteOfIseer` | 跳 iTunes(《赛尔号》导流,运营位) |

> `WrapperManager` / `UserInfoData` 的**方法集合无任何增减**(下一节会看到连指令都没动)。`GameData`/`NetworkManager` 的增量 100% 是海底寻宝,与等级/数值核心无关。

---

## 3. ★ 等级/数值混淆链 disasm 对比(关键结论)

对下列函数,在两版二进制各自 `otool -arch armv7 -tV` 反汇编,**按函数切片**(以方法表全部起始地址作为函数边界;注意 5.4.0 表保留 Thumb bit、地址为奇数,已统一清除 bit0 → 真实指令地址),做三重对比:
- **指令骨架**(仅助记符序列)
- **逻辑立即数**(只保留 `eor/and/orr/lsl/asr/mul/cmp/...` 等算术逻辑指令及其立即数 —— 混淆 key 就藏在这里)
- **机器码字节**(终极:连 4 字节编码都比)

| 函数 | 540 地址 | 550 地址 | 行数(540/550) | 骨架 diff | 逻辑立即数 diff | 机器码字节 diff |
|---|---|---|---|---|---|---|
| `CryptUtils encryptInt:` | 0x1225f8 | 0x124b28 | 4 / 4 | **0** | **0** | **0(完全一致)** |
| `CryptUtils decryptInt:` | 0x122604 | 0x124b34 | 4 / 4 | **0** | **0** | **0(完全一致)** |
| `CryptUtils encrypt:key:options:` | 0x1223fc | 0x12492c | 19 / 19 | **0** | **0** | 6(仅地址池立即数) |
| `CryptUtils decrypt:key:options:` | 0x122438 | 0x124968 | 19 / 19 | **0** | **0** | 6(仅地址池立即数) |
| `UserInfoData addXp:` | 0xb8db8 | 0xbb040 | 137 / 137 | **0** | **0** | 106(仅地址池立即数) |
| `UserInfoData checkUpgrade` | 0xb84cc | 0xba754 | 399 / 399 | **0** | **0** | 330(仅地址池立即数) |
| `UserInfoData curLevel` | 0xb8d74 | 0xbaffc | 18 / 18 | **0** | **0** | 12(仅地址池立即数) |
| `UserInfoData encryptCurLevel` | 0xb8da8 | 0xbb030 | 6 / 6 | **0** | **0** | 2(仅地址池立即数) |
| `UserInfoData setNewCurLevel:` | 0xb9efc | 0xbc184 | 6 / 6 | **0** | **0** | 2(仅地址池立即数) |
| `UserInfoData setCurLevel:` | 0xbb2c8 | 0xbd550 | 6 / 6 | **0** | **0** | 4(仅地址池立即数) |
| `UserInfoData encryptVipGold` | 0xb8d08 | 0xbaf90 | 6 / 6 | **0** | **0** | 2(仅地址池立即数) |
| `WrapperManager addXp:` | 0x25d8b8 | 0x261970 | 66 / 66 | **0** | **0** | 36(仅地址池立即数) |

### 3.1 核心混淆原语:`encryptInt:` / `decryptInt:` —— 两版机器码逐字节一致

```
CryptUtils encryptInt:                  CryptUtils decryptInt:
540 @0x1225f8 / 550 @0x124b28           540 @0x122604 / 550 @0x124b34
  f2410011  movw  r0, #0x1011             f2410011  movw  r0, #0x1011
  f2c01001  movt  r0, #0x101              f2c01001  movt  r0, #0x101
  4050      eors  r0, r2                  4050      eors  r0, r2
  4770      bx    lr                      4770      bx    lr
```

- 两版**机器码字节 100% 相同**(`f2410011 f2c01001 4050 4770`),仅函数所在地址不同。
- 混淆 = 单条 **XOR**:key = `movt #0x0101` 拼 `movw #0x1011` = **`0x01011011`**(与已知 `CryptUtils` XOR key 完全吻合),对入参 r2 做 `eors`。encrypt 与 decrypt 是**同一段异或**(XOR 自反),互为逆运算。
- 这就是"等级不涨"怀疑的混淆根。**它在 5.4.0 与 5.5.0 中是同一块二进制,末版一个 bit 都没动。**

### 3.2 等级访问器:差异 100% 是二进制布局产物,语义零变化

`encryptCurLevel` / `setNewCurLevel:` / `curLevel` / `setCurLevel:` / `encryptVipGold` 都是**纯 ivar 存取器**,形态一律为 `movw/movt(取 PC 相对字面量池槽位); add r1,pc; ldr r1,[r1]; ldr|str r0/r2,[r0,r1]; bx lr`。机器码字节 diff(2~4 行)**全部落在 `movw/movt` 的立即数上**——那是"ivar 引用字面量池的偏移",在两版里因二进制整体布局不同(5.5.0 多了 5 个类、更多方法)而必然不同。举证 `setCurLevel:`:

```
540 @0xbb2c8                            550 @0xbd550
  f64f7124  movw  r1, #0xff24    <--->    f646219c  movw  r1, #0x6a9c   仅此偏移不同
  f2c001a3  movt  r1, #0xa3      <--->    f2c001a4  movt  r1, #0xa4     仅此偏移不同
  4479      add   r1, pc                  4479      add   r1, pc        相同
  6809      ldr   r1, [r1]                6809      ldr   r1, [r1]      相同
  5042      str   r2, [r0, r1]            5042      str   r2, [r0, r1]  相同(写 ivar)
  4770      bx    lr                      4770      bx    lr            相同
```

剥离这些"地址池立即数"后(本报告的"骨架"与"逻辑立即数"两个视图正是为此而设),**diff 归零**。

### 3.3 上层链:`addXp:` / `checkUpgrade` / `WrapperManager addXp:`

三者**指令骨架、逻辑立即数 diff 均为 0**;机器码字节 diff(36/106/330 行)同样**全部是 `movw/movt` 字面量池偏移**(selector、ivar、字符串引用因布局差异而改变),无一条是算术/逻辑/分支结构的改变。即:`WrapperManager addXp:` → `UserInfoData addXp:` → `checkUpgrade` 这条链,以及里面对升级表(`#0x1388`=5000、`#0x13xx` 等阈值常量,在 logic 视图里两版完全一致)的判断逻辑,**末版补丁未作任何修改**。

### 3.4 等级链最终判定

**末版补丁(5.4.0 → 5.5.0)没有改动任何等级/数值混淆逻辑——逐函数指令骨架与逻辑立即数零差异,核心 XOR 原语 `encryptInt:`/`decryptInt:` 连机器码字节都完全相同。**

推论(对"等级不涨"修复的价值):
- "等级不涨" **若**根因是 curLevel XOR 混淆(key `0x01011011`)+ 升级判断逻辑,那么这个行为**在 5.4.0 里同样存在**,**并非末版引入**。
- 因此 **5.4.0 对修这个 bug 没有额外价值**——它的等级链与 5.5.0 是同一份代码,不可能成为"对照出哪里被改坏"的参照。修复方向仍应锁定运行时层(touchHLE 移植里 XOR 解码/`checkUpgrade` 的本地实现),而不是寄望于"末版改了混淆所以换 5.4.0 的逻辑"。
- 同理 `encryptVipGold` 也未变(本就 2.4.3 起存在),贝壳/VIP 金币的混淆口径两版一致。

---

## 4. 活动类增删(海洋更新加了哪些联网活动)

`grep -iE 'activity|caribbean|haiwang|seabed|treasure|ocean|summer'` 类集合:

- **新增(均为联网活动):** `SeabedSeekingTreasure{Data,MainLayer,ExchageRewardLayer,RuleLayer}` + `GetItemRewardFromHaiwangLayer`(海王领奖)。配套服务端协议见 §2b 的 `NetworkManager` +14 方法(拉活动信息 / 上报挖贝壳 / 刷新贝壳 / 珍珠兑换 / 领最终大奖 / 统计兑换人数 / 停服更新状态)。
- **删除:** 无。
- **Caribbean(加勒比/黄金岛):** 两版**都没有** `Caribbean*` 类——与既有结论一致(全系列均无 Caribbean,旧版救不了黄金岛),海洋更新也没补。
- 海底寻宝是一个**纯联网**玩法(数据靠服务器下发珍珠坐标/类型、挖贝壳上报、兑换走网络),在 touchHLE 离线环境下需要本地桩才能跑通——属于"海洋更新新增的联网活动",离线移植要么打桩要么直接屏蔽入口。

---

## 附:复现方法(只读)

```bash
# 类/方法集合
cut -f2 v540_class_addr_map.tsv | sort -u > c540; cut -f2 04-bridge/class_addr_map.tsv | sort -u > c550
comm -13 c540 c550   # 550 新增;comm -23 = 540 删除
awk -F'\t' '{print $1"\t"$2"\t"$4}' 表 | sort -u  # 方法身份(忽略地址)

# 反汇编切片对比(关键:540 表地址为奇数=带 Thumb bit,需 &~1)
otool -arch armv7 -tV 二进制 > textdump.txt
# 以"全部方法起始地址(清 bit0 后)"为函数边界切片,
# 再用 助记符序列 / 逻辑指令立即数 / 机器码字节 三视图 diff。
# encryptInt:/decryptInt: 机器码逐字节相同 = 混淆原语未改的铁证。
```
