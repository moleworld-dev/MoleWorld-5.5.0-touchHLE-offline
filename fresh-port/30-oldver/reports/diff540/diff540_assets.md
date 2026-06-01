# 摩尔庄园 5.4.0 ↔ 5.5.0 文件/资源 Micro-Diff(夏季海洋更新)

> 目的:对比 5.4.0(末版前一版)与 5.5.0(夏季海洋更新),弄清末版补丁增删了哪些本地资源,
> 并找出"5.4.0 本地自带、5.5.0 缺失且仍被引用"的反向移植恢复候选(继上一轮从 2.4.3 补 11 MP3 + 成就图集之后)。
> 全程只读,不拷贝任何游戏文件。

## 数据来源
| 版本 | 来源 | 文件数 |
|---|---|---|
| 5.4.0 | `30-oldver/v5.4.0/Payload/MoleWorld.app`(本次新生成清单 `reports/files_v5.4.0.txt`) | 2178 |
| 5.5.0 原始基线 | `reports/files_v5.5.0.txt`(列 = `大小\t相对路径`) | 2188 |
| 5.5.0 实时包 | `01-cracked/Payload/MoleWorld.app`(= 原始 + 上一轮 13 个回拷文件) | 2201 |

> 对比基线统一用 **原始 5.5.0 清单 `files_v5.5.0.txt`**,不是实时包,因此上一轮回拷的
> 11 个 MP3 + 成就图集**不会**被误判成"5.5.0 原生"。`files_v5.5.0.txt`(2188)经核对**不含**那 11 MP3 /
> achievementiPhone 图集,仅含 2 个旧的 `*.decoded.plist`(下文已单独剔除)。2201 = 2188 + 11 MP3 + 2 成就图集 ✅。

> 方法学说明:basename 集合 diff 与**完整相对路径** diff 结果**完全一致**(都为 6 删 / 16 增),
> 说明两版之间**没有"换目录改名"**的资源,basename 结论可信。

---

## 一、Basename / 路径 集合 diff 总览

| 方向 | 数量 | 含义 |
|---|---|---|
| 5.4.0 有、原始 5.5.0 无 | **6** | 末版删掉/改名的本地资源(潜在恢复候选) |
| 原始 5.5.0 有、5.4.0 无 | **16** | 海洋更新新增的本地资源(含 4 个垃圾文件) |
| 两版同路径但内容不同(大小变化) | **52** | 末版改写内容、文件名不变(美术刷新 / 数据表更新) |

---

## 二、5.4.0 有 / 5.5.0 删掉(6 个)—— 恢复候选评估

| 相对路径 | 5.4.0 大小 | 扩展名 | 5.5.0 二进制是否引用 | 结论 |
|---|---|---|---|---|
| `OpenTreasureChest.plist`  | 1577    | .plist   | **否(字符串完全消失)** | ❌ 不值得拷 |
| `OpenTreasureChest.pvr.ccz`| 159891  | .pvr.ccz | **否** | ❌ 不值得拷 |
| `VoteActivity.plist`       | 2544    | .plist   | **否(字符串完全消失)** | ❌ 不值得拷 |
| `VoteActivity.pvr.ccz`     | 130465  | .pvr.ccz | **否** | ❌ 不值得拷 |
| `SF_Info/MoleWorld.sinf`   | —       | .sinf    | —(FairPlay DRM) | ⛔ 非游戏资源 |
| `SF_Info/MoleWorld.supp`   | —       | .supp    | —(FairPlay DRM) | ⛔ 非游戏资源 |

### 关键判定(与上一轮 MP3 恢复逻辑相同的引用验证法)
上一轮能恢复 MP3,是因为 5.5.0 二进制 / `sound.plist` **仍引用**那些文件名,只是文件被改成服务器下载、没打进离线包 ——
属于"逻辑还在、资源缺位"。本轮这 4 个图集**恰恰相反**:

- **5.4.0 二进制**含硬编码字符串 `OpenTreasureChest.plist`、`VoteActivity.plist`(实际加载过)。
- **5.5.0 二进制完全没有这两个字符串**(`strings | grep` 零命中,连 `%@.plist` 拼接也对不上这两个 stem)。
- 5.5.0 里虽仍有 `openTreasureChestTimes` / `parseNewYearVoteActivityStatus:` 等**残留 selector**,但那是
  活动**数据字段/网络解析**的遗留,**不再加载对应图集**。

> 即:**开宝箱(OpenTreasureChest)与投票活动(VoteActivity / 新年投票)这两个活动在 5.5.0 的代码里已被移除**,
> 图集随之正确删除。把它们拷回 5.5.0 离线包 = **纯死重量,没有任何代码会加载,等于自欺**。
> **本轮无新的有效恢复候选。**

实时包确认:`01-cracked` 里也**不存在**这 4 个文件(未被回拷),与上述结论自洽。

---

## 三、5.5.0 新增 / 5.4.0 无(16 个)—— 海洋更新本地新增

### 3.1 真实海洋更新资源(12 个,均被 5.5.0 二进制引用)
| 相对路径 | 扩展名 | 功能 | 二进制引用证据 |
|---|---|---|---|
| `seabedseekingtreasure.plist`     | .plist   | **海底寻宝**图集(夏季海洋更新核心玩法) | `getSeabedSeekingTreasureActivityInfo`、`seabedSeekingTreasureActivityFlag`、`seabedseekingtreasure.plist` 字符串均在 |
| `seabedseekingtreasure.pvr.ccz`   | .pvr.ccz | 同上(纹理) | 同上 |
| `SOUND_242.mp3` ~ `SOUND_247.mp3` | .mp3 ×6  | 海洋更新新增音效 | 5.4.0 无此编号;`sound.plist` 仅索引 22 个旧音效(最高 SOUND_129),新音效由运行时直接播放 |
| `31198_aspriteiPad.dat` / `.png`  | .dat/.png| 编号 31198 角色/物件序列帧图集 | 二进制含 `31198_asprite` 字符串 |
| `share6_scene_aspriteiPad.dat` / `.png` | .dat/.png | 第 6 套分享场景序列帧图集 | 二进制含 `share6_scene_asprite` 字符串 |

> 这 12 个都是 5.5.0 **原生新增**且**已正确打进离线包**(`files_v5.5.0.txt` 基线里就有,无需处理)。

### 3.2 需剔除的非原生 / 垃圾文件(4 个)
| 相对路径 | 性质 | 说明 |
|---|---|---|
| `xiaotulv_userinfo.decoded.plist`        | 上一轮产物 | 之前解档调试解出来的明文 plist,**非 5.5.0 原生** |
| `xiaotulv_winter_userinfo.decoded.plist` | 上一轮产物 | 同上 |
| `hs_err_pid85229.log` | 运行时垃圾 | JVM/HotSpot 崩溃日志,误打进包 |
| `hs_err_pid85276.log` | 运行时垃圾 | 同上 |

> 这 4 个不是海洋更新引入的游戏资源,统计"真实新增"时应排除。

---

## 四、同路径内容变化(52 个,文件名不变)

末版把这 52 个文件改了内容(大小变化),**文件名/路径不变**,属于常规版本刷新,与"资源增删"无关,
列此仅供完整性参考。按扩展名:

| 扩展名 | 数量 | 代表文件 |
|---|---|---|
| .png    | 24 | App 图标全套(Icon-*、Icon@2x)、`logoiPad/iPhone-hd/iPhone5`、`new_feature_1~4`、`new_feature_background`、`scenesRoadItemsiPad`、`scenesShareStore2iPad` |
| .plist  | 10 | `discountiPad`、`forecastiPad`、`buttons_local*`(zh-Hans)、`scenes*`、`LightPosi*`、`registeriPad`(注:registeri* 同 basename 多目录) |
| .dat    | 8  | `property.dat`、`propertyHV.dat`、`property_description(HV).dat`(多语言目录) |
| .pvr.ccz| 5  | `discountiPad`(10万→156万,折扣活动图集大改)、`forecastiPad`、`buttons_local*`(zh-Hans 3 个) |
| .strings| 2  | `Localizable.strings`(zh-Hans / zh-Hant) |
| 其他    | 3  | `MoleWorld`(主程序 12.20M→12.25M)、`iTunesArtwork`、`_CodeSignature/CodeResources` |

> 几个偏大的变化:`discountiPad.pvr.ccz` 100KB→1.56MB(折扣/促销界面大改),`logo*` 三张全部增大(新 logo),
> `new_feature_*` 全部刷新(新版本介绍图)。这些都不是恢复对象。

---

## 五、结论

1. **末版(5.5.0 海洋更新)资源增删概况**:相对 5.4.0,**删 6 / 增 16 / 改 52**。
   - 删的 6 个里:4 个是被砍掉活动的图集(开宝箱 OpenTreasureChest、投票活动 VoteActivity),2 个是 DRM。
   - 增的 16 个里:**12 个真海洋资源**(海底寻宝图集 `seabedseekingtreasure.*`、6 个新音效 `SOUND_242~247`、
     `31198_asprite`、`share6_scene_asprite`),另 4 个是上一轮 decoded plist + JVM 崩溃日志(垃圾,需剔除)。
   - 改的 52 个是美术/数据表常规刷新(图标、logo、折扣图集、property 数据表、本地化),非恢复对象。

2. **是否有新的"5.4.0 本地有、5.5.0 缺且被引用"恢复候选?——没有。**
   唯一的 5.4.0 独占游戏资源是 `OpenTreasureChest.{plist,pvr.ccz}` 和 `VoteActivity.{plist,pvr.ccz}`。
   与上一轮 MP3 不同(那些在 5.5.0 仍被引用、只是没打包),**这两组活动在 5.5.0 代码里已彻底删除**
   (二进制零字符串引用),拷回去没有任何代码加载 = 死重量。**结论:不值得拷,本轮没有可移植项。**

3. **排除回拷文件后的两版资源差异本质**:
   把上一轮回拷的 13 个文件(11 MP3 + achievementiPhone 图集 .plist/.ccz)和 4 个垃圾文件都剔除后,
   5.4.0→5.5.0 的**净本地资源变化 = 删 4(两个停办活动的图集) + 增 12(海底寻宝玩法 + 配套音效/序列帧图集)**。
   换言之,海洋更新在离线包层面就是**用"海底寻宝"一套活动替换了"开宝箱 + 投票"两套老活动**,
   外加常规美术/数据刷新。整体高度稳定,**不存在像上一轮那样"5.5.0 缺、5.4.0 全、可一键补"的资源缺口。**

---

## 附:本次产出文件
- `reports/files_v5.4.0.txt` —— 本次新生成的 5.4.0 完整文件清单(2178 条,`大小\t相对路径`,与基线同格式)
- 本报告 `reports/diff540/diff540_assets.md`
