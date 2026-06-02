# 摩尔庄园HD 移动版 现代化离线移植（基于touchHLE）

![摩尔庄园 5.5.0 在 touchHLE 上离线运行（Apple Silicon Mac）](demo.png)

把 2015 年已停运下架的《摩尔庄园移动版》（安卓叫《摩尔庄园豪华版》，**2D 平面模拟经营**，非现在的 3D 新版）的最后一个版本 **5.5.0（夏季海洋更新）**，通过 [touchHLE](https://touchhle.org)（Rust 写的 iOS 高层模拟器）**搬到 macOS / Windows / Linux / Android 等现代系统上离线游玩**（iOS 原生版移植进行中）。

> 这是一台 32 位 ARMv7 的 cocos2d-iphone 老游戏。官方服务器已彻底关停，本项目目标是**不依赖任何服务器、纯离线**把单机部分跑起来、并把原作者真机越狱修改器（贝壳/数值/解锁等）的能力复刻进模拟器。

---

## 🎯 项目核心诉求

1. **离线可玩**：服务器死了，让村庄、种田、小游戏、商店、升级、存档这些**单机循环**在 touchHLE 上完整跑起来。
2. **复刻修改器**：把真机 Substrate tweak 的功能（免费贝壳、数值加成、强制 VIP、全解锁、一键收获、调试菜单…）用原生 Rust 在模拟器里重做，按 **T 键**召出菜单。
3. **抢救资源**：服务器下载的内容离线缺失，尽量从旧版本（1.1.5 / 2.4.3 / 5.4.0）反向找回能补的（音乐、图集等）。

---

## 🟢 能用（离线已验证）

- **启动进村庄**：开机动画 → 标题 → 村庄场景正常渲染。
- **核心经营**：种田/收获、建造、装饰、房间、商店（已改为**免费贝壳**，跳过死掉的内购）。
- **6 个本地小游戏**：切水果、钓鱼、挖矿、涂鸦、犁地、洗澡（重力感应分拣）。
- **存档**：本地 NSKeyedArchiver + AES 存档可正常读写（修过进游戏解档崩溃）。
- **丝尔特试玩村（xiaotulv）**：选村界面内置、不联网，可直接浏览（春/冬两套地图）。
- **内置修改器菜单（按 T）**：
  - 数值：经验/摩尔豆/贝壳/VIP值/食物/奖励券/时间/任务进度 ±（走游戏自带 `TestLayer`）
  - 开关：购物免费、金币 x10、经验 x10、**强制 VIP（已修）**、关反作弊、作物瞬熟、永不枯萎、冷却归零、建筑瞬完成
  - 解锁/成就/收获：全物品解锁、全成就显示、魔法密码任意过、设头像、设奖励券、一键收获全部地
  - 召唤本地道具/UI 层（超级贝壳树、水塔、村庄菜单、新版商店…）
- **离线美术修补**：从 2.4.3 反向移植了 **11 个服务器下载的缺失 BGM/音效** + **成就奖章图集**（`sound.plist`/`achievementiPhone.plist` 实证引用，零回归）。
- **旧式 UIView 动画**：弹窗/转场的 begin/commitAnimations 已实现（不再硬切）。
- **黄金岛入口**：点了**干净退回村庄**（不再卡死）。

## 🔴 不能用（离线天然受限，非代码 bug）

- **17 个季节联网活动**：加勒比黄金岛、爱丽丝、史莱克、火焰之战、海底寻宝、世界杯竞猜、放风筝、青团、复活节… 这些活动的**美术是运行时从服务器下载的、本地根本不存在**，且要联网校验状态。离线渲染不出来。菜单里这些召唤项已标 **`(弃用)`**。
- **神算子（占卜）小游戏**：5.5.0 改成了**联网扭蛋**（概率表服务器下发），离线会等服务器。
- **多人/好友/UGC 社交、排行榜、广告墙**：纯服务器功能。
- **「等级不涨」（靠经验自然升级）**：根因是 5.5.0 新增的 `curLevel` XOR 混淆逻辑在 touchHLE 运行时层的处理问题（**不是数据/版本差异**，已用 1.1.5/2.4.3/5.4.0 四版反汇编对比坐实）。**临时绕过**：修改器里直接设等级。
- **内购（IAP）**：已停，贝壳改为本地免费发放。

## 📊 完成进度

| 模块 | 状态 |
|---|---|
| 启动 / 村庄 / 渲染 | ✅ 可用 |
| 种田 / 建造 / 商店 / 存档 | ✅ 可用 |
| 6 本地小游戏 | ✅ 可用 |
| 修改器菜单（9 开关 + 数值 + 解锁/成就/收获） | ✅ 可用 |
| 离线音乐/成就图修补 | ✅ 已补 |
| 丝尔特试玩村 | ✅ 可浏览 |
| 季节联网活动 / 神算子 / 社交 | ❌ 离线无解（缺服务器美术+校验） |
| 经验自然升级（等级不涨） | ⚠️ 运行时层 bug，有作弊绕过 |

**一句话**：单机核心循环离线可玩；服务器内容受限于"美术/数据本就在服务器、本地没有"，无法离线复活。

---

## 🛠️ 支持情况 / 怎么跑

**支持平台**：macOS（Apple Silicon）、Windows x64、Linux x64、Android arm64 —— 这四个平台都提供**开箱即玩**的发布包（游戏已内置进包里，**点击即玩**，无需自己找 IPA）。iOS 原生版**移植进行中**(详见下文)。

发布包从 GitHub Releases 页下载：<https://github.com/Shad0w23333/MoleWorld-5.5.0-touchHLE-offline/releases>

### 🟢 开箱即玩（下载即跑，已内置游戏）

- **🍎 macOS（Apple Silicon）**：下载 `.zip` → 解压 → **右键**「摩尔庄园.app」→「打开」（第一次需这样过 Gatekeeper，之后双击即可）→ 进游戏。
- **🪟 Windows x64**：下载 `.zip` → 解压 → 双击 **`Run-MoleWorld.bat`** → 进游戏。
- **🐧 Linux x64**：下载 `.tar.gz` → 解压得到一个文件夹 → 进文件夹双击 **`启动游戏.sh`**(KDE/XFCE 会弹「运行 / Run」；**GNOME 需右键 →「以程序运行 / Run as a Program」**，因为 GNOME 双击 `.sh` 默认只会用文本编辑器打开、不会运行)；也可在终端里 `./启动游戏.sh`。想要桌面/菜单图标(GNOME 最省心)就运行同目录的 **`安装到应用菜单.sh`**;详细说明见包内 **`如何运行.txt`**。需要系统装有 OpenGL / SDL2 运行库。
- **🤖 Android arm64**：下载 `.apk` → 安装(debug 签名,需在系统里允许「未知来源 / 安装未知应用」)→ 直接进游戏(游戏已内置进 apk,**点击即玩**)。

所有平台进游戏后,按 **T** 键召出修改器菜单。

### 🧪 iOS（原生 arm64,移植进行中 / 实验性,尚未发布）

基于 touchHLE 的 iOS arm64 构建。目前在 **Apple Silicon Mac 的 PlayCover** 上已经能安装、能启动、能跑到 GL 渲染阶段,但卡在 **GLES1.1 上下文**初始化(疑似 PlayCover / iOS-on-Mac 这套环境对古老 GLES1.1 的支持限制,**不是 IPA 本身的问题**);**原生 GLES1.1 的真机**验证尚未进行。该版本**还没有发布**,这里如实标注为「进行中」。

### 🔧 从源码构建（可选）

1. 进 `fresh-port/20-touchHLE-src/touchHLE/`,执行 `cargo build --release`。
2. **无需** `git submodule` 初始化 —— vendor 依赖已摊平为仓库里的普通文件;boost 由构建脚本自动下载。
3. 运行:`./target/release/touchHLE "<MoleWorld.app 路径>" --landscape-right --device-family=ipad`,或在 macOS 上用根目录的 `启动摩尔庄园.command`。游戏包在 `fresh-port/01-cracked/Payload/MoleWorld.app`(已含离线补的音乐/成就图)。
4. 游戏内按 **T** 召出修改器菜单。

> Windows / Linux / Android 三个平台的发布包由跨平台 CI(`.github/workflows/build-release.yml`)自动构建并附到 Release;macOS 的特制 `.app` 在本地打包后手动上传。

---

## 🔬 用了什么工具

- **[touchHLE](https://touchhle.org)** — 本项目的运行基座（Rust iOS 高层模拟器），本仓库是带摩尔庄园移植改动的 fork。
- **otool / nm / lipo**（Xcode 自带）— 反汇编、ObjC 元数据导出、胖二进制切片。
- **openssl** — 解密游戏数据表（**AES-128-ECB**，全版本通用 key = ASCII `39653543fa0d66aa`，`getEncrypKey` 前 16 字节）。
- **plutil / Python** — 解析 bplist 数据表（物品/任务/等级/音乐映射）。
- **cycript + Mach API**（历史，真机脱壳）— 老越狱设备上 `task_for_pid`+`vm_read_overwrite` 自进程脱壳。
- **自建无头验证 harness** — 持有脚本跑游戏 + 触摸注入 + framebuffer 截帧（PPM→PNG）。
- **多 subagent 逆向对比** — 1.1.5 / 2.4.3 / 5.4.0 / 5.5.0 四版的类/方法/物品/场景/小游戏差异分析（报告见 `fresh-port/30-oldver/reports/`）。

---

## 📁 仓库结构

```
.
├─ README.md  /  NOTICE.md  /  LICENSE        # 说明 / 版权 / MPL-2.0
├─ demo.png                       # 运行截图(README 顶图)
├─ ios-packages/                  # 五个版本原始 IPA（Git LFS）：1.1.5 / 2.4.3 / 5.4.0 / 5.5.0 / 5.5.0破解版
├─ .github/workflows/             # 跨平台发布 CI(Win/Linux/Android)
├─ 启动摩尔庄园.command           # mac 一键启动脚本
├─ screenshot/                   # 运行截图
└─ fresh-port/
   ├─ 01-cracked/                # 可运行的 5.5.0 游戏包（含离线补的音乐/成就图）
   ├─ 02-ida / 03-objc / 04-bridge / 10-recomp   # 逆向产物（类表/方法表/选择子/桥接数据）
   ├─ 20-touchHLE-src/touchHLE/  # touchHLE 源码 + 本项目改动（mole_cheats/mole_menu/… + 框架补丁）
   └─ 30-oldver/reports/         # 四版对比研究报告（差异/移除弃用/AES key/数据表）
```

> 本项目的主要改动文件：`fresh-port/20-touchHLE-src/touchHLE/src/` 下的 `mole_cheats.rs`（作弊拦截）、`mole_menu.rs`（T 键菜单）、`mole_diag.rs`、`save_reset.rs`，以及对 `objc/messages.rs`、`frameworks/uikit/ui_view.rs`、各 `frameworks/foundation/*` 的补丁。

---

## 🙏 致谢

本项目站在前人肩膀上，特别感谢：

- **哔哩哔哩 [@萌新迎风听雨](https://space.bilibili.com/411256864)** —— 提供安装包与思路。相关帖子：<https://www.bilibili.com/opus/1118433441251065897>
- **Never.** 的教程《记一次在老 iOS 设备上折腾"摩尔庄园移动版/豪华版（2015）"的经历》—— <https://dreamiao.com/2229/>（游戏身份/版本史/超级贝壳内购破解/存档与语言 bug 等背景知识）
- **[touchHLE](https://touchhle.org)** 项目及其作者 —— 没有这个 iOS 模拟器,这一切都无从谈起。
- 淘米《摩尔庄园》原作团队 —— 童年回忆。
- **GitHub [@EdmundDHow](https://github.com/EdmundDHow)** 🧧 —— 慷慨赞助了 **30 元人民币**,为这个纯爱发电的怀旧项目添了一把柴。这份心意我们记下了,谢谢你！

---

## ⚖️ 法律声明

- 本仓库的 **touchHLE 部分遵循其原始 MPL-2.0 许可**。
- 仓库内的**游戏 IPA / 资源版权归淘米（Taomee）所有**,仅用于个人怀旧、技术研究与存档,**不得用于任何商业用途**。游戏已于 2015 年停运下架、服务器关闭,无任何官方在售渠道。如版权方提出异议,将立即移除相关内容。
- 修改器/作弊功能仅用于**离线单机**,服务器已死,不涉及任何在线作弊。
