# touchHLE × 摩尔庄园 5.5.0 —— 离线移植 patch 状态

> 改在 touchHLE 源码(`fresh-port/20-touchHLE-src/touchHLE/`),HEAD v0.2.3-367-gc5dce3c9。
> 目标:Apple Silicon macOS 上跑摩尔庄园 5.5.0,纯离线(服务器已死,不实现联网)。

## 构建 & 运行(一键)
```
bash fresh-port/20-touchHLE-src/build_and_run.sh        # 构建+运行(窗口,45s)
bash fresh-port/20-touchHLE-src/build_and_run.sh build  # 只构建
bash fresh-port/20-touchHLE-src/build_and_run.sh run 60 # 只运行60s
```
手动构建:
```
cd fresh-port/20-touchHLE-src/touchHLE
export CMAKE_PREFIX_PATH=/opt/homebrew BOOST_ROOT=/opt/homebrew CMAKE_POLICY_VERSION_MINIMUM=3.5
unset RUSTUP_TOOLCHAIN          # 用 Homebrew stable cargo 1.95(支持 edition2024)
/opt/homebrew/bin/cargo build --release --offline
```
产物 `target/release/touchHLE`(arm64)。

## ★ 当前状态:applicationDidFinishLaunching 完整执行完毕,进入主 run loop ★
游戏启动方法 `-[iMoleVillageAppDelegate applicationDidFinishLaunching:]`(0xef40)的
**全部 81 个 ObjC 调用都执行了**,包括:
- CCDirector setDirectorType/sharedDirector、EAGLView viewWithFrame:...(创建 GL 视图)
- setOpenGLView:、UIWindow makeKeyAndVisible
- CCNode node(TaomeeLogoLayer)、**CCDirector runWithScene:(设置第一个场景!)**
然后 didFinishLaunching 返回 → 进入 iOS 主 run loop(`[NSRunLoop mainRunLoop] run`)。

**当前阻塞(性质已变,不再是"缺 API"):** 主 run loop 空转后返回 →
touchHLE 打印 "Tearing down app, exiting" → 然后退出清理时对 delegate 发
applicationDidFinishLaunching: 触发 panic(messages.rs:211)。
根本问题 = **run loop 没有持续驱动 cocos2d 的帧绘制(CADisplayLink/CCDirector drawScene)**,
所以它认为没活干就退出了。这是"出第一帧"的核心关口(帧循环驱动),需理解 touchHLE
run loop + CADisplayLink/NSTimer + 游戏 CCDirector 的交互,是实质工作而非补方法。

## 已应用 patch(23个,全部编译通过)— 改动的 touchHLE 源文件
1. `foundation/ns_object.rs` — methodForSelector:/instanceMethodForSelector:(JSONKit)
2. `libc/ctype.rs` — ___stack_chk_guard,关键名 `_$ld$hide$os4.0$__stack_chk_guard`(全函数级根因)
3. `objc/classes.rs` — fake class 名单加:iRate、TMA_SSKeychain、TaomeeAnalytics、PunchBoxAd
4. `objc.rs` — ARC: objc_retain/release/autorelease/storeStrong 等8个 C 函数(+import MutPtr)
5. `foundation/ns_operation.rs`(新文件)— NSOperation/NSInvocationOperation/NSOperationQueue
   (单线程同步执行;已在 foundation.rs 注册 mod + CLASSES)
6. `libc/net/if_.rs` — if_nametoindex 返回0
7. `libc/arpa/inet.rs` — inet_aton
8. `foundation/ns_bundle.rs` — bundleWithPath:(无效路径返nil=跳IDFA广告) + load/isLoaded
9. `foundation/ns_url.rs` — scheme/host/query/lastPathComponent/pathExtension
10. `uikit/ui_application.rs` — UIBackgroundTaskInvalid 常量(InMobi init 需要,全局根因)
11. `foundation/ns_file_manager.rs` — createDirectory 忽略attributes + contentsOfDirectory 返NSError
12. `uikit/ui_pasteboard.rs`(整文件重写)— 完整 UIPasteboard(generalPasteboard/
    pasteboardWithName:create:/string/items/dataForPasteboardType:/setData:forPasteboardType: 等)
13. `foundation/ns_dictionary.rs` — initWithDictionary:nil 守卫 + keysSortedByValueUsingSelector:
14. `foundation/ns_url_connection.rs`(整文件重写)— 离线化:start→异步 connection:didFailWithError:
    (NSURLErrorNotConnectedToInternet),不 hang;有 host object 存 delegate
15. `foundation/ns_process_info.rs` — globallyUniqueString(OpenUDID 需要)+
    operatingSystemVersionString/operatingSystem/hostName/processorCount 等
16. `foundation/ns_user_defaults.rs` — synchronize/registerDefaults:/stringForKey:/integerForKey:/
    floatForKey:/setInteger:forKey:/setFloat:forKey:/arrayForKey:/dictionaryForKey:/dataForKey:

## 崩溃推进轨迹(每patch前进一步,23步强收敛 → boot 完成)
JSONKit → ___stack_chk_guard → iRate → ARC → NSOperationQueue → if_nametoindex →
NSBundle bundleWithPath → NSURL.host → UIBackgroundTaskInvalid → NSFileManager →
TMA_SSKeychain → UIPasteboard×2 → NSURLConnection → initWithDictionary:nil →
keysSortedByValue → TaomeeAnalytics/PunchBoxAd(fake) → NSProcessInfo globallyUniqueString
→ NSProcessInfo operatingSystemVersionString → NSUserDefaults synchronize →
**applicationDidFinishLaunching 完整执行 + 进 run loop(当前)**

## 下一步(帧循环驱动 = 出第一帧)
1. 看 touchHLE `ns_run_loop.rs` 的 run 方法:为何空转返回?需要什么 source 才持续运行?
2. 游戏 setDirectorType:3(CADisplayLink)失败回退 0(NSTimer)。确认 touchHLE 的
   CADisplayLink / NSTimer 在主 run loop 是否被驱动;若 cocos2d 的帧 timer 没注册到
   touchHLE run loop,补上。
3. 目标:run loop 持续运行 → CCDirector drawScene 每帧调用 → ES1Renderer GL → 首帧上屏。
4. 服务器已死:NSURLConnection 已离线化(异步报错),若后续登录场景硬卡再本地 mock。

## 关键调试经验
- Edit 前必先 Read 真实文件(多次因猜错原文 Edit 失败 → 空编译假成功)。
- 验证真编译:`grep -c 'Compiling touchHLE' 日志`(0=空编译,RC仍可能=0)。
- 定位 guest 崩溃:`RUST_BACKTRACE=1` 得 touchHLE 侧栈;PC/LR 用 02-ida/out/functions.txt 二分定位游戏函数。
- IDA 反编译 0xef40 得 didFinishLaunching 完整调用序列(/tmp/adfseq.txt 思路)。
- 输出超长截断:python 写文件再小块读。中文路径大文件用 `/usr/bin/python3 -c`。
- 无 PIL/Quartz;截图用 sips 转 BMP + python stdlib。杀残留:`pkill -9 touchHLE`。

## 会话结束快照(25 patch,源码干净可编译 FINAL_BUILD_RC=0,16.5MB arm64)
新增 patch:NSDate encodeWithCoder:(ns_date.rs:148,NS.time键存Real秒)、UIPasteboard setPersistent:/isPersistent。

**下个会话起点 = NSString longLongValue 归属问题(已诊断,未改)**:
- 崩点:`_touchHLE_NSString does not respond to longLongValue`(游戏解析字符串数字)。
- 根因:ns_string.rs ~1057-1090 结构特殊——有两套 NSString 数值方法定义:
  - 一套在 1062-1086(intValue/longLongValue/integerValue,被 1086 `}` 结束,非标准块)
  - 真正的 category CLASSES 在 1088 `pub const NSStringExtensionMethods: ClassExports = objc_classes!`,
    1092 `@implementation NSString` 之后。
- 即数值方法可能没合并进 `_touchHLE_NSString` 实例方法链,或 category 注册有遗漏。
- 修法:看清 1039-1130 完整宏结构,确认 longLongValue 是否真的注册到 NSString;
  若 category(NSStringExtensionMethods)没被 _touchHLE_NSString 继承,需查 touchHLE
  category 合并机制;最简单兜底=在 _touchHLE_NSString 块直接加 longLongValue。
- 注意:工具读取在 ns_string.rs 这段(中文路径+大文件)偶发重影,务必用 cat -n 或 python 交叉验证。

## 真实进度坐标
boot 已穿过:JSONKit→栈canary→全部广告分析SDK(Flurry/Tapjoy/InMobi/iRate/TaoMee/PunchBox 全fake)
→文件系统→本地化→NSOperation→剪贴板→NSDictionary→网络(离线化)→NSProcessInfo→NSUserDefaults
→归档(NSDate encodeWithCoder)→**NSString longLongValue(当前)**。
全程 25 patch 强收敛,每个崩点都是标准 iOS API,确定性高。
最终"出第一帧"的核心关口仍是:run loop 持续驱动 cocos2d CCDirector drawScene(帧循环驱动)。

## 真·会话末状态(28 patch,b49 RC=0)——以此为准
- 新增 patch:NSDate encodeWithCoder、NSString longLongValue、fake TDGA*/TalkingData*(TalkingData分析SDK)。
- **当前真实崩点(实测)= -[NSData AES256DecryptWithKey:] does not respond**(游戏NSData(AES) category解密资源)。
- 注意:本文件上方"当前状态:进入主run loop"那段是会话早期的错误臆测,已被实测推翻——
  真实是仍在 boot 的资源解密阶段,尚未稳定进 run loop。以本节为准。
- 下一步:在 touchHLE 实现 NSData AES256DecryptWithKey:(host类category;key 39653543fa0d66aa,
  AES-256;看 src/libc/crypto.rs 现有CC_*或加aes crate)。这是加载真实美术资源的关口。
- 一键续:cd fresh-port/20-touchHLE-src && bash build_and_run.sh run 30

## 2026-05-30 SESSION END (authoritative)
- Source tree builds clean (RC=0, arm64). ~30 patches.
- Newest patches: CC_MD5/CC_SHA1 null-guard, NSData subdataWithRange:, NSDate encodeWithCoder:,
  NSString longLongValue, fake TDGAKeyChain/TDGASecureUDID/TDGAOpenUDID/TDGAUtility.
- AES-via-crate attempt rolled back (offline, no crate). Cargo/ns_data clean.
- CURRENT GATE: NSData "immobViewAES256DecryptWithKey:" (game category on host NSData @0x5c00fc).
  It is AES-128-CBC/PKCS7, key "39653543fa0d66aa" (16B), IV needs full decompile of 0x5c00fc.
  Fix path: make category-merge reach host classes (check dyld linking order) + implement CCCrypt
  in libc/crypto.rs; OR implement immobViewAES256DecryptWithKey: as host method (need exact IV).
- Next: bash build_and_run.sh run 30 to reproduce; decompile 0x5c00fc for IV; impl CCCrypt.
- NOTE: ignore the earlier "进入主 run loop" section above — it was a mid-session wrong guess.
  Real state: still in boot, at resource-decryption gate.
