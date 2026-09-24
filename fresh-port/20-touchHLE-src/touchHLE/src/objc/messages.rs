/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Handling of Objective-C messaging (`objc_msgSend` and friends).
//!
//! Resources:
//! - Apple's [Objective-C Runtime Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/ObjCRuntimeGuide/Articles/ocrtHowMessagingWorks.html)
//! - [Apple's documentation of `objc_msgSend`](https://developer.apple.com/documentation/objectivec/1456712-objc_msgsend)
//! - Mike Ash's [objc_msgSend's New Prototype](https://www.mikeash.com/pyblog/objc_msgsends-new-prototype.html)
//! - Peter Steinberger's [Calling Super at Runtime in Swift](https://steipete.com/posts/calling-super-at-runtime/) explains `objc_msgSendSuper2`

use super::{id, nil, Class, ObjC, IMP, SEL};
use crate::abi::{CallFromHost, GuestRet};
use crate::environment::ThreadId;
use crate::libc::pthread::cond::{
    pthread_cond_broadcast, pthread_cond_destroy, pthread_cond_init, pthread_cond_t,
    pthread_cond_wait,
};
use crate::libc::pthread::mutex::{
    pthread_mutex_destroy, pthread_mutex_init, pthread_mutex_lock, pthread_mutex_t,
    pthread_mutex_unlock,
};
use crate::mem::{guest_size_of, ConstPtr, MutPtr, MutVoidPtr, SafeRead};
use crate::objc::classes::InitializationStatus;
use crate::Environment;
use std::any::TypeId;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// [扫描修 2026-09-15] F10-5:兼容性兜底告警(does not respond / faked class / unimplemented class)
/// 按 (类别, 类, 选择子) 去重:同一组合只在第一次返回 true(调用方 log!),之后返回 false(调用方走
/// log_dbg!)。原来每次调用都 log!,同一个缺失方法反复刷屏,把真正有价值的告警冲掉。
/// 只在这些冷路径上调用,不影响 objc_msgSend 热路径;键存 64 位哈希、不分配字符串,集合大小 =
/// 缺失方法个数(几十个)。哈希碰撞的代价只是某条告警降为 log_dbg!,没有功能影响。
fn first_compat_warning(kind: &str, class: &str, sel: &str) -> bool {
    use std::hash::{Hash, Hasher};
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<u64>>> =
        std::sync::Mutex::new(None);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (kind, class, sel).hash(&mut hasher);
    let key = hasher.finish();
    let mut guard = match SEEN.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.get_or_insert_with(Default::default).insert(key)
}

/// [复核修 2026-09-15] R3-1:选择子跟踪([TRACE])限流。
/// 根因:F7-12 的跟踪对每条命中消息同步 log! 一行(stderr + 写日志文件),mole_dev::trace_filter_matches
/// 的注释写明「限流由调用方负责」,调用方就是 objc_msgSend_inner,但这里原来没做。被跟踪类每帧都会收到
/// visit/transform/draw/update: 等消息,实测 touchHLE_log.txt 28392 行里 28252 行是 [TRACE],既冲掉真正的
/// 告警,又在 objc_msgSend 热路径和 drawScene 帧栈上同步写文件拖慢帧率。
/// 做法:按宿主单调时钟的整秒分窗,每窗最多打印 TRACE_LINES_PER_SEC 行,超出的只计数;进入新窗口后的第一条
/// 命中消息先补一行上一窗口的丢弃数。只用原子变量,无锁、无分配(除首次初始化时钟基准外)。
/// 只在「跟踪开启且规则命中」时才调用:跟踪关闭时 objc_msgSend 路径与原来一样,只有 trace_on() 那一次原子读。
/// [复核修 2026-09-15] R3-1 返修①(稀有消息不再被整批丢掉):原先整秒额度全局共用,每帧重复的
/// visit/transform/draw/sortAllChildren/tag 先把它用完——实测 touchHLE_log.txt 里相邻两条
/// ActionCenterLayer visit 之间有 1905 处正好隔 11 行,即稳态每帧 11 行,60FPS 约 660 行/秒,约 0.3 秒就用完
/// 200 条;点开活动中心时那批偶发消息(日志 792-846 行:NetworkManager isReachable/isConnected、setShowForecast:、
/// displayUILayer、ActivityForecastLayer 初始化链)落在同一秒剩下的时间里就一行不打,而这正是排查「签到页
/// isReachable/isConnected 漏放行 LR」要找的信息。现在「本秒内第一次出现的 (类, 选择子, 调用方 LR)」不占全局
/// 额度、直接打印(另设 TRACE_FIRST_SEEN_PER_SEC 上限,防宽泛的 *片段 规则失控),重复出现的才占 200 条额度。
/// 键里带 LR 而不只是 (类, 选择子):同一批里同一个选择子常从不同调用点发出(isOpen lr=0x1b877 / lr=0x59b81、
/// init lr=0x2d2629 / lr=0x3ece79),各打一行正好是定位调用点要的;每帧消息的调用点是固定的几个,不会多出多少行。
/// 不做去重,打印出来的每一行仍带 recv/lr。「本秒见过」集合是 TRACE_SEEN_WORDS 个 u64 组成的原子位图,开新窗口
/// 时整体清零:不分配、不加锁、不读字符串内容。哈希碰撞只会让某个新组合被当成重复、改走全局额度(额度没用完
/// 照样打印),不会反过来让重复消息绕过额度——每个位每秒最多放行一次,没有两组合互相覆盖导致反复放行的问题。
/// [复核修 2026-09-15] R3-1 返修②(缺口立刻可见):原先丢弃数只在「下一秒第一条命中」时补报;超限那一秒里
/// 跟踪被关掉(mole_dev::toggle_trace 不清算)或游戏 panic,日志就停在第 200 行,读日志的人会误以为那就是最后
/// 一次调用。现在每个窗口第一次丢弃时立刻打一行「已达上限」提示,每窗最多多一行。
/// guest 线程都在同一个宿主线程上轮转执行,这几个原子量之间没有真正的并发;即使将来有,最坏也只是计数略有
/// 偏差,不影响正确性。
const TRACE_LINES_PER_SEC: u32 = 200;
/// [复核修 2026-09-15] R3-1 返修①:每秒「首次出现的 (类, 选择子, 调用方 LR)」绕过全局额度打印的上限。
/// 正常的类名/类名.选择子 规则每秒不同组合只有几十个(整份 28252 行跟踪日志从头到尾一共才 238 个),
/// 这个上限只拦宽泛的 *片段 规则;超出后新组合改走全局额度。
const TRACE_FIRST_SEEN_PER_SEC: u32 = 500;
/// [复核修 2026-09-15] R3-1 返修①:「本秒见过的组合」位图位数取对数(2^13 = 8192 位 = 128 个 u64,共 1KB)。
/// 本秒已有 k 个不同组合时,一个新组合被误判为重复的概率约 k/8192。
const TRACE_SEEN_BITS_LOG2: u32 = 13;
const TRACE_SEEN_WORDS: usize = 1 << (TRACE_SEEN_BITS_LOG2 - 6);
static TRACE_SEEN: [AtomicU64; TRACE_SEEN_WORDS] = [const { AtomicU64::new(0) }; TRACE_SEEN_WORDS];
/// 当前窗口的秒号(进程内单调时钟,从 1 起算;0 = 还没开过窗口)。
static TRACE_WINDOW_SEC: AtomicU64 = AtomicU64::new(0);
/// 当前窗口占全局额度打印的 [TRACE] 行数(不含「首次出现」旁路打印的行)。
static TRACE_WINDOW_PRINTED: AtomicU32 = AtomicU32::new(0);
/// [复核修 2026-09-15] R3-1 返修①:当前窗口走「首次出现」旁路打印的行数。
static TRACE_WINDOW_FIRST_SEEN: AtomicU32 = AtomicU32::new(0);
/// 当前窗口因超限被丢弃的命中条数。
static TRACE_WINDOW_DROPPED: AtomicU32 = AtomicU32::new(0);

/// [复核修 2026-09-15] R3-1 返修①:把 (类, 选择子, 调用方 LR) 压成 64 位键,只做整数运算。
/// 类与 LR 都是 32 位 guest 地址;SEL 的内部字段在 objc::selectors 里是私有的,这里取不到,改用 as_str 借来的
/// 宿主指针——选择子已唯一化,guest 内存是一整块固定映射,同一个选择子的字符串地址不变。不读字符串内容。
/// 乘奇数常数在 2^64 下是双射,第二轮乘法让高位依赖全部输入位(trace_rate_admit 取高位当位图下标)。
fn trace_pair_key(class: Class, sel_str: &str, lr: u32) -> u64 {
    let class_lr = (class.to_bits() as u64) | ((lr as u64) << 32);
    (class_lr.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (sel_str.as_ptr() as usize as u64))
        .wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

/// [复核修 2026-09-15] R3-1 返修②:限流判定结果。原先用 Option 只能区分「打印/不打印」,
/// 没法告诉调用方「本窗口刚开始丢弃」,缺口要等下一秒才看得见。
enum TraceAdmit {
    /// 打印本条。dropped > 0 表示刚结束的那个窗口丢弃了 dropped 条、该窗口开始于 age_secs 秒前,调用方先补一行汇报。
    Print { dropped: u32, age_secs: u64 },
    /// 本窗口第一次超限:本条计入丢弃,调用方立刻打一行「已达上限」提示(每个窗口最多一次)。
    FirstDrop,
    /// 超限:只计数,不打印。
    Drop,
}

/// [复核修 2026-09-15] R3-1:限流判定。`key` 由 trace_pair_key 算出,只用来判断本秒是否第一次出现。
/// 开新窗口时各计数清零,新窗口的第一条要么走「首次出现」旁路(位图刚清空,必然命中),要么走全局额度
/// (已打印数为 0,必然有额度),两条路都返回 Print,所以上一窗口的丢弃汇报总是和一条允许打印的消息一起返回,不会丢。
fn trace_rate_admit(key: u64) -> TraceAdmit {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    // 秒号 +1,保证与初始值 0 不同:第一次命中一定会开出新窗口。
    let now_sec = EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs() + 1;
    let window_sec = TRACE_WINDOW_SEC.load(Ordering::Relaxed);
    let mut prev_dropped = 0u32;
    let mut prev_age_secs = 0u64;
    if window_sec != now_sec {
        TRACE_WINDOW_SEC.store(now_sec, Ordering::Relaxed);
        TRACE_WINDOW_PRINTED.store(0, Ordering::Relaxed);
        TRACE_WINDOW_FIRST_SEEN.store(0, Ordering::Relaxed);
        // 每秒最多一次,128 次原子写。
        for word in TRACE_SEEN.iter() {
            word.store(0, Ordering::Relaxed);
        }
        prev_dropped = TRACE_WINDOW_DROPPED.swap(0, Ordering::Relaxed);
        prev_age_secs = now_sec.saturating_sub(window_sec);
    }
    // 返修①:本秒第一次出现的组合不占全局额度。旁路额度用完后不再置位,新组合直接走下面的全局额度。
    let first_seen = TRACE_WINDOW_FIRST_SEEN.load(Ordering::Relaxed);
    if first_seen < TRACE_FIRST_SEEN_PER_SEC {
        let bit = (key >> (64 - TRACE_SEEN_BITS_LOG2)) as usize;
        let mask = 1u64 << (bit & 63);
        let before = TRACE_SEEN[bit >> 6].fetch_or(mask, Ordering::Relaxed);
        if before & mask == 0 {
            TRACE_WINDOW_FIRST_SEEN.store(first_seen + 1, Ordering::Relaxed);
            return TraceAdmit::Print {
                dropped: prev_dropped,
                age_secs: prev_age_secs,
            };
        }
    }
    let printed = TRACE_WINDOW_PRINTED.load(Ordering::Relaxed);
    if printed < TRACE_LINES_PER_SEC {
        TRACE_WINDOW_PRINTED.store(printed + 1, Ordering::Relaxed);
        TraceAdmit::Print {
            dropped: prev_dropped,
            age_secs: prev_age_secs,
        }
    } else {
        let dropped = TRACE_WINDOW_DROPPED.load(Ordering::Relaxed);
        TRACE_WINDOW_DROPPED.store(dropped.saturating_add(1), Ordering::Relaxed);
        // 返修②:丢弃数由 0 变 1 = 本窗口第一次超限,让调用方立刻留痕。
        if dropped == 0 {
            TraceAdmit::FirstDrop
        } else {
            TraceAdmit::Drop
        }
    }
}

pub(super) struct ThreadInitializer {
    mutex: MutPtr<pthread_mutex_t>,
    cond: MutPtr<pthread_cond_t>,
    tid: ThreadId,
    waiters: u32,
}

fn maybe_initialize_class(env: &mut Environment, receiver: id) {
    let class_host_object = match env.objc.get_host_object(receiver) {
        Some(o) => o,
        None => {
            // [P1 iOS interp debug] The "class" being messaged isn't a registered
            // host object — almost always means a wrong value reached R0 as the
            // receiver (e.g. the stack-guard sentinel 0xdead2a55). Dump the
            // interpreter's recent-instruction trace so we can see which guest
            // instruction produced the bad receiver, then panic as before.
            echo!(
                "[OBJC-BADRECV] maybe_initialize_class: receiver {:?} not a host object; regs R0..R3 = {:08x?}",
                receiver,
                &env.cpu.regs()[0..4]
            );
            env.cpu.dump_interp_trace();
            panic!("maybe_initialize_class: receiver {:?} is not a registered object", receiver);
        }
    };
    let Some(&super::ClassHostObject {
        superclass,
        is_metaclass,
        is_initialized,
        ..
    }) = class_host_object.as_any().downcast_ref()
    else {
        // If it's here, there's one of two cases:
        //
        // 1: The receiver is an instance. The class should then have already
        // called +initialize since you need to call +alloc to create an
        // instance (this also needs to be upheld for instances created with
        // class_createInstance(), whenever we implement that)
        //
        // 2: The reciever is a fake/unimplemented class. There's no reason to
        // send +initialize to those, so we don't bother.
        return;
    };

    if is_metaclass || is_initialized == InitializationStatus::Initialized {
        // On the offchance that this is a metaclass, we don't need to send
        // +initialize to it. We also don't need to send it if the class is
        // already initialized.
        return;
    }

    // This class is not initialized, but there might be classes above it in the
    // hierarchy that also need to be checked, so check those first.
    if !superclass.is_null() {
        maybe_initialize_class(env, superclass);
    }

    if is_initialized == InitializationStatus::Initializing {
        env.objc
            .initializer_threads
            .get_mut(&receiver)
            .unwrap()
            .waiters += 1;
        let ThreadInitializer {
            mutex, cond, tid, ..
        } = *env.objc.initializer_threads.get(&receiver).unwrap();

        // The current thread is already initializing, so let it call other
        // messages while it does so.
        if tid == env.current_thread {
            return;
        }

        // We are waiting for another thread to initialize, wait for it to
        // broadcast that it has finished.
        pthread_mutex_lock(env, mutex);
        loop {
            let class_host_object = env.objc.get_host_object(receiver).unwrap();
            let &super::ClassHostObject { is_initialized, .. } =
                class_host_object.as_any().downcast_ref().unwrap();
            if is_initialized == InitializationStatus::Initialized {
                break;
            }
            pthread_cond_wait(env, cond, mutex);
        }
        pthread_mutex_unlock(env, mutex);

        let ThreadInitializer {
            ref mut waiters, ..
        } = *env.objc.initializer_threads.get_mut(&receiver).unwrap();
        *waiters -= 1;
        if *waiters == 0 {
            // We're the last waiter for this initialize, so clean up state on
            // the way out.
            pthread_cond_destroy(env, cond);
            pthread_mutex_destroy(env, mutex);
            env.objc.initializer_threads.remove(&receiver);
        }
    } else {
        log_dbg!(
            "Initializing {:?} on thread {}",
            env.objc.try_get_class_name(receiver),
            env.current_thread
        );
        let regs = *env.cpu.regs();

        let mutex = env.mem.alloc(guest_size_of::<pthread_mutex_t>()).cast();
        let cond = env.mem.alloc(guest_size_of::<pthread_cond_t>()).cast();
        pthread_mutex_init(env, mutex, ConstPtr::null());
        pthread_cond_init(env, cond, ConstPtr::null());
        env.objc.initializer_threads.insert(
            receiver,
            ThreadInitializer {
                mutex,
                cond,
                tid: env.current_thread,
                waiters: 0,
            },
        );

        let super::ClassHostObject { is_initialized, .. } = env.objc.borrow_mut(receiver);
        *is_initialized = InitializationStatus::Initializing;
        () = msg![env; receiver initialize];
        let super::ClassHostObject { is_initialized, .. } = env.objc.borrow_mut(receiver);
        *is_initialized = InitializationStatus::Initialized;
        env.cpu.regs_mut().copy_from_slice(&regs);
        log_dbg!(
            "Done initializing {:?} on thread {}",
            env.objc.try_get_class_name(receiver),
            env.current_thread
        );
        if env.objc.initializer_threads.get(&receiver).unwrap().waiters == 0 {
            // Nobody ended up waiting for this initializer, so we can just
            // destroy it.
            pthread_cond_destroy(env, cond);
            pthread_mutex_destroy(env, mutex);
            env.objc.initializer_threads.remove(&receiver);
        } else {
            pthread_mutex_lock(env, mutex);
            pthread_cond_broadcast(env, cond);
            pthread_mutex_unlock(env, mutex);
        }
    }
}

/// The core implementation of `objc_msgSend`, the main function of Objective-C.
///
/// Note that while only two parameters (usually receiver and selector) are
/// defined by the wrappers over this function, a call to an `objc_msgSend`
/// variant may have additional arguments to be forwarded (or rather, left
/// untouched) by `objc_msgSend` when it tail-calls the method implementation it
/// looks up. This is invisible to the Rust type system; we're relying on
/// [crate::abi::CallFromGuest] here.
///
/// Similarly, the return value of `objc_msgSend` is whatever value is returned
/// by the method implementation. We are relying on CallFromGuest not
/// overwriting it.
#[allow(non_snake_case)]
fn objc_msgSend_inner(
    env: &mut Environment,
    receiver: id,
    selector: SEL,
    super2: Option<Class>,
    tolerate_type_mismatch: bool,
) {
    log_dbg!(
        "Dispatching {} for {:?}",
        selector.as_str(&env.mem),
        receiver
    );
    let message_type_info = env.objc.message_type_info.take();

    if receiver == nil {
        // https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/ObjectiveC/Chapters/ocObjectsClasses.html#//apple_ref/doc/uid/TP30001163-CH11-SW7
        log_dbg!("[nil {}]", selector.as_str(&env.mem));
        env.cpu.regs_mut()[0..2].fill(0);
        return;
    }

    // [MoleWorld offline port] A non-nil "receiver" inside the null page (< 0x1000)
    // is not a real object — it's an uninitialized sentinel (e.g. 0x1) that an
    // offline island code path passes where an object is expected (our one-key
    // entry bypasses some of the game's normal scene wiring, leaving sentinel
    // slots). Reading its isa would either fault (null-page access at 0x1) or yield
    // garbage that then fails class lookup ("Could not get class name!"), aborting
    // the whole emulator. Treat it like a message to nil (no-op, return 0) instead.
    // Nothing valid lives below 0x1000 (the guest image loads at 0x4000+, the heap
    // is far higher), so this never masks a real object. Robust last line of
    // defense for the island touch-0x1 crash, independent of any guest-side hook.
    if receiver.to_bits() < 0x1000 {
        log_dbg!(
            "[(null-page receiver {:?}) {}] -> treating as nil (no-op)",
            receiver,
            selector.as_str(&env.mem)
        );
        env.cpu.regs_mut()[0..2].fill(0);
        return;
    }

    let orig_class = super2.unwrap_or_else(|| ObjC::read_isa(receiver, &env.mem));
    // MoleWorld offline port: a non-nil receiver can still have a nil isa/class
    // when it isn't a real object — e.g. the analytics/ad SDKs use the result of
    // a no-op'd function (CFArrayCreate/Sec*/ASIdentifierManager return 0/nil) as
    // if it were an object and send it a message. Rather than aborting, treat
    // "object with nil class" the same as a message to nil: return zero.
    if orig_class == nil {
        log_dbg!(
            "[(receiver {:?} with nil class) {}] -> treating as nil (no-op)",
            receiver,
            selector.as_str(&env.mem)
        );
        env.cpu.regs_mut()[0..2].fill(0);
        return;
    }
    // [同步 iOS 91eb00f · 2026-09-24] 同理,接收者非 nil、但 isa 指向一个没有注册 host object 的「类」
    // (取不到类名的垃圾指针):离线移植里有些类方法被伪造成返回非 nil 哨兵,游戏又拿它当真对象发消息
    // (iOS 上在好友界面实测崩过)。下面的 get_class_name(作弊钩子粗筛、「does not respond」路径都会调)
    // 对这种类会 expect() panic「Could not get class name!」。当作发给 nil 处理:返回 0。
    if env.objc.try_get_class_name(orig_class).is_none() {
        log_dbg!(
            "[(receiver {:?} with unresolvable class {:?}) {}] -> treating as nil (no-op)",
            receiver,
            orig_class,
            selector.as_str(&env.mem)
        );
        env.cpu.regs_mut()[0..2].fill(0);
        return;
    }
    // [MoleWorld] Debug-menu cheat toggles (free shop, multipliers, force VIP,
    // anti-cheat off). Gated by a cheap any_enabled() check so the hot path pays
    // nothing when all cheats are off. intercept() may fully handle the call
    // (return) or tweak an argument register and let the real method run.
    // [扫描修 2026-09-15] F7-12:选择子跟踪(开发工具)。类名/选择子字符串在这里统一取一次,供跟踪与
    // 作弊钩子粗筛共用。跟踪判断放在 any_enabled 总闸之前、intercept_wants 之外:既不依赖作弊总闸,
    // 也不会把被跟踪的类误送进 intercept、破坏它的白名单不变量。trace_on() 只读原子变量,关闭时这里
    // 只多一次分支判断;get_class_name / as_str 都是借用,整条路径不产生任何字符串分配。
    let trace_on = crate::mole_dev::trace_on();
    let cheats_on = crate::mole_cheats::any_enabled();
    // [同步 iOS 2026-09-16] 宽屏 UI43 v2 虚拟世界换算(移植自 iOS 分支 8bc7046):已右移根层的 position/setPosition:、
    // 已右移 UIKit 子视图的 frame/setFrame:、白名单代码的触摸/世界坐标换算、dealloc 除名。SEL 指针快判定、零分配,
    // 没有任何登记对象时(未开 UI43 时恒如此)只付两次原子读。★`message_type_info.is_some()` = 本条消息由**宿主**
    // msg_send 发出(宿主 msg_send 设置它、guest 派发恒为 None):宿主发起时 LR 是陈旧的 main 返回地址,不能拿来判定
    // "谁在问",且宿主必须看到真实坐标。放在跟踪之前不会让跟踪漏看:换算臂转发真方法走的是宿主 msg_send,
    // 同一个选择子会再经过这里一次并被跟踪打印。
    if cheats_on && crate::mole_cheats::intercept_fast(env, selector, message_type_info.is_some()) {
        return;
    }
    if trace_on || cheats_on {
        // [MoleWorld P0-B] 先用借来的 &str(零分配)过粗筛:游戏每帧约 16000 条消息,99% 不命中任何
        // hook,直接 bail——不付出下面两次 to_string 堆分配,也不进 intercept 的长比较链(每条消息省
        // 2 次 malloc/free,显著降分配器压力/抖动)。只有命中白名单的少数消息才 to_string + 进 intercept。
        let class_name = env.objc.get_class_name(orig_class);
        let sel_str = selector.as_str(&env.mem);
        if trace_on && crate::mole_dev::trace_filter_matches(class_name, sel_str) {
            // [复核修 2026-09-15] R3-1:命中后先过每秒限流(见 trace_rate_admit),超限的只计数不打印;
            // 进入新的一秒时先补一行上一窗口的丢弃数,再打印本条。
            // [复核修 2026-09-15] R3-1 返修:本秒首次出现的 (类, 选择子, 调用方 LR) 不占额度照常打印;
            // 每个窗口第一次丢弃时立刻打一行提示,跟踪被关掉或崩溃时也看得出后面有截断。
            let lr = env.cpu.regs()[crate::cpu::Cpu::LR];
            match trace_rate_admit(trace_pair_key(orig_class, sel_str, lr)) {
                TraceAdmit::Print { dropped, age_secs } => {
                    if dropped > 0 {
                        log!(
                            "[TRACE] 限流:{} 秒前开始的那 1 秒内另有 {} 条命中未打印(重复命中上限 {} 条/秒)",
                            age_secs,
                            dropped,
                            TRACE_LINES_PER_SEC
                        );
                    }
                    // 格式:[TRACE] 类 选择子 接收者地址 调用方LR(LR = 调用点 + 4,Thumb 代码带最低位 1)。
                    log!(
                        "[TRACE] {} {} recv={:?} lr={:#x}",
                        class_name,
                        sel_str,
                        receiver,
                        lr
                    );
                }
                TraceAdmit::FirstDrop => {
                    log!(
                        "[TRACE] 限流:本秒重复命中已达上限 {} 条,之后的重复命中只计数(下一秒开头汇总);本秒首次出现的 类+选择子+调用点 仍照常打印(每秒最多 {} 条)",
                        TRACE_LINES_PER_SEC,
                        TRACE_FIRST_SEEN_PER_SEC
                    );
                }
                TraceAdmit::Drop => {}
            }
        }
        if cheats_on && crate::mole_cheats::intercept_wants(class_name, sel_str) {
            let class_owned = class_name.to_string();
            let sel_owned = sel_str.to_string();
            if crate::mole_cheats::intercept(env, &class_owned, &sel_owned) {
                return;
            }
        }
    }
    // [MoleWorld] Two functional short-circuits for offline play. Gated on the
    // class name FIRST (a cheap &str compare against the already-available class
    // name) so the hot objc_msgSend path only pays a selector-string conversion
    // for messages to these two specific classes — not for every message. (The
    // earlier broad [SCENE]/[FLOW]/[COCOS]/[TOUCHDISP] tracing probes that ran a
    // selector.as_str + dozens of string compares on EVERY message were removed;
    // they were a major per-frame slowdown.)
    if let Some(ho) = env.objc.get_host_object(orig_class) {
        if let Some(&super::ClassHostObject { ref name, .. }) =
            ho.as_any().downcast_ref::<super::ClassHostObject>()
        {
            // -[AvatarLayer showNetWorkError]: 离线改名落地。
            //
            // 改名流程(RE 自 5.5.0 armv7,对照 2.4.3):
            //   用户在改名框点"确定"→ 收键盘 → -[AvatarLayer hideTextField]
            //   (IMP 0xffb74)。hideTextField 先把输入框/背景 setHidden:/setVisible:
            //   收起(UI 拆解,无条件先执行),再 `if textField.text.length==0 return`,
            //   然后唯一的"无网络门":
            //       if ([[NetworkManager sharedInstance] isReachable]) [self VerifyNickName];
            //       else                                              [self showNetWorkError];
            //   在线分支 -[AvatarLayer VerifyNickName](0xffc4c)只做
            //   `[<obj> sendNickNameToServer: self.textField.text]`(上传,不本地落地);
            //   真正"本地改名+刷新屏幕昵称"在 -[AvatarLayer saveName](0xffca4),它做
            //   `[[GameData sharedInstance].userInfoData setName: self.textField.text]`×2
            //   + `[<label> setString: self.textField.text]`,只读 textField,绝不碰服务器
            //   数据 → 离线调用 100% 安全。正常在线时 saveName 由服务器改名成功回包
            //   (远在 0x229bc 的网络分发器)触发;离线那一回包永不到达,所以名字落不下来。
            //   (-[AvatarLayer showEditNickNameResult:] 在 5.5.0 与 2.4.3 都是空壳 `bx lr`,
            //   且全二进制无任何 selref 引用 → 驱动它毫无意义,这里不用它。)
            //
            // 为什么之前把 SCNetworkReachabilityGetFlags 谎报可达没用:门读的是
            // NetworkManager 缓存的 isReachable_ ivar(由 reachability 回调写,离线永远
            // 不触发),不是实时 GetFlags。
            //
            // 拦截点选 showNetWorkError 而非 hideTextField:已核实在整个 AvatarLayer
            // 代码段(0xfc6c8..0x1001d3)里 showNetWorkError 只有 0xffc40 这一个调用点
            // (就是上面的离线门),所以 `name=="AvatarLayer" && sel=="showNetWorkError"`
            // 唯一对应"离线改名失败"这一条路 —— 既不误伤别处的无网络提示,又让 hideTextField
            // 的 UI 拆解照常先跑完。拦到后:用游戏自己的 saveName 本地落地 + 刷新屏幕昵称,
            // 再 -[GameData saveUserInfoData] 落盘(AES 归档,与 mole_menu 同一持久化路径),
            // 最后吞掉"无网络"弹窗。
            if name == "AvatarLayer" && selector.as_str(&env.mem) == "showNetWorkError" {
                let recv = receiver;
                // 1) 本地生效 + 刷新屏幕昵称(saveName 只读 self.textField,离线安全)。
                if env.objc.object_has_method_named(&env.mem, recv, "saveName") {
                    let save_name = env
                        .objc
                        .register_host_selector("saveName".to_string(), &mut env.mem);
                    let _: () = crate::objc::msg_send(env, (recv, save_name));
                } else {
                    // 兜底(理论上 5.5.0 必有 saveName):直接
                    // [[GameData sharedInstance].userInfoData setName: self.textField.text]。
                    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
                    let shared = env
                        .objc
                        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                    let gd: id = crate::objc::msg_send(env, (gd_cls, shared));
                    let ui_sel = env
                        .objc
                        .register_host_selector("userInfoData".to_string(), &mut env.mem);
                    let ui: id = if gd != nil {
                        crate::objc::msg_send(env, (gd, ui_sel))
                    } else {
                        nil
                    };
                    // self.textField:RE 显示 hideTextField/saveName 取 self 上的输入框 ivar;
                    // 从 Rust 取不到该 ivar,故兜底走 AvatarLayer 的 -nickName getter(若有)
                    // 或直接放弃改名值(只持久化已有状态)。saveName 路径几乎总会命中,
                    // 这里仅作不崩的保险。
                    if ui != nil && env.objc.object_has_method_named(&env.mem, recv, "nickName") {
                        let nick_sel = env
                            .objc
                            .register_host_selector("nickName".to_string(), &mut env.mem);
                        let nick: id = crate::objc::msg_send(env, (recv, nick_sel));
                        if nick != nil {
                            let set_name = env
                                .objc
                                .register_host_selector("setName:".to_string(), &mut env.mem);
                            let _: () = crate::objc::msg_send(env, (ui, set_name, nick));
                        }
                    }
                }
                // 2) 落盘:-[GameData saveUserInfoData](AES 归档到 /Documents 的 .dat,
                //    与 mole_menu 的 save_user_info 同路径)。
                let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
                let shared = env
                    .objc
                    .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                let gd: id = crate::objc::msg_send(env, (gd_cls, shared));
                if gd != nil {
                    let save = env
                        .objc
                        .register_host_selector("saveUserInfoData".to_string(), &mut env.mem);
                    let _: () = crate::objc::msg_send(env, (gd, save));
                }
                log!("[改名] 离线改名已本地生效并落盘(saveName + saveUserInfoData),吞掉无网络弹窗");
                // 3) 吞掉"无网络"弹窗 —— 不让原 showNetWorkError 跑。
                env.cpu.regs_mut()[0..2].fill(0);
                return;
            }
            // [2026-09-16] F1-05 删掉 -[UserInfoData initWithCoder:] 的「贝壳读档还原」钩子(连同递归卫 MOLE_IN_UID_INITCODER)。
            // 它是给无限贝壳破解包写的:破解包在 VA 0xb9ce0 把 vipGold 塞死成 2097151。现在基底是香草,
            // -[UserInfoData initWithCoder:]@0xb99f4 在 0xb9ce0-0xb9d02 本来就是 decodeIntForKey:@"vipGold" → encryptInt: →
            // setNewVipGold:,原版自己按存档真实值还原贝壳;CRACK_PATCHES 也不含 0xb9ce0。钩子只会让每次读档重复解码一遍、
            // 打出已不成立的「跳过破解版」日志,还泄漏一个 from_rust_string 的 +1 字符串。
            // 将来若换回破解包,需要恢复这个钩子(可先读 0xb9ce0 处字节确认是破解版常量装载再生效)。
            // -[IMCommonMgr checkUpdates:]: kicks off +[CryptUtils doCipher:...]
            // on network data that's empty offline, computing a negative (huge
            // unsigned) buffer size that corrupts memory. Pure analytics/update
            // feature — make it a no-op offline.
            if name == "IMCommonMgr" && selector.as_str(&env.mem) == "checkUpdates:" {
                env.cpu.regs_mut()[0..2].fill(0);
                return;
            }
            // -[LogoLayer shownewFunctionIntroductionLayer]: presents a swipeable
            // promo intro whose paging needs UITapGestureRecognizer/UIScrollView
            // gestures we don't implement, so boot would stall there. Its own
            // no-new-version branch just calls -replaceByLoadingScene; do that.
            if name == "LogoLayer"
                && selector.as_str(&env.mem) == "shownewFunctionIntroductionLayer"
            {
                if let Some(sel) = env.objc.lookup_selector("replaceByLoadingScene") {
                    let recv = receiver;
                    // [深扫修 2026-09-12] 补回原版在这里做的版本记录写入(0x1905b0-0x190616):
                    // 本地版本号大于 newVersionRecord 时先 setNewVersionRecord: + saveSettings,
                    // 然后才决定弹不弹介绍层。只吞介绍层不补写的话 newVersionRecord 永远是 0,
                    // -[GameSettings loadSettings]@0x185890 每次启动都把 isNightEffectOff 强制置 1,
                    // 玩家在选项里打开的夜晚效果重启即丢。
                    if let (Some(sh_mgr), Some(get_ver), Some(sh_inst), Some(nvr), Some(set_nvr), Some(save)) = (
                        env.objc.lookup_selector("sharedManager"),
                        env.objc.lookup_selector("getLocalVersion"),
                        env.objc.lookup_selector("sharedInstance"),
                        env.objc.lookup_selector("newVersionRecord"),
                        env.objc.lookup_selector("setNewVersionRecord:"),
                        env.objc.lookup_selector("saveSettings"),
                    ) {
                        let wm_cls = env.objc.get_known_class("WrapperManager", &mut env.mem);
                        let gs_cls = env.objc.get_known_class("GameSettings", &mut env.mem);
                        let wm: id = crate::objc::msg_send(env, (wm_cls, sh_mgr));
                        let gs: id = crate::objc::msg_send(env, (gs_cls, sh_inst));
                        if wm != nil && gs != nil {
                            let local: u32 = crate::objc::msg_send(env, (wm, get_ver));
                            let record: u32 = crate::objc::msg_send(env, (gs, nvr));
                            if record < local {
                                let _: () = crate::objc::msg_send(env, (gs, set_nvr, local));
                                let _: () = crate::objc::msg_send(env, (gs, save));
                                log!(
                                    "[深扫修] 跳过新功能介绍层,补写 newVersionRecord {:#x} → {:#x}(夜晚效果开关从此按玩家设置读取)",
                                    record,
                                    local
                                );
                            }
                        }
                    }
                    () = crate::objc::msg_send(env, (recv, sel));
                    return;
                }
            }
            // [2026-09-16] F1-05 删掉 LogoLayer 客服/换账号/换玩家/版本四个选择子的「标题页删档」臂:5.5.0 的 LogoLayer
            // 只有 scene/init/replaceByLoadingScene/shownewFunctionIntroductionLayer/callLoading/onEnter/update:/
            // PlayAnimation/dealloc,那四个选择子在 methods.txt 与 selref 里零命中,这一臂永远不会触发。
            // 删档入口在作弊菜单「删本地存档并退出」(共享实现 save_reset::delete_local_saves)。
            // -[NewStyleStoreMainLayer onBuyVIPGold:]: the 贝壳 (shell) packs are
            // real-money StoreKit IAP gated by network reachability — both dead in
            // this offline port, so a shell-pack tap normally just shows a
            // "no network" box and never credits anything. Per user request, make
            // it a free local purchase: credit shells via
            // -[GameData addVipGoldForBuy:UIUpdate:] (adds to vip_gold + refreshes
            // the HUD) and skip the dead IAP path entirely.
            // [2026-09-16] E-01 只接管 100_0.dat 里真有的充值档位(shell_pack 查得到的 itemid 1..7)。itemid 8 是广告墙「免费贝壳」格:
            // onItemsMenuSelected: 在 0x3b23ce 固定传 8,资源页 onButtonBuyItemSelected: 在 _selectedObjectId−1≤7 时也可能传 8。
            // 以前它落到兜底白送 20 贝壳:岛上(isReachable 被通配成 1)和联机时每点一次送一次,还经 on_shells_purchased 误触发
            // gamedataFlag|=0x30、解锁 16283/14974 这些「充值成功」副作用;主村离线却弹「没有连接网络」,同一个按钮两种表现。
            // 现在查不到档位就不进这一臂:条件里只读 r2,不发任何 msg_send,寄存器原样,落到下面的放行分支和真 onBuyVIPGold:。
            // 原版 0x3b29c4 `cmp r2,#8` → [[WrapperManager sharedManager] checkAdWallAvailable]@0x262274,canShowADForExchange 只由服务器
            // 1064/1182 回包写入,离线和私服下恒为 NO → 直接收尾,即原版离线的空操作。广告墙类在 classes.rs 里整类伪造,不复活。
            if name == "NewStyleStoreMainLayer"
                && selector.as_str(&env.mem) == "onBuyVIPGold:"
                && crate::mole_items::shell_pack(env.cpu.regs()[2]).is_some()
            {
                // onBuyVIPGold:(int):按原版各档真实贝壳数发放,不再死值 1000。
                // 必须在任何 msg_send 前读 regs[2],否则被覆盖。
                // [补完 2026-09-15] 参数纠正:regs[2] 不是 0 起的包下标,而是 ShopItemData.itemid(1..7,取自 100_0.dat)。
                // 原版 -[NewStyleStoreMainLayer onBuyVIPGold:]@0x3b29b0 用它调 -[GameData getShopItemData:]@0x7bf3c,
                // 按 itemid 相等查档位;itemid 8 是广告墙「免费贝壳」格(0x3b29c4 cmp r2,#8 → 广告墙,不是充值)。
                // 三个调用方传的都是 itemid:-[NewStyleStoreItemsView onButtonBuyItemSelected:]@0x3bd9c0(_selectedObjectId−1 ≤ 7 才发,
                // 0 永远到不了这里)、-[DiscountInfoLayer onButtonShop:]@0x1ec46a(goodsId ≤ 7)、onItemsMenuSelected:@0x3b23ce(固定 8)。
                // 旧代码按下标取 [20,105,…,3500]:每档都多发一档,itemid 7(3500 贝壳档)越界落到兜底只发 20。
                // 档位表(贝壳数 + 标价)移到 mole_items::SHELL_PACKS,与 VIP 累计共用一份。
                // [2026-09-16] E-01 查不到档位的参数已被上面的条件挡在外面,这里 pack 恒为 Some;map_or 的 20 只是避免写 unwrap。
                let item_id = env.cpu.regs()[2];
                let pack = crate::mole_items::shell_pack(item_id);
                let shells = pack.map_or(20, |p| p.shells);
                let gd_class = env.objc.get_known_class("GameData", &mut env.mem);
                let shared_sel = env
                    .objc
                    .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                let add_sel = env.objc.register_host_selector(
                    "addVipGoldForBuy:UIUpdate:".to_string(),
                    &mut env.mem,
                );
                let gd: id = crate::objc::msg_send(env, (gd_class, shared_sel));
                if gd != nil {
                    let amount: i32 = shells;
                    let do_update: bool = true;
                    let _: () = crate::objc::msg_send(env, (gd, add_sel, amount, do_update));
                    // [2026-09-16] E-01 去掉「不是充值档位,按旧兜底发放」那半句:这种参数已不会进来。有档位时输出与改动前逐字相同。
                    log!(
                        "[SHELLHOOK] granted {} shells (pack idx {} = ShopItemData.itemid, offline IAP bypass)",
                        amount,
                        item_id
                    );
                    // [扫描修 2026-09-15] F2-1:这个钩子吞掉了原版 IAP 流程,原版「充值成功」的副作用
                    // (-[GameData addAlreadyPurchaseVipgoldWithPurchaseInfo:]@0x7f3bc:gamedataFlag |= 0x20/0x10、
                    // unlockItem:16283 都教授等)离线永远不会发生。只在贝壳确实发放成功(gd != nil)后交给
                    // mole_items 补齐。它内部会发宿主 msg_send、可能改写 r0-r3,所以放在最终写回返回寄存器
                    // 之前调用;onBuyVIPGold: 返回 void,下面统一把 r0/r1 清零,寄存器最终状态与原来一致。
                    // [补完 2026-09-15] 同时把本次购买的 itemid / 实发贝壳数 / 档位(含标价)交给 mole_items,
                    // 替原版服务器做「累计充值 → VIP 等级」(原版 1083 上报 + 1084 回包 parseVipInfo 写三值)。
                    crate::mole_items::on_shells_purchased(env, item_id, amount, pack);
                }
                env.cpu.regs_mut()[0..2].fill(0);
                return;
            }
            // [2026-09-16] E-01 上面没接管的 onBuyVIPGold:(查不到充值档位,即广告墙「免费贝壳」itemid 8):不吞、不发贝壳,
            // 只记一行日志,然后照常派发真方法(原版离线空操作)。只读寄存器,不发消息。
            if name == "NewStyleStoreMainLayer"
                && selector.as_str(&env.mem) == "onBuyVIPGold:"
            {
                log!(
                    "[SHELLHOOK] onBuyVIPGold:{} 不是 100_0.dat 充值档位(广告墙「免费贝壳」),离线/私服下不可用,放行原版:checkAdWallAvailable 为 NO 时什么都不做",
                    env.cpu.regs()[2]
                );
            }
            // -[MagicNumberView onButtonYesSelected:]: the "magic number" gate
            // (a secret-content password prompt). With the bypass on, skip the
            // real password comparison and drive the success path directly —
            // tell the delegate it finished, then close the prompt. Mirrors the
            // user's tweak (%hook MagicNumberView in Tweak.xm).
            if name == "MagicNumberView"
                && selector.as_str(&env.mem) == "onButtonYesSelected:"
                && crate::mole_cheats::magic_bypass_on()
            {
                let recv = receiver;
                let del_sel = env
                    .objc
                    .register_host_selector("magicNumberDelegate".to_string(), &mut env.mem);
                let finish_sel = env
                    .objc
                    .register_host_selector("onMagicNumberFinished".to_string(), &mut env.mem);
                let close_sel = env
                    .objc
                    .register_host_selector("doClose".to_string(), &mut env.mem);
                let del: id = crate::objc::msg_send(env, (recv, del_sel));
                if del != nil {
                    let _: () = crate::objc::msg_send(env, (del, finish_sel));
                }
                let _: () = crate::objc::msg_send(env, (recv, close_sel));
                log!("[MAGICBYPASS] forced magic-number success");
                env.cpu.regs_mut()[0..2].fill(0);
                return;
            }
            // [MoleWorld] Golden Island (加勒比寻宝 Caribbean) offline fix. The
            // activity fetches its state from the now-dead server; with no data
            // CaribbeanMainLayer black-screens. Mirror the user's tweak: serve a
            // locally-built CaribbeanDiscoveringData from the caribbeanData
            // getter, short-circuit the network fetch (return "OK"), and swallow
            // the no-network popup. Gated on a cheap flag so it's free when off.
            if crate::mole_cheats::fix_golden_island_on() {
                if name == "GameData" && selector.as_str(&env.mem) == "caribbeanData" {
                    let data = crate::mole_cheats::build_caribbean_data(env);
                    env.cpu.regs_mut()[0] = data.to_bits();
                    return;
                }
                // The Golden Island activity's artwork (voyage_*.png — board,
                // buttons, ship, islands) was SERVER-DOWNLOADED content that does
                // not exist anywhere offline (verified: 0 such files on disk). So
                // the activity can only ever open as an invisible, touch-swallowing
                // modal whose (also-invisible) close button can't be tapped = the
                // freeze. Until those assets are supplied, decline the open cleanly:
                // close the layer immediately so tapping it in the Action Center
                // bounces back instead of trapping the player.
                if name == "CaribbeanMainLayer"
                    && selector.as_str(&env.mem) == "showLayerWithTarget:selector:"
                {
                    let recv = receiver;
                    if env
                        .objc
                        .object_has_method_named(&env.mem, recv, "closeCaribbeanMainLayer")
                    {
                        let close = env.objc.register_host_selector(
                            "closeCaribbeanMainLayer".to_string(),
                            &mut env.mem,
                        );
                        let _: () = crate::objc::msg_send(env, (recv, close));
                    }
                    log!("[GOLDENISLE] art is server-only (absent offline) — declined open to avoid the touch-freeze");
                    env.cpu.regs_mut()[0..2].fill(0);
                    return;
                }
                if name == "NetworkManager"
                    && selector.as_str(&env.mem) == "getCaribbeanStateInfo:"
                {
                    let recv = receiver;
                    // Build the local state and store it in GameData so the
                    // activity's getter / direct-ivar reads both see valid data.
                    let data = crate::mole_cheats::build_caribbean_data(env);
                    let gd_cls = env.objc.get_known_class("GameData", &mut env.mem);
                    let shared = env
                        .objc
                        .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                    let gd: id = crate::objc::msg_send(env, (gd_cls, shared));
                    if gd != nil && data != nil {
                        let set = env
                            .objc
                            .register_host_selector("setCaribbeanData:".to_string(), &mut env.mem);
                        let _: () = crate::objc::msg_send(env, (gd, set, data));
                    }
                    // CRITICAL un-freeze: showLayerWithTarget put up a modal
                    // LoadingLayer right before this call; offline its dismissal
                    // (onCommandReceived: / onStateChangedTo:8) never fires, so it
                    // blocks the whole UI forever. Dismiss it now.
                    let ll_cls = env.objc.get_known_class("LoadingLayer", &mut env.mem);
                    if ll_cls != nil {
                        let ll_shared = env
                            .objc
                            .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                        let ll: id = crate::objc::msg_send(env, (ll_cls, ll_shared));
                        if ll != nil
                            && env.objc.object_has_method_named(&env.mem, ll, "hideLoadingLayer")
                        {
                            let hide = env
                                .objc
                                .register_host_selector("hideLoadingLayer".to_string(), &mut env.mem);
                            let _: () = crate::objc::msg_send(env, (ll, hide));
                        }
                    }
                    // The server response that would normally call back into the
                    // activity to display never arrives offline — drive the
                    // display now via the registered Caribbean delegate.
                    let del_sel = env.objc.register_host_selector(
                        "delegateCaribbeanActivity".to_string(),
                        &mut env.mem,
                    );
                    let del: id = crate::objc::msg_send(env, (recv, del_sel));
                    if del != nil && env.objc.object_has_method_named(&env.mem, del, "displayUI") {
                        let disp = env
                            .objc
                            .register_host_selector("displayUI".to_string(), &mut env.mem);
                        let _: () = crate::objc::msg_send(env, (del, disp));
                    }
                    log!("[GOLDENISLE] short-circuited getCaribbeanStateInfo: + drove display");
                    env.cpu.regs_mut()[0] = 0; // fake "OK", no network
                    return;
                }
                if name == "CaribbeanMainLayer"
                    && selector.as_str(&env.mem) == "showNetWorkError"
                {
                    env.cpu.regs_mut()[0..2].fill(0); // swallow the no-network popup
                    return;
                }
            }
        }
    }
    maybe_initialize_class(env, receiver);

    // Traverse the chain of superclasses to find the method implementation.

    let mut class = orig_class;
    loop {
        if class == nil {
            assert!(class != orig_class);

            let name: String = {
                let class_host_object = env.objc.get_host_object(orig_class).unwrap();
                let &super::ClassHostObject { ref name, .. } =
                    class_host_object.as_any().downcast_ref().unwrap();
                name.clone()
            };

            // Compatibility shim: instead of aborting the whole emulator when an
            // object doesn't respond to a selector, log it and behave as if the
            // message was sent to nil (return 0). This lets MoleWorld skip the
            // many non-essential calls (analytics/ad SDK helpers, optional UIKit
            // niceties) that would otherwise each crash boot, and keep going
            // toward the first frame. Essential gaps still surface as visibly
            // wrong behavior to investigate.
            // [扫描修 2026-09-15] F10-5:selector.as_str 只求值一次(原来 3 次,每次都要在客体内存里
            // 逐字节找字符串结尾);同一 (类, 选择子) 只在第一次 log!,之后降为 log_dbg!。
            let sel_str = selector.as_str(&env.mem);
            if first_compat_warning("does-not-respond", &name, sel_str) {
                log!(
                    "Warning: {:?} (class \"{}\") does not respond to selector \"{}\"; treating as no-op (nil). [repeats of this class+selector go to log_dbg]",
                    receiver,
                    name,
                    sel_str,
                );
            } else {
                log_dbg!(
                    "Warning: {:?} (class \"{}\") does not respond to selector \"{}\"; treating as no-op (nil).",
                    receiver,
                    name,
                    sel_str,
                );
            }
            // [MoleWorld DIAG] Persist a de-duplicated list of every class+selector
            // that silently no-ops, so a normal play session leaves behind the full
            // set of missing methods to read from /tmp/mole_diag.log.
            crate::mole_diag::log_unique(&name, sel_str);
            let _ = super2;
            // MoleWorld offline port: a guest class whose superclass chain
            // doesn't reach a real -initWithCoder: (some TMMapData* saved-map
            // classes link in a way that misses it) must still return self here,
            // exactly as -[NSObject initWithCoder:] does. Returning nil instead
            // made every decoded saved-map object (buildings, farmland,
            // decorations) come back nil and vanish from a reloaded village.
            if sel_str == "initWithCoder:" {
                env.cpu.regs_mut()[0] = receiver.to_bits();
                env.cpu.regs_mut()[1] = 0;
                return;
            }
            env.cpu.regs_mut()[0..2].fill(0);
            return;
        }

        let host_object = env.objc.get_host_object(class).unwrap();

        if let Some(&super::ClassHostObject {
            superclass,
            ref methods,
            ref name,
            ..
        }) = host_object.as_any().downcast_ref()
        {
            // Skip method lookup on first iteration if this is the super-call
            // variant of objc_msgSend (look up the superclass first)
            if super2.is_some() && class == orig_class {
                class = superclass;
                continue;
            }

            if let Some(imp) = methods.get(&selector) {
                log_dbg!("Found method on: {}", name);
                match imp {
                    IMP::Host(host_imp) => {
                        // TODO: do type checks when calling GuestIMPs too.
                        // That requires using Objective-C type strings,
                        // rather than Rust types, and should probably
                        // warn rather than panicking,
                        // because apps might rely on type punning.
                        if let Some((sent_type_id, sent_type_desc)) = message_type_info {
                            let (expected_type_id, expected_type_desc) = host_imp.type_info();
                            if sent_type_id != expected_type_id {
                                let msg = format!(
                                    "\
Type mismatch when sending message {} to {:?}!
- Message has type: {:?} / {}
- Method expects type: {:?} / {}",
                                    selector.as_str(&env.mem),
                                    receiver,
                                    sent_type_id,
                                    sent_type_desc,
                                    expected_type_id,
                                    expected_type_desc
                                );
                                if tolerate_type_mismatch {
                                    log!("Warning: {}", msg);
                                } else {
                                    panic!("{}", msg);
                                }
                            }
                        }
                        host_imp.call_from_guest(env)
                    }
                    // We can't create a new stack frame, because that would
                    // interfere with pass-through of stack arguments.
                    IMP::Guest(guest_imp) => guest_imp.call_without_pushing_stack_frame(env),
                }
                return;
            } else {
                class = superclass;
            }
        } else if let Some(&super::UnimplementedClass {
            ref name,
            is_metaclass,
        }) = host_object.as_any().downcast_ref()
        {
            // Compatibility shim: don't abort on an unimplemented class (e.g.
            // JSONKit's runtime-created JKArray/JKDictionary). Behave as if the
            // message went to nil so the game keeps booting toward the first
            // frame instead of crashing the emulator.
            // [扫描修 2026-09-15] F10-5:同 does-not-respond,按 (类, 选择子) 去重,首次 log!、之后 log_dbg!。
            let sel_str = selector.as_str(&env.mem);
            if first_compat_warning("unimplemented-class", name, sel_str) {
                log!(
                    "Class \"{}\" ({:?}) is unimplemented; {} method \"{}\" treated as no-op (nil). [repeats of this class+selector go to log_dbg]",
                    name,
                    class,
                    if is_metaclass { "class" } else { "instance" },
                    sel_str,
                );
            } else {
                log_dbg!(
                    "Class \"{}\" ({:?}) is unimplemented; {} method \"{}\" treated as no-op (nil).",
                    name,
                    class,
                    if is_metaclass { "class" } else { "instance" },
                    sel_str,
                );
            }
            env.cpu.regs_mut()[0..2].fill(0);
            return;
        } else if let Some(&super::FakeClass {
            ref name,
            is_metaclass,
        }) = host_object.as_any().downcast_ref()
        {
            // [MoleWorld] 广告/统计/评分 SDK 的 fake class 是【有意 no-op 掉】的(TalkingData 统计、
            // Flurry、iRate 评分弹窗、淘米广告墙…),它们每帧被调几十次,逐次打日志纯属噪音,会把
            // 真正有价值的告警冲掉。这些类静默处理(行为不变,仍返回 nil);其余 fake class 照常打印。
            const SILENT_FAKE_CLASSES: &[&str] = &[
                "TDGAUtility",        // TalkingData 游戏统计
                "TalkingData",
                "TalkingDataGA",
                "TaomeeAnalytics",    // 淘米自家统计
                "Flurry",             // Flurry 统计
                "iRate",              // App Store 评分弹窗
                "AdWallsManager",     // 广告墙
                "AdViewForMoleCart",  // 摩尔卡丁车跨游戏广告
                "GADBannerView",      // Google AdMob
                "GADRequest",
                "GADInterstitial",
                "AtomAdNetworkAdapter",
                // [扫描修 2026-09-15] F10-5:淘米统计 SDK 自带的 SSKeychain,classes.rs 有意把它 fake 掉
                // (没有真钥匙串,离线也不要统计)。一轮启动 19 行 passwordForService:account: /
                // setPassword:forService:account: 全是它。账号菜单模式的 allAccounts 空数组桩在
                // mole_cheats::intercept 里、先于这里执行,不受影响。将来若给它做真钥匙串桩,记得移出名单。
                "TMA_SSKeychain",
            ];
            if !SILENT_FAKE_CLASSES.contains(&name.as_str()) {
                // [扫描修 2026-09-15] F10-5:其余 fake class 按 (类, 选择子) 去重,首次 log!、之后 log_dbg!。
                let sel_str = selector.as_str(&env.mem);
                if first_compat_warning("faked-class", name, sel_str) {
                    log!(
                        "Call to faked class \"{}\" ({:?}) {} method \"{}\". Behaving as if message was sent to nil. [repeats of this class+selector go to log_dbg]",
                        name,
                        class,
                        if is_metaclass { "class" } else { "instance" },
                        sel_str,
                    );
                } else {
                    log_dbg!(
                        "Call to faked class \"{}\" ({:?}) {} method \"{}\". Behaving as if message was sent to nil.",
                        name,
                        class,
                        if is_metaclass { "class" } else { "instance" },
                        sel_str,
                    );
                }
            }
            env.cpu.regs_mut()[0..2].fill(0);
            return;
        } else {
            panic!(
                "Item {class:?} in superclass chain of object {receiver:?}'s class {orig_class:?} has an unexpected host object type."
            );
        }
    }
}

/// Standard variant of `objc_msgSend`. See [objc_msgSend_inner].
#[allow(non_snake_case)]
pub(crate) fn objc_msgSend(env: &mut Environment, receiver: id, selector: SEL) {
    objc_msgSend_inner(
        env, receiver, selector, /* super2: */ None, /* tolerate_type_mismatch: */ false,
    )
}

#[allow(non_snake_case)]
pub(crate) fn _touchHLE_objc_msgSend_tolerant(env: &mut Environment, receiver: id, selector: SEL) {
    objc_msgSend_inner(
        env, receiver, selector, /* super2: */ None, /* tolerate_type_mismatch: */ true,
    )
}

/// Variant of `objc_msgSend` for methods that return a struct via a pointer.
/// See [objc_msgSend_inner].
///
/// The first parameter here is the pointer for the struct return. This is an
/// ABI detail that is usually hidden and handled behind-the-scenes by
/// [crate::abi], but `objc_msgSend` is a special case because of the
/// pass-through behaviour. Of course, the pass-through only works if the [IMP]
/// also has the pointer parameter. The caller therefore has to pick the
/// appropriate `objc_msgSend` variant depending on the method it wants to call.
pub(super) fn objc_msgSend_stret(
    env: &mut Environment,
    _stret: MutVoidPtr,
    receiver: id,
    selector: SEL,
) {
    objc_msgSend_inner(
        env, receiver, selector, /* super2: */ None, /* tolerate_type_mismatch: */ false,
    )
}

#[allow(non_snake_case)]
pub(crate) fn _touchHLE_objc_msgSend_stret_tolerant(
    env: &mut Environment,
    _stret: MutVoidPtr,
    receiver: id,
    selector: SEL,
) {
    objc_msgSend_inner(
        env, receiver, selector, /* super2: */ None, /* tolerate_type_mismatch: */ true,
    )
}

#[repr(C, packed)]
/// A pointer to this struct replaces the normal receiver parameter for
/// `objc_msgSendSuper2` and [msg_send_super2].
pub struct objc_super {
    pub receiver: id,
    /// If this is used with `objc_msgSendSuper` (not implemented here, TODO),
    /// this is a pointer to the superclass to look up the method on.
    /// If this is used with `objc_msgSendSuper2`, this is a pointer to a class
    /// and the superclass will be looked up from it.
    pub class: Class,
}
unsafe impl SafeRead for objc_super {}

/// Variant of `objc_msgSend` for supercalls. See [objc_msgSend_inner].
///
/// This variant has a weird ABI because it needs to receive an additional piece
/// of information (a class pointer), but it can't actually take this as an
/// extra parameter, because that would take one of the argument slots reserved
/// for arguments passed onto the method implementation. Hence the [objc_super]
/// pointer in place of the normal [id].
#[allow(non_snake_case)]
pub(super) fn objc_msgSendSuper2(
    env: &mut Environment,
    super_ptr: ConstPtr<objc_super>,
    selector: SEL,
) {
    let objc_super { receiver, class } = env.mem.read(super_ptr);

    // Rewrite first argument to match the normal ABI.
    crate::abi::write_next_arg(&mut 0, env.cpu.regs_mut(), &mut env.mem, receiver);

    objc_msgSend_inner(
        env,
        receiver,
        selector,
        /* super2: */ Some(class),
        /* tolerate_type_mismatch: */ false,
    )
}

/// Trait that assists with type-checking of [msg_send]'s arguments.
///
/// - Statically constrains the types of [msg_send]'s arguments so that the
///   first two are always [id] and [SEL].
/// - Provides the type ID to enable dynamic type checking of subsequent
///   arguments and the return type.
///
/// See `impl_HostIMP` for implementations. See also [MsgSendSuperSignature].
pub trait MsgSendSignature: 'static {
    /// Get the [TypeId] and a human-readable description for this signature.
    fn type_info() -> (TypeId, &'static str) {
        #[cfg(debug_assertions)]
        let type_name = std::any::type_name::<Self>();
        // Avoid wasting space on type names in release builds. At the time of
        // writing this saves about 36KB.
        #[cfg(not(debug_assertions))]
        let type_name = "[description unavailable in release builds]";
        (TypeId::of::<Self>(), type_name)
    }
}

/// Wrapper around [objc_msgSend] which, together with [msg], makes it easy to
/// send messages in host code. Warning: all types are inferred from the
/// call-site and they may not be checked, so be very sure you get them correct!
pub fn msg_send<R, P>(env: &mut Environment, args: P) -> R
where
    fn(&mut Environment, id, SEL): CallFromHost<R, P>,
    fn(&mut Environment, MutVoidPtr, id, SEL): CallFromHost<R, P>,
    (R, P): MsgSendSignature,
    R: GuestRet,
{
    // Provide type info for dynamic type checking.
    env.objc.message_type_info = Some(<(R, P) as MsgSendSignature>::type_info());
    if R::SIZE_IN_MEM.is_some() {
        (objc_msgSend_stret as fn(&mut Environment, MutVoidPtr, id, SEL)).call_from_host(env, args)
    } else {
        (objc_msgSend as fn(&mut Environment, id, SEL)).call_from_host(env, args)
    }
}

pub fn msg_send_no_type_checking<R, P>(env: &mut Environment, args: P) -> R
where
    fn(&mut Environment, id, SEL): CallFromHost<R, P>,
    fn(&mut Environment, MutVoidPtr, id, SEL): CallFromHost<R, P>,
    (R, P): MsgSendSignature,
    R: GuestRet,
{
    if R::SIZE_IN_MEM.is_some() {
        (_touchHLE_objc_msgSend_stret_tolerant as fn(&mut Environment, MutVoidPtr, id, SEL))
            .call_from_host(env, args)
    } else {
        (_touchHLE_objc_msgSend_tolerant as fn(&mut Environment, id, SEL)).call_from_host(env, args)
    }
}

/// Counterpart of [MsgSendSignature] for [msg_send_super2].
pub trait MsgSendSuperSignature: 'static {
    /// Signature with the [objc_super] pointer replaced by [id].
    type WithoutSuper: MsgSendSignature;
}

/// [msg_send] but for super-calls (calls [objc_msgSendSuper2]). You probably
/// want to use [msg_super] rather than calling this directly.
pub fn msg_send_super2<R, P>(env: &mut Environment, args: P) -> R
where
    fn(&mut Environment, ConstPtr<objc_super>, SEL): CallFromHost<R, P>,
    fn(&mut Environment, MutVoidPtr, ConstPtr<objc_super>, SEL): CallFromHost<R, P>,
    (R, P): MsgSendSuperSignature,
    R: GuestRet,
{
    // Provide type info for dynamic type checking.
    env.objc.message_type_info = Some(<(R, P) as MsgSendSuperSignature>::WithoutSuper::type_info());
    if R::SIZE_IN_MEM.is_some() {
        todo!() // no stret yet
    } else {
        (objc_msgSendSuper2 as fn(&mut Environment, ConstPtr<objc_super>, SEL))
            .call_from_host(env, args)
    }
}

/// Macro for sending a message which imitates the Objective-C messaging syntax.
/// See [msg_send] for the underlying implementation. Warning: all types are
/// inferred from the call-site and they may not be checked, so be very sure you
/// get them correct!
///
/// ```ignore
/// msg![env; foo setBar:bar withQux:qux];
/// ```
///
/// desugars to:
///
/// ```ignore
/// {
///     let sel = env.objc.lookup_selector("setFoo:withBar").unwrap();
///     msg_send(env, (foo, sel, bar, qux))
/// }
/// ```
///
/// Note that argument values that aren't a bare single identifier like `foo`
/// need to be bracketed.
///
/// See also [msg_class], if you want to send a message to a class.
#[macro_export]
macro_rules! msg {
    [$env:expr; $receiver:tt $name:ident $(: $arg1:tt $($($namen:ident)?: $argn:tt)*)?] => {
        {
            let sel = $crate::objc::selector!($($arg1;)? $name $($(, $($namen)?)*)?);
            let sel = $env.objc.lookup_selector(sel)
                .expect("Unknown selector");
            let args = ($receiver, sel, $($arg1, $($argn),*)?);
            $crate::objc::msg_send($env, args)
        }
    }
}
pub use crate::msg; // #[macro_export] is weird...

/// Variant of [msg] for super-calls.
///
/// Unlike the other variants, this macro can only be used within
/// [crate::objc::objc_classes], because it relies on that macro defining a
/// constant containing the name of the current class.
///
/// ```ignore
/// msg_super![env; this init]
/// ```
///
/// desugars to something like this, if the current class is `SomeClass`:
///
/// ```ignore
/// {
///     let super_arg_ptr = push_to_stack(env, objc_super {
///         receiver: this,
///         class: env.objc.get_known_class("SomeClass", &mut env.mem),
///     });
///     let sel = env.objc.lookup_selector("init").unwrap();
///     let res = msg_send_super2(env, (super_arg_ptr, sel));
///     pop_from_stack::<objc_super>(env);
///     res
/// }
/// ```
#[macro_export]
macro_rules! msg_super {
    [$env:expr; $receiver:tt $name:ident $(: $arg1:tt $($($namen:ident)?: $argn:tt)*)?] => {
        {
            let class = $env.objc.get_known_class(
                _OBJC_CURRENT_CLASS,
                &mut $env.mem
            );
            let sel = $crate::objc::selector!($($arg1;)? $name $($(, $($namen)?)*)?);
            let sel = $env.objc.lookup_selector(sel)
                .expect("Unknown selector");

            let sp = &mut $env.cpu.regs_mut()[$crate::cpu::Cpu::SP];
            let old_sp = *sp;
            *sp -= $crate::mem::guest_size_of::<$crate::objc::objc_super>();
            let super_ptr = $crate::mem::Ptr::from_bits(*sp);
            $env.mem.write(super_ptr, $crate::objc::objc_super {
                receiver: $receiver,
                class,
            });

            let args = (super_ptr.cast_const(), sel, $($arg1, $($argn),*)?);
            let res = $crate::objc::msg_send_super2($env, args);

            $env.cpu.regs_mut()[$crate::cpu::Cpu::SP] = old_sp;

            res
        }
    }
}
pub use crate::msg_super; // #[macro_export] is weird...

/// Variant of [msg] for sending a message to a named class. Useful for calling
/// class methods, especially `new`.
///
/// ```ignore
/// msg_class![env; SomeClass alloc]
/// ```
///
/// desugars to:
///
/// ```ignore
/// msg![env; (env.objc.get_known_class("SomeClass", &mut env.mem)) alloc]
/// ```
#[macro_export]
macro_rules! msg_class {
    [$env:expr; $receiver_class:ident $name:ident $(: $arg1:tt $($($namen:ident)?: $argn:tt)*)?] => {
        {
            let class = $env.objc.get_known_class(
                stringify!($receiver_class),
                &mut $env.mem
            );
            $crate::objc::msg![$env; class $name $(: $arg1 $($($namen)?: $argn)*)?]
        }
    }
}
pub use crate::msg_class; // #[macro_export] is weird...

/// Shorthand for `let _: id = msg![env; object retain];`
pub fn retain(env: &mut Environment, object: id) -> id {
    if object == nil {
        // fast path
        return nil;
    }
    msg![env; object retain]
}

/// Shorthand for `() = msg![env; object release];`
pub fn release(env: &mut Environment, object: id) {
    if object == nil {
        // fast path
        return;
    }
    msg![env; object release]
}

/// Shorthand for `let _: id = msg![env; object autorelease];`
pub fn autorelease(env: &mut Environment, object: id) -> id {
    if object == nil {
        // fast path
        return nil;
    }
    msg![env; object autorelease]
}
