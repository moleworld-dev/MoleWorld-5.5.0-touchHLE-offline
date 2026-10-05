# 摩尔庄园三版本资源对比报告（离线移植视角）

> 目的：为 touchHLE 离线运行的 **5.5.0** 找出可从旧版「反向移植」的缺失资源/数据。
> 纯文件系统层分析，不碰二进制反汇编。生成日期 2026-06-01。

## 0. 版本概况

| 版本 | 路径 | 文件数 | 总大小 |
|------|------|-------|--------|
| **5.5.0（目标）** | `01-cracked/Payload/MoleWorld.app` | 2188 | 103.9 MB |
| 2.4.3 | `30-oldver/v2.4.3/Payload/MoleWorld.app` | 1714 | 95.3 MB |
| 1.1.5 | `30-oldver/v1.1.5/Payload/Mole's World.app` | 1206 | 64.7 MB |

完整清单见同目录 `files_v5.5.0.txt` / `files_v2.4.3.txt` / `files_v1.1.5.txt`（格式：`字节大小<TAB>相对路径`）。

**关键前提：5.5.0 不是纯 iPad 包。** 它同时带 iPhone + iPad 两套资源：
- 5.5.0 含 `iPhone` 字样文件 **795** 个，`iPad` 字样 **299** 个。

这一点直接决定了下面大量「旧版有、5.5.0 没有」的 iPhone 资源 **没有移植价值**（5.5.0 自己已有同名 iPhone 版或对应 iPad 版）。

---

## 1. 集合 diff：旧版有、5.5.0 没有（按 basename）

| 来源 | 候选总数 | png | dat | plist | pvr.ccz | mp3 | mp4 | xml | 其他 |
|------|---------|-----|-----|-------|---------|-----|-----|-----|------|
| **v2.4.3-only** | 145 | 58 | 32 | 23 | 17 | 11 | 4 | 0 | — |
| **v1.1.5-only** | 124 | 55 | 3 | 14 | 8 | 4 | 3 | 34 | nib×2 |

### 1a. 绝大多数是 iPhone 分辨率冗余（无价值）

v2.4.3-only 的 58 个 png / 17 个 pvr.ccz / 23 个 plist、v1.1.5-only 的大部分，文件名都带 `iPhone` 后缀，是 **iPhone 分辨率的图集 + 索引 plist + 加密图集索引 .dat**，例如：

```
mainvillage_aspriteiPhone.png/.dat   scene_aspriteiPhone.png/.dat
npc_aspriteiPhone.*   mole_aspriteiPhone.*   shopping_aspriteiPhone.*
fishiPhone.pvr.ccz/.plist   cutfruitiPhone.*   hv_ui1iPhone.*  ...
```

实测 5.5.0 **已自带对应 iPad 版图集**（抽样命中数）：

| 图集 | 5.5.0 里 `*iPad*` 命中 |
|------|----------------------|
| mainvillage_aspriteiPad | 2 |
| scene_aspriteiPad | 28 |
| npc_aspriteiPad | 6 |
| mole_aspriteiPad | 2 |
| shopping_aspriteiPad | 2 |
| fishiPad | 2 |
| cutfruitiPad | 2 |

结论：这些 `*iPhone*` 资源对 5.5.0 离线 **无增量价值**（要么 5.5.0 自己有 iPhone 版，要么有更高清的 iPad 版）。

### 1b. `_aspriteiPhone.dat`（32 个）= 图集索引，不是数据表

v2.4.3-only 的 32 个 .dat 里，30 个是 `*_aspriteiPhone.dat`（加密的 cocos2d 图集坐标索引，配对上面的 iPhone 图集），同样冗余。真正的「数据表」候选只有：`120_0.dat`、`ShopItems.dat`（见 §3）。

### 1c. v1.1.5-only 的 png/xml = 1.1.5 时代旧美术管线（已废弃）

v1.1.5 用「逐帧 PNG 大图 + `*_config.xml` 动画配置」的老管线：
```
mole_leftdown_1.png  momo_leftdown_1.png  koala_leftdown_1.png ...
mole_config.xml  momo_config.xml  yali_config.xml ... (各带 @iphone 变体, 共34个xml)
```
5.5.0 改用 pvr.ccz 图集管线，**不引用**这些。抽查 `mole_leftdown_1.png` / `momo_leftdown_1.png` / `koala_leftdown_1.png` 在 5.5.0 命中均为 **0**，但属于不同美术体系，无法直接套用。无价值。

---

## 2. 真正有价值的缺失资源（移植金矿）

### 2a. 【高价值】背景音乐 / 小游戏音乐 / 剧情音乐（11 个，纯 MP3，可直接拷）

5.5.0 的 BGM/GAME/STORY 编号 **有明显跳号**，缺的全是服务器下载的：

5.5.0 现有：`BGM_001/002/004/005/008/009/014/015`、`GAME_101/501/601`、`STORY_001/002`、`SPLASH`
**5.5.0 缺失（全盘 grep 命中 0）**，而 **v2.4.3 本地自带**：

| 文件 | 大小 (字节) | v2.4.3 | v1.1.5 | 类型推断 |
|------|------------|--------|--------|---------|
| BGM_003.mp3 | 904,056 | ✔ | ✔ | 背景音乐 |
| BGM_006.mp3 | 888,627 | ✔ | — | 背景音乐 |
| BGM_007.mp3 | 1,026,237 | ✔ | — | 背景音乐 |
| BGM_010.mp3 | 802,089 | ✔ | — | 背景音乐 |
| BGM_011.mp3 | 926,641 | ✔ | — | 背景音乐 |
| BGM_012.mp3 | 933,328 | ✔ | — | 背景音乐 |
| GAME_201.mp3 | 515,883 | ✔ | ✔ | 小游戏音乐 |
| GAME_301.mp3 | 708,066 | ✔ | ✔ | 小游戏音乐 |
| GAME_401.mp3 | 483,774 | ✔ | ✔ | 小游戏音乐 |
| GAME_801.mp3 | 654,549 | ✔ | — | 小游戏音乐 |
| STORY_003.mp3 | 339,826 | ✔ | — | 剧情音乐 |

**合计 ~8.18 MB / 11 文件**，全在 v2.4.3 根目录。

**格式验证（确认是真 MP3，非加密）：**
- v2.4.3 `BGM_003.mp3` 头：`FF FB 90 60 ...`（MPEG 帧同步，裸 MP3，可直接播放）
- 对照 5.5.0 自带 `BGM_002.mp3` 头：`49 44 33 03 ...`（`ID3` 标签 MP3）

两者都是标准 MP3，5.5.0 引擎本来就在播这类文件。**v2.4.3 这 11 个可原样拷进 5.5.0 根目录补齐离线 BGM。** BGM_003 / GAME_201/301/401 优先用 v2.4.3 版本（v1.1.5 的 BGM_003 大小不同=888,627，是早期混音，建议用 v2.4.3 那版与 5.5.0 风格一致）。

### 2b. 【中价值】成就图集 achievement（5.5.0 完全缺失，唯一来源=旧版）

5.5.0 **完全没有任何成就资源**：`grep -iE 'achiev|medal|honor|trophy|badge'` 在 5.5.0 清单命中 **0**，连成就数据表都没有。而旧版自带成就图集：

| 文件 | v2.4.3 | v1.1.5 | 内容 |
|------|--------|--------|------|
| achievementiPhone.pvr.ccz | 134,758 B | 58,616 B | 成就徽章图集 |
| achievementiPhone.plist | 5,502 B | 2,615 B | 图集坐标索引（bplist） |

`achievementiPhone.plist` 是明文可解的 cocos2d 二进制 plist，引用图集 `achievementiPhone.pvr.ccz`，含 **`_achievement_badge_1.png` ~ `_56.png` + `_unlocked.png`** 等帧。
- **v2.4.3 含 57 个 badge 帧**，v1.1.5 只含 21 个 → **优先用 v2.4.3 版本**。

注意：这是 iPhone 版图集（512×512），旧版也没有 iPad 版成就图。能否生效取决于 5.5.0 成就 UI 是否真的去加载 `achievementiPhone.pvr.ccz`（结合 memory 里「成就 void 坑」，这块本就是问题区）。**作为离线唯一可得的成就美术，值得拷过去试。**

### 2c. 【低价值】片头视频 taomee*.mp4

5.5.0 **一个 .mp4 都没有**。旧版有发行商片头：`taomeeiPad.mp4`(491,952)、`taomeeiPhone-hd.mp4`、`taomeeiPhone.mp4`、`taomeeiPhone5.mp4`(v2.4.3)。属于淘米 Logo 开场动画，**对玩法无影响**，仅观感。可选拷 `taomeeiPad.mp4` 补片头。

---

## 3. 同名数据表 .dat 对比（核心）

### 3a. 【关键结论】levelupHV.dat 三版同格式、且 5.5.0 与 v2.4.3 **字节完全相同**

| 文件 | 5.5.0 | v2.4.3 | cmp 结果 | 文件头 |
|------|-------|--------|---------|--------|
| **levelupHV.dat** | 1504 B | 1504 B | **IDENTICAL（字节完全相同）** | 两版均 `F7 12 D2 B3 FF 42 08 FD ...`（加密） |
| timequestHV.dat | 448 B | 448 B | **IDENTICAL** | 加密 |
| DailyQuestHV.dat | 688 B | 688 B | **IDENTICAL** | 加密 |

> **对 5.5.0「等级不涨」bug 的直接含义：**
> `levelupHV.dat` 与正常工作的 v2.4.3 **逐字节一模一样**（同一加密 blob，非明文）。
> 因此升级表数据文件本身没坏，**「从旧版借 levelupHV.dat」不会有任何改变**——5.5.0 用的就是同一个文件。
> 升级 bug 的根因不在数据文件层，而在**二进制的解密/解析逻辑**（属于反汇编层，不在本次文件对比范围）。
> v1.1.5 没有 `levelupHV.dat`（HV 命名是 2.4.x 之后才有），无参考价值。

### 3b. 其余 HV 表：5.5.0 更大/更新，**不可降级**

| 文件 | 5.5.0 | v2.4.3 | cmp | 判断 |
|------|-------|--------|-----|------|
| farmquestHV.dat | 3856 B | 3600 B | 不同（char 1 起就不同） | 5.5.0 内容更多，**保留 5.5.0** |
| propertyHV.dat | 38880 B | 11392 B | 不同 | 5.5.0 大 3 倍（更多道具），**保留 5.5.0** |
| DailyQuest.dat | 1264 B | 1104 B | 不同 | 5.5.0 更新，**保留 5.5.0** |
| cafeQuestHV.dat | 688 B | 432 B | 不同 | 5.5.0 更新，**保留 5.5.0** |

这些都是「5.5.0 比旧版内容更全」，从旧版拿只会丢内容。

### 3c. 基础（非 HV）表三版均为加密，旧版**不是明文**

抽查 `farmquest.dat` / `property.dat` / `timequest.dat`：

| 文件 | 5.5.0 头 | v2.4.3 头 | v1.1.5 头 |
|------|---------|----------|-----------|
| farmquest.dat | `4C 4A B2 06 77 E8 ...` | `B7 67 2B FB EE D6 ...` | `44 BA 50 D5 93 B6 ...` |
| property.dat | `56 CC 98 14 D4 68 ...` | `7A EC F4 31 D8 39 ...` | `53 02 4E 7C B3 E3 ...` |
| timequest.dat | `D7 36 CA 9C 15 F3 ...` | `F6 19 FF 94 A1 1F ...` | `BE 40 C3 C8 C1 EC ...` |

三版头各不相同、全是高熵随机字节 = **都加密**（无 `bplist` / 无明文 XML / 无 JSON）。**旧版 .dat 不是明文版**，无法用来「白嫖明文格式」。大小对比 5.5.0 也普遍更大（如 property.dat 67872 vs v1.1.5 20992）。

### 3d. 其他数据表 .dat 抽样

- `120_0.dat`（v2.4.3-only, 1232 B）头 `CA 98 4B 0C 61 EE ...` = 加密，编号体系与 5.5.0 的 `1xx_0/2xx_0/3xx_0.dat`（地图/场景分块数据）一致，但 5.5.0 无此编号，可能是已删除的旧场景，价值低。
- `ShopItems.dat`（v2.4.3 & v1.1.5 各带 en/zh-Hans/zh-Hant 三语，~864-944 B）头 `12 FA 39 D1 ...` = 加密。5.5.0 无同名文件（商店改走别的数据结构），格式不通用，无价值。
- `xiaotulv_map.dat`（v1.1.5-only, 138,786 B）头 **`62 70 6C 69 73 74 30 30`=`bplist00`（明文二进制 plist！）** —— 是 v1.1.5「小土驴」玩法的地图数据，5.5.0 无此玩法，无价值，但记录在案：v1.1.5 个别数据是明文 bplist。
- `xiaotulv_userinfo.dat`（v1.1.5-only, 1504 B）头 `33 8A BF A2 ...` = 加密。

---

## 4. 配置 / 映射 plist

- `uncompressUIiPhone.plist`：**三版都有**，不在 diff 列表中（非旧版独有）。未发现 5.5.0 缺失的解压配置。
- 旧版独有的 plist 几乎全是 `*iPhone.plist` 图集索引（配对 §1a 的 iPhone 图集），随对应图集一起冗余。
- v1.1.5 独有 `ObjectType.plist`(324B)、`Animations.plist`(4314B)、`Animations@iphone.plist`(4440B)：是 1.1.5 老动画/对象类型配置，配 §1c 的废弃美术管线，5.5.0 不用。
- **结论：没有「旧版 plist 带着 5.5.0 缺失资源索引」的情况。** 唯一有意义的索引是 `achievementiPhone.plist`（§2b，随成就图集一起拷）。

---

## 5. 声音资源汇总

- 5.5.0 共 **422** 个音频（mp3/caf/wav；其中 `UI_021.wav` 是唯一 wav，其余 mp3，无 caf）。
- **旧版有、5.5.0 缺**的音频 = §2a 的 11 个 BGM/GAME/STORY（已确认 v2.4.3 本地自带、纯 MP3、可直接补）。
- 反向（5.5.0 有、旧版无）：195 个 mp3（大量 SOUND_*/NPC_*/EFFECT_* 新音效），属 5.5.0 新增，与离线缺图无关。

---

## 6. 反向：5.5.0 有、旧版都没有（仅备注）

5.5.0 独有 basename **592** 个：png 269、mp3 195、plist 50、dat 43、ccz 12、jpg 8、sql 3、txt 2、html 2、ccb 2、strings 1、nib 1。
多为 5.5.0 新功能/新资源（新玩法图集、新音效、SQLite 数据库等），**对离线缺图无帮助**，不展开。

---

## 7. 总结：值得从旧版拷到 5.5.0 离线版的清单

| 优先级 | 资源 | 数量/大小 | 来源 | 说明 |
|-------|------|----------|------|------|
| ⭐⭐⭐ 高 | BGM_003/006/007/010/011/012、GAME_201/301/401/801、STORY_003（.mp3） | 11 个 / ~8.18 MB | **v2.4.3** 根目录 | 5.5.0 BGM 跳号缺失、服务器下载；裸 MP3 可直接播；补齐离线背景/小游戏/剧情音乐 |
| ⭐⭐ 中 | achievementiPhone.pvr.ccz + achievementiPhone.plist | 2 个 / ~140 KB | **v2.4.3**（57 帧，比 v1.1.5 全） | 5.5.0 零成就美术，唯一来源；成就 UI 缺图可试补 |
| ⭐ 低/可选 | taomeeiPad.mp4 | 1 个 / 492 KB | v2.4.3 | 淘米片头，仅观感，5.5.0 无任何 mp4 |

**关键数据表（levelupHV.dat 等）结论：**
- `levelupHV.dat`、`timequestHV.dat`、`DailyQuestHV.dat` —— 5.5.0 与 v2.4.3 **字节完全相同**，同一加密格式。**借旧版毫无意义**，5.5.0 用的就是同一文件。
- 5.5.0「等级不涨」**不是数据文件问题**，根因在二进制解密/解析逻辑（超出文件对比范围）。
- `farmquestHV / propertyHV / DailyQuest / cafeQuestHV` —— 5.5.0 内容更全，**严禁降级**。
- 所有基础 .dat 三版均加密，**旧版没有明文版**可借。

**无价值（明确排除）：** 全部 `*iPhone` 图集/索引（5.5.0 已有 iPad 高清版或自带 iPhone 版）、v1.1.5 的 `*_leftdown_*.png` + `*_config.xml`（废弃美术管线）、`ShopItems.dat` / `120_0.dat` / `xiaotulv_*`（玩法已删或格式不通用）。已知的 voyage_* 黄金岛/加勒比美术按要求未再排查。
