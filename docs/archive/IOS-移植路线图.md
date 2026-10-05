# 摩尔庄园 5.5.0 — iOS 移植路线图(feat/ios-interpreter 分支)

> 本轮目标:打地基 + 探路。结论已就绪,且**比预期乐观得多**。
> 撰写:2026-06-10 · 分支:`feat/ios-interpreter` · 基底:moleworld-dev main(dynarmic-only,桌面/Android 可玩)

---

## 0. 一句话结论(重大更新)

**iOS 可行性已被真机证实,不是理论推演。** 无 JIT 的纯 Rust ARMv7 解释器(`cpu_interpreter`)**已经在真机 iPhone 16 Pro Max(A18 Pro,TXM 设备)上把摩尔庄园渲染出画面**——无 JIT、无 entitlement、无调试器附加、无越狱(证据见 `LOG`:`EAGLContext FPS` 计数器活跃、150 帧采样)。**唯一剩下的拦路虎不再是"能不能跑",而是"跑得太慢":~2 FPS,需要约 15–30× 提速才可玩。**

→ 整个项目的剩余工作从"**让它跑起来**"变成了"**让它跑得快**"。这是性能工程问题,不是可行性问题。

---

## 1. 已证实的能力(真机实证)

| 能力 | 证据 |
|---|---|
| 无 JIT 在 TXM 真机启动运行 | `mw-ios-run.sh` 普通 `devicectl launch`(无 debugserver/CS_DEBUGGED)对 iPhone 16 Pro Max |
| 加载游戏 + 系统 dylib | `LOG`:Loading armv7 slice for MoleWorld / libstdc++ / libgcc_s |
| 跑通 ObjC + cocos2d 场景图 | `LOG` [MOLE-PROF]:25,777 objc_msgSend/sec,热点 selector = zOrder/compare:/draw/visit/transform(cocos2d 渲染管线) |
| **渲染出帧** | `LOG`:`EAGLContext FPS: 0.53~120`(150 采样),splash + present_frame OK |
| 指令完整度高 | 整个 run 仅 15 条 UNIMPL/DERAIL,且不阻塞渲染 |
| 4GiB 内存模型在 iOS 可用 | 真机跑到渲染 = `mmap(4GiB)` 惰性回填成功,**无需 extended-virtual-addressing entitlement** |

---

## 2. 真正的拦路虎:性能(~2 FPS)

`LOG` 显示 cocos2d 场景渲染时持续 ~1.7–3.3 FPS,profiler 实测 **25,777 objc_msgSend/sec**。根因在代码里已定位(均为高杠杆、可做):

### 2.1 解释器没有预解码缓存(#1 杠杆,未实现)
- `src/cpu/interpreter/mod.rs:499 step_one()`:**每执行一条指令都重新 fetch + 解码**(thumb/thumb2 变长判断、读内存),没有 PC→已解码指令的缓存。
- `InterpreterCpu` struct 末尾(`mod.rs:86`)只有一行注释 `// P1: ITSTATE cache + PC->decoded-instruction cache.` —— **缓存字段根本没建**。
- `invalidate_cache_range()`(`mod.rs:150`)是 `P0: no-op`。
- **影响**:每条热循环指令重复解码。实现"地址→已解码 handler"缓存(dyld 改写时按范围失效)通常一把 **3–10×**。

### 2.2 热循环里有逐指令调试埋点(应 cfg 关掉)
- `step_one` 每条指令都执行:`dbg_n += 1`、写 trace 环形缓冲(`mod.rs:539-547`)、每 ~4M 条心跳打印。
- struct 里 `trace[64]`、`dbg_n`、`dbg_last_pc/insn` 是纯 P1 调试开销。
- **影响**:在 ~2 FPS 这种已经吃紧的热路径上,这些 bookkeeping 该用 `#[cfg(feature="interp_debug")]` 或 release 编译关掉。预计 **1.2–1.5×**。

### 2.3 objc_msgSend 派发开销(25.7K/sec)
- cocos2d 场景图每帧大量 `release/zOrder/compare:/draw/visit/transform`。
- **杠杆**:msgSend 内联缓存(selector→IMP 按 (isa,sel) 缓存)、减少 retain/release 抖动。中等杠杆。

### 2.4 其它常规解释器优化
- lazy NZCV 标志(只在被读时求值)、线程化分派(computed-goto 近似)、VFP/NEON 路径优化。

---

## 3. 性能优化路线(按杠杆排序 = 下一轮主线)

| 优先级 | 工作 | 预期 | 文件 |
|---|---|---|---|
| **P-1** | **PC→已解码指令缓存**(decode-once) | 3–10× | `mod.rs` struct + `step_one` + `invalidate_cache_range` |
| **P-2** | 逐指令调试埋点 cfg 化关掉 | 1.2–1.5× | `mod.rs:539-547` + struct trace/dbg 字段 |
| **P-3** | objc_msgSend 内联缓存 | 1.3–2× | `src/objc/messages.rs` |
| **P-4** | lazy flags + 线程化分派 | 1.2–1.5× | `interpreter/{arm,thumb*}.rs` |
| 验证 | 全程 macOS 上对 dynarmic 跑 `diff.rs` 差分,保证优化不破坏正确性 | — | `interpreter/diff.rs` |

> 叠乘估算:P-1×P-2×P-3 ≈ 5–25×,有望把 ~2 FPS 推到接近可玩(目标客户机本是 412–600MHz,A18 余量充足)。

---

## 4. 内存模型决策(Phase D — 已解决)

**保持 4GiB 扁平 `mmap`,不缩减、不加 entitlement。** 真机已证实 `mem.rs:210` 的 `[u8; 1<<32]` 在 iOS(A18/8GB)上惰性 mmap 成功并跑到渲染。`extended-virtual-addressing` entitlement **不需要**(与社区 Limon 一致)。后续若遇低内存机型 jetsam,再考虑缩到 1GiB,但当前非阻塞。

---

## 5. 脚手架审计(Phase E)

**已就位的 iOS 分支(免费复用)**:`bin.rs`(SDL_UIKitRunApp)、`lib.rs ios_entry`、`build.rs`(16 framework 链接)、`window.rs`(默认 FBO/RBO、gl_proc_ios_fallback、NPOT CLAMP)、`gles/{gles1_native,present}.rs`、`paths.rs`、`core_animation/composition.rs`、`ios-xcode/`(MoleWorldHD 工程)、`make-ios-ipa.sh`(本轮已切无 JIT)、`mw-ios-run.sh`(无 JIT 真机回路)。

**待补的小缺口**:
- `src/audio/openal_soft_wrapper/build.rs`:**无 iOS 分支**(仅 android/macos)。需加 iOS 链接 AudioToolbox/CoreAudio/CoreFoundation(照抄 macos 分支)。
- `src/mole_sysinfo.rs`:无 iOS 分支(仅诊断用,低优先,加 `#[cfg(target_os="ios")]` 走 libc::uname/UIDevice)。

**SDL2 已提供 iOS 壳**:UIApplication/CADisplayLink/EAGLContext/触摸全由 SDL2(fork tag touchHLE-3)承担,无需原生重写 window.rs。

---

## 6. 签名 / 分发(本轮已对齐)

- **无 JIT → 无需任何特殊 entitlement**:`make-ios-ipa.sh` 已删除 allow-jit / allow-unsigned-executable-memory / dynamic-codesigning / disable-library-validation,entitlements 留空。
- 构建命令已改 `--no-default-features --features static,cpu_interpreter`(原默认 dynarmic = "闪一下"根因)。
- 分发:普通开发证书 / ad-hoc / AltStore-SideStore 重签即可,**A17+/iOS 26 直接侧载**,不需调试器/越狱。

---

## 7. 到"可玩"的路径(下一轮)

1. **P-1 预解码缓存**(最高杠杆)→ macOS 上对 dynarmic 跑 diff 验正确性 → mw-ios-run.sh 真机测 FPS。
2. P-2 调试埋点 cfg 化 → 再测。
3. P-3 msgSend 内联缓存 → 再测。
4. 补 openal iOS 链接分支(确保真机音频)。
5. 逐项 P-4 + 逐场景调通(村庄/农场/商店/小游戏),目标稳定 ≥30 FPS。

---

## 附:本轮(feat/ios-interpreter)已完成

- ✅ 抢救 6326 行无 JIT 解释器到 GitHub(此前仅本地未提交,有丢失风险)。
- ✅ 隔离新目录 `MoleWorld-iOS-experimental/`(与 `摩尔庄园 5.5.0/` 物理隔离),分支 `feat/ios-interpreter`。
- ✅ iOS 构建切无 JIT:make-ios-ipa.sh 去 JIT entitlement + cpu_interpreter 构建命令。
- ✅ 探明真实状态:**真机已渲染,瓶颈是性能而非可行性**;定位 #1 杠杆 = 预解码缓存。
- ✅ 内存决策(4GiB 可用)+ 脚手架审计 + 本路线图。
