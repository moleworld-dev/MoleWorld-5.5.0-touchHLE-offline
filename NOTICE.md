# NOTICE — 版权与许可说明

本仓库混合了**开源代码**与**第三方版权资源**,适用不同条款:

## 1. 代码(MPL-2.0)
- `fresh-port/20-touchHLE-src/touchHLE/`(touchHLE 模拟器 + 本项目对它的全部改动,包括 `src/mole_cheats.rs`、`src/mole_menu.rs`、`src/mole_diag.rs`、`src/save_reset.rs` 及各框架补丁)遵循 **Mozilla Public License 2.0**(见根目录 [LICENSE](LICENSE))。这是 touchHLE 上游的许可证,本 fork 沿用。
- 我们新增/修改的源码文件均带 MPL-2.0 头。

## 2. 第三方依赖(各自许可)
- `fresh-port/20-touchHLE-src/touchHLE/vendor/`(SDL、dynarmic、OpenAL Soft、stb 等)各自遵循其上游许可证,见各子目录内的 LICENSE/COPYING。
- 打包进 macOS .app 的字体(`touchHLE_fonts/`,如 Liberation、Noto Sans SC)与动态库(`touchHLE_dylibs/`)遵循各自许可证。

## 3. 游戏本体与资源(淘米版权 —— **不在 MPL 覆盖范围内**)
- 仓库内的 **IPA 文件**(`ios-packages/*.ipa`)、解包的游戏包(`fresh-port/01-cracked/`)、旧版逆向素材与研究产物(`fresh-port/30-oldver/`、`fresh-port/02-ida/`、`fresh-port/03-objc/` 等)中的**游戏代码、美术、音频、数据**,**版权归淘米(Taomee)所有**。
- 这些内容仅用于**个人怀旧、技术研究与数字存档**,**严禁任何商业用途**。
- 《摩尔庄园移动版/豪华版》已于 **2015 年停止运营、从 App Store 下架**,服务器关闭,无任何官方在售或下载渠道。
- **如版权方提出异议,将立即移除相关内容。**

## 4. 修改器/作弊功能
仅用于**离线单机**(服务器已停运),不涉及任何在线作弊或对他人的影响。
