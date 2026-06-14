# 摩尔庄园 touchHLE → 浏览器 WASM 移植路线图

> 分支:`wasm-poc`(仅此分支,永不并入 main,五平台零影响)
> 目标:摩尔庄园 IPA 在桌面 Chrome 完整可玩。这是**多月工程**,分阶段推进,每阶段独立 commit + 可演示链接。

## 阶段状态

| 阶段 | 内容 | 状态 | 演示 |
|---|---|---|---|
| Gate 0 | 解释器能否编 wasm + 性能 | ✅ **完成** | `wasm-poc/web/index.html`,Chrome 130-150 MIPS |
| Phase 0 | wasm-poc 分支 + PoC 迁入 | ✅ **完成** | 同上(本地 http 服务) |
| M0 基线 | 同步 touchHLE 当前源码到分支 | ✅ **完成** | — |
| M0 | 真实 crate **lib 编译到 wasm** | ✅ **核心达成** | touchHLE.wasm 55MB 产出 |
| M0.5 | **binary 链接 → 可加载 .wasm+.js** | ✅ **达成** | touchHLE.js 564KB + .wasm 76MB |
| M1 | guest 跑起来(协程 fibers/JSPI + mem 手术 + 主循环反应堆化) | ✅ **达成** | boot 打到 UIApplicationMain + app delegate |
| M2 | 标题画面第一帧(GLES1→WebGL2) | ✅ **达成** | **浏览器渲染出淘米 logo→摩尔庄园海洋标题画面**(全彩纹理/几何全通) |
| M3 | 音频+输入+GL 补全 → 进村可玩 | ⏳ | 浏览器进村交互 |
| M4 | IPA 加载+存档+联机+公开 URL | ⏳ | 公开 URL 完整可玩 |

## M1→M2 大突破(2026-06-13):5 个 wasm 专属根因修复,boot 打到 GL 渲染

无头调试主回路:`emulator/run-headless.py`(playwright + SwiftShader WebGL2 + /log 服务 + 截图)。
boot 链已打通:wasm init → 解密 IPA 加载 → Mach-O/dyld → ObjC 运行时(类+category 全注册)→
**静态初始化器全跑完 → UIApplicationMain → app delegate**(Flurry/Taomee analytics/keychain 全 faked)→
**GL ES1.1 via WebGL2 上下文创建成功(攻破 M0.5 的 GL 墙)→ splash 首帧通过 WebGL2 渲染**。

五个根因(每个单独 commit,均 cfg/target 门控、五平台零回归):
1. **★mach_object `read_uleb128` 用 usize 累加**:32 位 Mach-O 的 dyld bind 流有 ≥6 字节 ULEB,
   wasm32 上 `n << bits`(bits≥32)被掩码 mod 32 → bind 地址全算错 → ObjC category 的 cls 绑不上
   崩。治本=vendor mach_object 改 u64 累加(`vendor/mach_object`)。这是最大根因,撤掉了所有 null 旁路。
2. **静态初始化器 SP / 线程栈区上界**还钉死 4GiB 顶,没随 mem 手术重定位到 1GiB 窗口 → 越界。
3. **emscripten fiber 的 C 栈没 16 对齐**(`vec![0u8]` 对齐=1)→ 协程栈上 SDL 的 EM_ASM 参数缓冲
   触发 `assert(buf%16==0)` abort。改 `Box<[u128]>`。影响 12 个 on_parent_stack 点(含 GL 上下文)。
4. **webgl2 GET_PARAMS 表不全** → 游戏查 `GL_MAX_MODELVIEW_STACK_DEPTH` panic。补表+未登记不崩。
5. **debug 下 copy_nonoverlapping「对齐」UB 误报**(guest 指针对 host 不对齐,memcpy 本就安全)→
   wasm target 加 `-C debug-assertions=off`。

**当前墙(M2 继续)**:splash 渲染后,app 继续初始化(到 keychain 读写)时撞 emscripten asyncify
`Assertion failed: id 27 not found in callStackIdToFunc`(fiber rewind 时 call-stack-id 对不上)。
=fiber + asyncify 状态管理的深层问题,可能要调 asyncify 栈管理或转 JSPI。

## M0.5 实测:真实模拟器在 Chrome boot(2026-06-13)

构建完整模拟器(`wasm-poc/emulator/build-emulator.sh`)+ 浏览器加载(`loader.html`),实测:
**真实 touchHLE 模拟器 wasm 在 Brave/Chrome 里启动**——打印 touchHLE 启动 banner、解析参数、
base path、进 app picker、输出诊断块(`操作系统: emscripten (wasm32) · CPU: wasm32 1核`)。

**第一道运行期墙 = GL 上下文创建**(`src/gles.rs:160` panic "Couldn't create OpenGL ES 1.1
context"):游戏要 ES1.1(EGL_BAD_CONFIG)或 GL2.1-compat(context attributes not supported),
WebGL 两者都不给。→ 这正是 **M2 的 GLES1→WebGL2 着色器后端**(需新写第三个 GLES 实现请求
WebGL2/ES3 上下文 + 按状态生成 GLSL ES 着色器模拟固定管线)。主机侧 init 全通,协程/mem 墙
在 GL 之后才会撞到。

## M0 关键发现(2026-06-13)

试编 `wasm32-unknown-unknown` 暴露的错误面**高度集中**:
- **2641 个错误中 2638 个是同一根因**:`sdl2-sys` 的 bindgen 生成代码引用 `libc::c_int`/`c_char` 等,
  而 `libc` crate 在 `wasm32-unknown-unknown` 下不提供这些 C 类型。
- 其余依赖(corosensei、3 个 C wrapper、symphonia、aes 等纯 Rust)**编译期都过了**——
  corosensei 编译期没挡路(研究曾担心,实测可编;运行期栈切换才是问题,留 M1)。
- 唯一已修:`getrandom` 在 wasm 需 `js` feature(uuid v4 依赖),已 target-gate。

**结论**:wasm 移植的真实工作量 = 替换 SDL2/OpenAL/stb/pvrt **平台层**,不是核心逻辑。
核心(objc/mem/dyld/frameworks/libc 逻辑)是纯 Rust,大概率能编。

## 路线决策:目标三元组用 emscripten

两条路:
- **`wasm32-unknown-unknown`**:必须把 SDL2/OpenAL/stb/pvrt 全撕掉、用 web-sys 重写平台层
  (2638 错误的真实代价 + 协程要自建调度器)。干净但工作量大一个量级。
- **`wasm32-unknown-emscripten`(选定)**:emscripten 提供 SDL2/OpenAL ports + libc + POSIX
  socket→WebSocket + fibers,让现有 sdl2/openal-using 代码**几乎原样编**。研究推荐路线。
  需 emsdk 工具链 + 锁定 rustc↔emsdk 版本对。

CPU 后端两条路都用碎出验证过的**纯 Rust 解释器**(`--features cpu_interpreter`,替 dynarmic JIT)。

## 三大已知硬前置(来自调研,M0-M1 攻坚)

1. **4GiB 内存**:`mem.rs` 的 `[u8; 1<<32]` 单体数组 wasm 装不下 → 收缩 guest 地址窗口 + 主栈重定位。
2. **协程**:`corosensei` 无 wasm 运行期后端 → emscripten fibers 或 JSPI。
3. **GLES1 固定管线**:`gles1_on_gl2.rs` 是固定管线直转发、web 报废 → 新写 `GLES1OnWebGL2` 着色器后端
   (游戏仅 import 65 个 GL 函数、无光照,着色器排列小)。

## 当前可演示(Gate 0)

```bash
cd wasm-poc && ./build.sh                       # 构建解释器基准 wasm
python3 -m http.server 8780 --directory web     # 本地服务
# 打开 http://127.0.0.1:8780/index.html → 浏览器实测 130-150 MIPS
```

## 浏览器要求(当前 / 目标)

- **当前(Gate 0 基准)**:任意支持 WebAssembly MVP 的桌面浏览器(Chrome/Edge/Brave/Firefox/Safari)。
- **目标(完整移植)**:Chromium 内核桌面浏览器(Chrome/Edge/Brave);需 WebGL2;
  联机需 WebSocket;若用 SharedArrayBuffer(线程/音频低延迟)需 COOP/COEP 跨源隔离头。

## 五平台安全纪律

- 所有 wasm 改动 `#[cfg(target_arch="wasm32")]` / target-gate,永不影响 mac/win/linux/android/ios。
- 每个独立修改单独 commit(中文)。绝不 `git add -A`(主仓有敏感二进制/IPA/逆向文档)。
- 每阶段跑五平台构建确认零回归(M0 完成后接 CI)。
