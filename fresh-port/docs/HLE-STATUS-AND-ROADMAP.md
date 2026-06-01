# 摩尔庄园 5.5.0 移植 —— 自写 HLE 引擎:状态与路线图

> 目标:把 32 位 ARMv7 的摩尔庄园 5.5.0(破解版)**1:1 移植**到现代 arm64 / iOS 26,
> **跑原版二进制、不重写、产物原生 arm64(无 JIT,能上最新 TXM 设备)**。
> 走法:自写 HLE —— 在原生 arm64 上解释执行原版 ARMv7 代码,框架调用转发到 iOS26 原生框架。

## 一、整体架构(已建成)

```
原版 ARMv7 Mach-O(破解版,已脱壳 cryptid=0,12MB)
   │  load_macho:按 vmaddr 映射段 + 解析 __cfstring 段 + 预留 guest 堆[top,+12MB)
   ▼
ARMv7/Thumb-2 解释器(capstone 解码 + 自写执行)
   │  整型全集 + IT条件块 + VFP浮点(s寄存器) + STM/LDM + 干净返回检测
   │  实例对象模型:[GameClass alloc] 在 guest 堆分配实例+写 isa
   ▼
objc_msgSend 拦截(靠 stub_map 认出桩地址)→ 三路混合派发:
   ├─ self 是游戏类(class_addr_map)   → 类方法 MPLUS→IMP,递归解释
   ├─ self 是游戏实例(isa∈class表)    → 实例方法 MMINUS→IMP,递归解释
   ├─ self 是框架对象(句柄表 NH)      → libffi 按类型签名编组 → 真原生 objc_msgSend
   └─ 导入 C 函数 → gl_shim(GL→CGL上下文) / c_shim(malloc→guest堆/mem*/ARC)
```

**关键设计**:
- **句柄表 NH**:32 位 guest 句柄(高位 0x80000000 标记)↔ 64 位真原生 `id`。框架对象/类用句柄表示,绝不把 64 位指针塞进 guest。
- **游戏类 vs 框架类判别**:dyld 绑定区分——外部 `bind`(`classref_map.tsv`)= 框架类→转发原生;内部 `rebase`(class_t 在 guest 内,`class_addr_map.tsv`)= 游戏类→解释 IMP。
- **参数编组**:`method_getTypeEncoding` 拿签名 → libffi 通用调用;`@`参数 = 句柄/`__cfstring`常量转 NSString/nil;int/float/指针各类型;结构体签名暂放弃。
- **渲染后端**:macOS 桌面 OpenGL(CGL 离屏 FBO,GL_VERSION 2.1 Metal),GLES1→桌面 GL 近 1:1 翻译。

## 二、已验证的能力(全部实跑,可核对)

| 能力 | 证据 |
|---|---|
| 加载 + Thumb 反汇编 | `disasm.cpp`;0xef40 序言与 IDA Hex-Rays 逐条对上 |
| 整型解释执行 | 栈/分支/PC相对/内存/IT/VFP/STM/LDM 全对 |
| **1:1 铁证** | 跑原版 `-[GameData getEncrypKey]` 逐字节拼出已知 AES key `39653543fa0d66aa` |
| ObjC 派发(类+实例) | 沿真实调用图递归:NewRelicAgent→Internal→NRNonARCMethods;CCDirector/EAGLView/GameData init |
| 框架转发(libffi 编组) | `[NSUserDefaults boolForKey:@"activation"]`=真NO、`[NSURLConnection connectionWithRequest:delegate:]`、`[UIWindow alloc]`/`[UIScreen mainScreen]` 真原生 |
| 实例对象模型 | 18 实例分配,17 实例 init 解释(GameData/GameSettings/Global/NetworkManager/CCDirector/EAGLView…) |
| C shim | malloc→guest堆、memcpy/memset/strlen、ARC retain/release |
| 渲染后端 | CGL 离屏清屏读回像素正确(255,0,0,255) |
| **真实启动序列执行** | didFinishLaunching 走真机路径:激活上报→窗口建立→引擎初始化,数千步零 UNIMPL/零崩 |

## 三、构建 & 运行

```bash
D="…/fresh-port/10-recomp"; B="…/fresh-port"; SDK=$(xcrun --sdk macosx --show-sdk-path)
# Mac Catalyst 构建(真 UIKit/Foundation + 桌面GL + libffi):
clang++ -std=c++17 -x objective-c++ -target arm64-apple-ios14.0-macabi -isysroot "$SDK" \
  -iframework "$SDK/System/iOSSupport/System/Library/Frameworks" -I/opt/homebrew/include \
  "$D/interp.cpp" -o "$D/interp_cat" \
  -L/opt/homebrew/lib -lcapstone -lffi -framework Foundation -framework UIKit -framework OpenGL
# 运行(参数:二进制 起始vaddr 步数 stub_map classref_map method_imp_map class_addr_map):
"$D/interp_cat" "$B/01-cracked/Payload/MoleWorld.app/MoleWorld" ef40 20000 \
  "$B/04-bridge/stub_map.tsv" "$B/04-bridge/classref_map.tsv" \
  "$B/04-bridge/method_imp_map.tsv" "$B/04-bridge/class_addr_map.tsv"
```

## 四、工件清单

- `10-recomp/interp.cpp` —— **HLE 引擎主体**(加载/解释/派发/转发/GL,~600 行)
- `10-recomp/disasm.cpp`、`bridge_poc.mm` —— Brick1 反汇编、Brick3b 转发机制 PoC
- `04-bridge/stub_map.tsv`(1915 桩→导入名)、`classref_map.tsv`(1394 框架classref→类名)、
  `method_imp_map.tsv`(36127 (类,sel)→IMP)、`class_addr_map.tsv`(1968 class_t→类名)、`parse_classes.py`
- `02-ida/out/`(IDA 全反编译:functions.txt 4万函数、decomp_key.c 等)、`/tmp/MoleWorld_cracked.i64`(易失)
- `01-cracked/`(破解版解包,主二进制已脱壳)

## 五、剩余路线图(精确,带已知阻塞)

### 立即阻塞(出第一帧的拦路)
1. **启动循环空转**:didFinishLaunching 在 ~0x4a776e 附近反复打转(20000 步仅 163 消息,GL 调用 0 次),怀疑某框架调用返回值导致等待/重试循环(如网络可达性轮询)。需定位该循环、给出让其退出的返回值。
2. **帧循环未驱动**:真正的绘制在 `CCDirector` 的 `CADisplayLink` 回调里;线性跑 didFinishLaunching 不会触发。需在启动后**手动驱动一帧**:定位 `[CCDirector drawScene]`/`mainLoop` 的 IMP,设置好 director→runningScene,从该 IMP 起跑解释器。
3. **renderbuffer 绑定**:ES1Renderer 用 `[EAGLContext renderbufferStorage:fromDrawable:layer]` 给 renderbuffer 配存储;Catalyst 无 OpenGLES,EAGLContext=nil。需**拦截该 selector**,改为对当前 CGL 上下文做 `glRenderbufferStorage(…,1024,768)`,并让 `glGetRenderbufferParameteriv` 返回 1024/768。

### 出第一帧后
4. **纹理上传**:guest 解码 PNG/PVR.ccz(资源管线,部分已有 05-assets 预研)→ `glTexImage2D`(gl_shim 已支持)。
5. **驱动循环 + 提交帧**:每帧解释 drawScene → `glReadPixels`→PNG(已接,见 done 段);真机/Catalyst 上再接到 CAEAGLLayer 显示。
6. **App 壳(真机显示)**:`UIApplicationMain`+UIViewController+CAEAGLLayer+CADisplayLink,把解释器做成真 App(目前是 CLI,离屏渲染到 PPM)。

### 长尾(到"可玩")
7. 游戏给框架类的 category(如 `UIDevice+hasIllegalApp`):原生不响应时改查 MMINUS 走解释。
8. 输入(触摸→`[EAGLView touchesBegan:]`)、音频(OpenAL→openal-soft)、文件 IO、计时器。
9. 结构体参数/返回(CGRect 等,objc_msgSend_stret)、double/varargs(stringWithFormat:)的编组。
10. 逐场景调通(村庄/农场/任务/商店/小游戏),资源(asprite/dat,key 已知 `39653543fa0d66aa`)。

## 六、诚实的工程量判断

- **已完成**:一个**忠实执行原版 ObjC 游戏代码、转发真原生框架**的 HLE 内核,启动初始化全程真实执行,渲染后端已验证。可行性**全部证死,无玄学**。
- **到"出第一帧"**:卡在上面 1-3(循环+帧驱动+renderbuffer),都是明确可做但需聚焦调试的步骤,非盲目增量。
- **到"完整可玩 1:1"**:是 touchHLE 级别的长尾(结构体编组、全部场景/玩法、输入/音频/资源、海量方法与指令覆盖),**以人月/人年计**。

这份文档保证整套工作可续、可交接。下一步聚焦"渲染集成"(循环+帧驱动+renderbuffer→出第一帧)。
