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
use std::sync::atomic::{AtomicBool, Ordering};

/// [MoleWorld] 递归卫:`-[UserInfoData initWithCoder:]` 的离线贝壳还原拦截内部要再调用
/// 一次真正的 `initWithCoder:`(让原方法把存档各字段解出来),那一次必须落到正常派发、
/// 不能再被本拦截截走,否则无限递归。进入拦截前置 true,放行原方法后置回 false。
static MOLE_IN_UID_INITCODER: AtomicBool = AtomicBool::new(false);

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

/// [MoleWorld iOS · 性能] 沿超类链找到"选择子实现所在的类"。返回值语义与 `objc_msgSend_inner`
/// 主循环一致:含该方法的 [ClassHostObject] 类 / 一个 [super::UnimplementedClass](让主循环按原逻辑
/// 打日志并当 nil)/ 非类宿主对象(让主循环按原逻辑 panic)/ `nil`(整条链都没有)。
fn resolve_class_for_selector(objc: &ObjC, orig_class: Class, selector: SEL, is_super2: bool) -> Class {
    let mut class = orig_class;
    let mut first = true;
    loop {
        if class == nil {
            return nil;
        }
        let Some(host_object) = objc.get_host_object(class) else {
            return class; // 主循环会 unwrap 失败/按原逻辑处理
        };
        if let Some(&super::ClassHostObject { superclass, ref methods, .. }) =
            host_object.as_any().downcast_ref()
        {
            if is_super2 && first {
                first = false;
                class = superclass;
                continue;
            }
            first = false;
            if methods.contains_key(&selector) {
                return class;
            }
            class = superclass;
        } else {
            return class; // UnimplementedClass 或意外类型:交给主循环
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
/// [hang debug] 全局 msgSend 计数 + 最后出帧时的计数(见环形缓冲 dump)。
pub static MSG_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static LAST_PRESENT_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// present 时调用:记下当前 msgSend 计数,用于检测"出帧失速"死循环。
pub fn note_present() {
    LAST_PRESENT_N.store(
        MSG_N.load(std::sync::atomic::Ordering::Relaxed),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn objc_msgSend_inner(
    env: &mut Environment,
    receiver: id,
    selector: SEL,
    super2: Option<Class>,
    tolerate_type_mismatch: bool,
) {
    crate::mole_perf::MSGSEND.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    // [MoleWorld] Same idea, but for a non-nil receiver whose isa has NO registered
    // host object (an unresolvable / garbage "class"). The offline port fakes some
    // class methods to return a non-nil sentinel that the game then messages as if it
    // were a real object — seen crashing the 好友 (friend) screen. get_class_name
    // (called just below for the cheat intercept, and by the dispatcher's
    // "does not respond to selector" path) would `expect()`-panic
    // ("Could not get class name!") on such a class. Treat it as a message to nil.
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
    // [hang debug] msgSend 环形缓冲 + 出帧失速触发 dump。每条 msgSend 记 (类指针,选择子指针)
    // 进 512 环;present 时记下当前 msgSend 计数(note_present)。若 ~80 万次 msgSend 没出过帧
    // = 死循环 → dump 环里最近 512 条(按时间序,不去重)→ 暴露好友面板入口 + 死循环体的真实
    // 方法序列(穿透 objc 跳板,因为这是在派发处记的)。每个失速 episode 只 dump 一次。
    #[cfg(any(feature = "interp_hb", debug_assertions))]
    {
        use std::cell::RefCell;
        use std::sync::atomic::Ordering;
        thread_local! {
            static RING: RefCell<Vec<(u32, u32)>> = RefCell::new(Vec::new());
            static RPOS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
            static DUMPED_AT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        }
        let n = MSG_N.fetch_add(1, Ordering::Relaxed);
        RING.with(|r| {
            let mut r = r.borrow_mut();
            if r.len() < 512 {
                r.push((orig_class.to_bits(), selector.to_bits()));
            } else {
                let p = RPOS.with(|c| {
                    let x = c.get();
                    c.set((x + 1) % 512);
                    x
                });
                r[p] = (orig_class.to_bits(), selector.to_bits());
            }
        });
        if n & 0x0001_ffff == 0 {
            let last_present = LAST_PRESENT_N.load(Ordering::Relaxed);
            let stale = n.wrapping_sub(last_present);
            let already = DUMPED_AT.with(|c| c.get());
            if stale > 800_000 && last_present != already {
                DUMPED_AT.with(|c| c.set(last_present));
                let entries: Vec<(u32, u32)> = RING.with(|r| r.borrow().clone());
                let start = RPOS.with(|c| c.get());
                echo!("[MSGRING] >>> 出帧失速 {} 次msgSend无present,dump最近 {} 条(时间序):", stale, entries.len());
                let len = entries.len();
                for i in 0..len {
                    let (cb, sb) = entries[(start + i) % len];
                    let class_id: id = crate::mem::Ptr::from_bits(cb);
                    let cls = env.objc.try_get_class_name(class_id).unwrap_or("?").to_string();
                    let selp: crate::mem::ConstPtr<u8> = crate::mem::Ptr::from_bits(sb);
                    let sel = env.mem.cstr_at_utf8(selp).unwrap_or("?").to_string();
                    echo!("[MSGRING] {:03} [{} {}]", i, cls, sel);
                }
                echo!("[MSGRING] <<< end");
            }
        }
    }
    // [MoleWorld] Debug-menu cheat toggles (free shop, multipliers, force VIP,
    // anti-cheat off). Gated by a cheap any_enabled() check so the hot path pays
    // nothing when all cheats are off. intercept() may fully handle the call
    // (return) or tweak an argument register and let the real method run.
    if crate::mole_cheats::any_enabled() {
        // [MoleWorld iOS · 性能] ★原本每条 objc 消息都做【两次堆分配 String】(类名 + 选择子)。
        // any_enabled() 在本移植里恒为真(破解开关默认开),所以这是【每条消息】的固定成本,
        // 在 60 万消息/秒量级下相当可观。改为:先用【已注册 SEL 指针的集合】做 O(1) 整数快判定,
        // 只有可能命中 intercept 的选择子才付字符串化的代价。行为等价(intercept 的每条分支都是
        // `sel == "..."` 形式,选择子不在集合里就不可能命中任何分支)。
        // [MoleWorld 宽屏适配·热路径] 虚拟世界换算(已右移根层的 position/setPosition:、已右移 UIKit
        // 子视图的 frame/setFrame:、白名单代码的触摸/世界坐标换算、dealloc 除名):SEL 指针快判定、零分配,
        // 没有任何登记对象时只付一次原子读。★`message_type_info.is_some()` = 本条消息由**宿主** msg_send
        // 发出(宿主 msg_send 设置它、guest 派发恒为 None):宿主发起时 LR 是陈旧的 main 返回地址,不能拿来
        // 判定"谁在问",且宿主必须看到真实坐标。
        if crate::mole_cheats::intercept_fast(env, selector, message_type_info.is_some()) {
            return;
        }
        if crate::mole_cheats::is_intercept_sel(&mut env.objc, &mut env.mem, selector) {
            let class_name = env.objc.get_class_name(orig_class).to_string();
            let sel_str = selector.as_str(&env.mem).to_string();
            if crate::mole_cheats::intercept(env, &class_name, &sel_str) {
                return;
            }
        }
    }
    // [MoleWorld] Offline-play functional hooks. Gated on a cheap SELECTOR
    // pre-check FIRST: selectors are interned (one canonical pointer per name),
    // so `is_mole_hook_sel` matches the incoming selector against the precomputed
    // hook-selector set by pointer identity (a few integer compares). The hot
    // path is dominated by non-hook selectors (release/zOrder/compare:/draw/
    // visit/transform/...); for those `.filter(|_| is_mole_hook)` makes the
    // `if let` fall through, skipping the ClassHostObject downcast and the whole
    // chain of class-name / selector-string compares below. Behaviour is
    // identical to the old class-name-first form: every hook below is
    // `name == X && selector == Y`, so a message whose selector isn't in the set
    // could never have matched any hook. (The earlier broad
    // [SCENE]/[FLOW]/[COCOS]/[TOUCHDISP] tracing probes that ran selector.as_str
    // + dozens of string compares on EVERY message were already removed.)
    let is_mole_hook = env.objc.is_mole_hook_sel(&mut env.mem, selector);
    // [MoleWorld iOS perf · 点好友卡死根治] 跳过冗余的每帧头像 ASprite 重建。
    // -[AnimManager updateTick:] 每帧无条件对每个 AnimInstance 发 "render" → -[AnimPlayer render]
    // → -[ASprite PaintAFrame…] removeAllChildrenWithCleanup + 为该帧每个 module 新建 CCSprite。
    // 动画帧每秒才变几次,却每显示帧都重建一份一模一样的 sprite = 纯浪费;JIT 桌面无感,无 JIT 的
    // iOS 解释器上单帧 drawScene 跑不完 = present 冻结 = 卡死。这里:动画状态没变就跳过(不派发真
    // IMP),帧一变就照常重建 → 视觉零差异、解释器从"冻结"变"流畅"。详见 mole_cheats::anim_render_should_skip。
    // 用 selector 指针 + orig_class 指针双重快判定(都在 is_mole_hook 命中后才做,非 hook 选择子零成本),
    // 放在 downcast 块之前,避免与下方 `name` 对 env.objc 的借用纠缠。
    if is_mole_hook {
        let render_sel = env
            .objc
            .register_host_selector("render".to_string(), &mut env.mem);
        if selector == render_sel {
            let ap_cls = env.objc.get_known_class("AnimPlayer", &mut env.mem);
            if orig_class == ap_cls && crate::mole_cheats::anim_render_should_skip(env, receiver) {
                env.cpu.regs_mut()[0] = 0;
                return;
            }
        } else if {
            // [MoleWorld iOS · P0 从好友村返回后主村只剩背景] 保护那份【地图数据字典】不被原地清空。
            // 实测(纯读 host 字典 count):首次进村 -[GameManager loadMapFromData:] 拿到的字典 0x30017440
            // count=7;从好友村返回时【同一个指针】count 变成 0 → -[GameManager
            // loadMapFromData:selector:mapData:forNPC:] 在 0x20b16 处命中 `count==0` 早退 → 直接调回调返回,
            // 一个 loadMapObjects: 都不执行 → 地面/建筑/人物/村庄UI 全不加载,只剩背景装饰。
            // 这里按【receiver 指针精确比对】吞掉对那一个字典的 removeAllObjects(只影响它,不动别的容器),
            // 使返回时数据仍在 → 早退不再命中 → 走正常加载路径。仅离线生效。
            let mapdata_ptr = crate::mole_cheats::MAPDATA_PTR
                .load(std::sync::atomic::Ordering::Relaxed);
            mapdata_ptr != 0
                && receiver.to_bits() == mapdata_ptr
                && !env.options.network_access
                && selector
                    == env
                        .objc
                        .register_host_selector("removeAllObjects".to_string(), &mut env.mem)
        } {
            use std::sync::atomic::{AtomicU32, Ordering as O2};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, O2::Relaxed);
            if n < 4 {
                log!(
                    "[MOLECHEAT] 保护地图数据字典 {:?}:吞掉第 {} 次 removeAllObjects(防返回主村空村)",
                    receiver, n
                );
            }
            env.cpu.regs_mut()[0] = 0;
            return;
        } else {
            // 每帧 drawScene 入口复位头像重建预算(早于本帧 updateTick→render 遍历)。不拦截,照常派发。
            let draw_scene_sel = env
                .objc
                .register_host_selector("drawScene".to_string(), &mut env.mem);
            if selector == draw_scene_sel {
                // [性能观测] 每帧一次;内部 5 秒节流打一行 [PERF] 汇总。
                crate::mole_perf::tick(env.objc.object_count());
                crate::mole_cheats::anim_render_reset_frame_budget();
                // [诊断] 每帧 drawScene 推进看门狗帧计数(证明 guest 还在出帧)。无条件,不依赖 island。
                crate::mole_cheats::watchdog_frame();
            }
        }
    }
    if let Some(ho) = env.objc.get_host_object(orig_class).filter(|_| is_mole_hook) {
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
                drop(message_type_info);
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
            // -[UserInfoData initWithCoder:]: 离线"忠于原版"地还原贝壳(vipGold)。
            //
            // 这个破解包(无限贝壳版)在读本地存档(NSKeyedUnarchiver → initWithCoder:)时,
            // 把贝壳写死成 2097151(0x1FFFFF):VA 0xb9ce0
            //   `[self setNewVipGold:[CryptUtils encryptInt:2097151]]`
            // 同一函数里 curLevel/xp/gold/totalWorkers 等其它字段统统老老实实
            // `decodeIntForKey:` 读存档真实值,唯独 vipGold 被塞死常量 → 每次进游戏都把贝壳
            // 覆写成 2097151(全二进制仅此一处 0x1FFFFF;2.4.3 零此常量、且根本没有
            // setNewVipGold: → 实锤是破解作者新加的私货,作者原话"重进游戏贝壳数自动变 2097151")。
            //
            // 我们要的是"离线、但贝壳像原版一样从存档自然持久化、能花能存",而不是每次被强制重置。
            // 做法:拦截 initWithCoder:,先放行原方法整段跑完(所有字段照常解出,包括那个被塞死的
            // 2097151),拿到返回的 self 后,用作者删掉的那行原始逻辑——
            //   `[self setNewVipGold:[CryptUtils encryptInt:[coder decodeIntForKey:@"vipGold"]]]`
            // ——把贝壳改回存档里的真实值。archive key 实测为 "vipGold"(AES 解密玩家 userinfo.dat
            // 的 NSKeyedArchiver 键名:username/userId/curLevel/xp/gold/vipGold/totalWorkers/…)。
            // setNewVipGold: 存的是密文(getter 用 CryptUtils decryptInt: 解),所以要先 encryptInt:
            // 再写,与原版完全一致。
            //
            // 只拦"读本地存档"这一条路径(initWithCoder:);联网同步 decodeRemoteData:/排行榜/
            // copyWithZone:/调试菜单里也会用到 setNewVipGold:,那些一律放行不动。占卜扣费走
            // [WrapperManager addVipGold:]、T 菜单加贝壳同样走 addVipGold:,都读 vipGold 真实值,
            // 不受影响。MOLE_IN_UID_INITCODER 是递归卫(定义见文件顶部)。
            if name == "UserInfoData"
                && !MOLE_IN_UID_INITCODER.load(Ordering::Relaxed)
                && selector.as_str(&env.mem) == "initWithCoder:"
            {
                let recv = receiver;
                // initWithCoder: 的 NSCoder 实参在 r2(r0=self, r1=_cmd, r2=arg1);
                // 必须在任何 msg_send 之前读出(后续调用会覆写寄存器)。
                let coder: id = MutPtr::from_bits(env.cpu.regs()[2]);
                drop(message_type_info);
                // 1) 放行原 initWithCoder:(递归卫=true → 落正常派发跑原方法)。
                MOLE_IN_UID_INITCODER.store(true, Ordering::Relaxed);
                let result: id = crate::objc::msg_send(env, (recv, selector, coder));
                MOLE_IN_UID_INITCODER.store(false, Ordering::Relaxed);
                // 2) 把 vipGold 从"塞死的 2097151"还原成存档真实值(= 作者删掉的那行)。
                if result != nil && coder != nil {
                    let key = crate::frameworks::foundation::ns_string::from_rust_string(
                        env,
                        "vipGold".to_string(),
                    );
                    let decode_sel = env
                        .objc
                        .register_host_selector("decodeIntForKey:".to_string(), &mut env.mem);
                    let real_vip: i32 = crate::objc::msg_send(env, (coder, decode_sel, key));
                    let crypt_cls = env.objc.get_known_class("CryptUtils", &mut env.mem);
                    if crypt_cls != nil {
                        let enc_sel = env
                            .objc
                            .register_host_selector("encryptInt:".to_string(), &mut env.mem);
                        let enc_vip: i32 =
                            crate::objc::msg_send(env, (crypt_cls, enc_sel, real_vip));
                        let set_sel = env
                            .objc
                            .register_host_selector("setNewVipGold:".to_string(), &mut env.mem);
                        let _: () = crate::objc::msg_send(env, (result, set_sel, enc_vip));
                        log!(
                            "[贝壳] 读档:跳过破解版强制 2097151,vipGold 还原为存档真实值 {}(保持加密持久化)",
                            real_vip
                        );
                    } else {
                        log!("[贝壳] 警告:未找到 CryptUtils 类,vipGold 未还原(仍为破解版默认)");
                    }
                }
                // 3) 返回原 initWithCoder: 的 self(上面的 msg_send 已覆写 r0,这里写回)。
                env.cpu.regs_mut()[0] = result.to_bits();
                return;
            }
            // -[IMCommonMgr checkUpdates:]: kicks off +[CryptUtils doCipher:...]
            // on network data that's empty offline, computing a negative (huge
            // unsigned) buffer size that corrupts memory. Pure analytics/update
            // feature — make it a no-op offline.
            if name == "IMCommonMgr" && selector.as_str(&env.mem) == "checkUpdates:" {
                env.cpu.regs_mut()[0..2].fill(0);
                return;
            }
            // ===== [MoleWorld offline port] Dead-SDK AppDelegate wrapper no-ops. =====
            // applicationDidFinishLaunching calls these wrappers, which fan out to
            // analytics/ad SDKs that are 100% dead offline. Faking the SDK leaf
            // classes (classes.rs) stops the leaves, but the GAME's own wrapper
            // method still runs (builds args, NSBundle/NSURL round-trips, several
            // outer msgSends, per-FakeClass log spam). Cut the whole wrapper here:
            //   umengTrack            -> bare NSURLConnection ping to log.umtrack.com
            //                            (NO fakeable class — MUST cut here).
            //   umengAnalyze          -> MobClick startWithAppkey: chain.
            //   startTaomeeAndFlurryStatisticsSession -> Flurry + Taomee session.
            //   reportAppOpenToAdMob  -> background sync request to a.admob.com
            //                            (offline timeout). Cut to skip the stall.
            // All four return void; call sites don't read the result (verified).
            // NOTE: these 4 selectors are registered in is_mole_hook_sel's
            // HOOK_SEL_NAMES (selectors.rs), else is_mole_hook gates this block out.
            if name == "iMoleVillageAppDelegate"
                && matches!(
                    selector.as_str(&env.mem),
                    "umengTrack"
                        | "umengAnalyze"
                        | "startTaomeeAndFlurryStatisticsSession"
                        | "reportAppOpenToAdMob"
                )
            {
                env.cpu.regs_mut()[0..2].fill(0); // no-op (void)
                return;
            }
            // +[CrashLog initCrashLogNotShowViewWithDelegate:andGameType:] @0x53dfc0:
            // the Flurry/Quincy crash-reporter entry called in didFinishLaunching.
            // Faking BWQuincyManager (classes.rs) already neuters the core, but this
            // wrapper still builds args + drives the install. void return, result
            // unused at the call site (decomp) -> cut the whole wrapper here.
            if name == "CrashLog"
                && selector.as_str(&env.mem) == "initCrashLogNotShowViewWithDelegate:andGameType:"
            {
                env.cpu.regs_mut()[0..2].fill(0); // no-op (void)
                return;
            }
            // +[AdWallsManager init*] @0x39ecb4..0x39ed68: per-ad-network boot setup
            // (Tapjoy/Taomee/MiDi/PunchBox + taomeeAnalytics) called synchronously in
            // didFinishLaunching. All void, results unused (decomp). Offline-dead: the
            // ad walls themselves are user-triggered and gated by isReachable, never
            // shown offline. NOTE: do NOT touch sharedInstance/setRootViewController:
            // — the game uses the AdWallsManager singleton (so the class is NOT faked);
            // only these boot init* class methods are cut.
            if name == "AdWallsManager"
                && matches!(
                    selector.as_str(&env.mem),
                    "initTaomee"
                        | "taomeeAnalytics"
                        | "initMiDi"
                        | "initPunchBox"
                        | "initTapjoyRequestInAppDelegate"
                )
            {
                env.cpu.regs_mut()[0..2].fill(0); // no-op (void)
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
                    drop(message_type_info);
                    () = crate::objc::msg_send(env, (recv, sel));
                    return;
                }
            }
            // The start-screen's secondary buttons (客服 / 换账号 / 换玩家 / 版本)
            // are all server-dependent and do nothing useful offline. Repurpose
            // whichever the user taps into a one-tap player-save reset, guarded by
            // a native confirmation dialog. (设置/帮助 keep their normal behaviour.)
            if name == "LogoLayer"
                && matches!(
                    selector.as_str(&env.mem),
                    "onMenuKefuSelected"
                        | "onMenuChangeAccountSelected"
                        | "onMenuChangePlayerSelected"
                        | "onMenuVersionInfoSelected"
                )
            {
                crate::save_reset::confirm_and_reset_saves(env);
                env.cpu.regs_mut()[0..2].fill(0);
                return;
            }
            // -[NewStyleStoreMainLayer onBuyVIPGold:]: the 贝壳 (shell) packs are
            // real-money StoreKit IAP gated by network reachability — both dead in
            // this offline port, so a shell-pack tap normally just shows a
            // "no network" box and never credits anything. Per user request, make
            // it a free local purchase: credit shells via
            // -[GameData addVipGoldForBuy:UIUpdate:] (adds to vip_gold + refreshes
            // the HUD) and skip the dead IAP path entirely.
            if name == "NewStyleStoreMainLayer"
                && selector.as_str(&env.mem) == "onBuyVIPGold:"
            {
                // onBuyVIPGold:(int 档位索引):regs[2] = 包索引(0..=6;原版 cmp r2,8)。
                // 按原版各档真实贝壳数发放(20/105/225/370/650/1500/3500),不再死值 1000。
                // 必须在任何 msg_send 前读 regs[2],否则被覆盖。
                let pack_idx = env.cpu.regs()[2] as usize;
                const SHELL_PACKS: [i32; 7] = [20, 105, 225, 370, 650, 1500, 3500];
                let shells = SHELL_PACKS.get(pack_idx).copied().unwrap_or(20);
                let gd_class = env.objc.get_known_class("GameData", &mut env.mem);
                let shared_sel = env
                    .objc
                    .register_host_selector("sharedInstance".to_string(), &mut env.mem);
                let add_sel = env.objc.register_host_selector(
                    "addVipGoldForBuy:UIUpdate:".to_string(),
                    &mut env.mem,
                );
                drop(message_type_info);
                let gd: id = crate::objc::msg_send(env, (gd_class, shared_sel));
                if gd != nil {
                    let amount: i32 = shells;
                    let do_update: bool = true;
                    let _: () = crate::objc::msg_send(env, (gd, add_sel, amount, do_update));
                    log!(
                        "[SHELLHOOK] granted {} shells (pack idx {}, offline IAP bypass)",
                        amount, pack_idx
                    );
                }
                env.cpu.regs_mut()[0..2].fill(0);
                return;
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
                drop(message_type_info);
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
                    drop(message_type_info);
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
                    drop(message_type_info);
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
                    drop(message_type_info);
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

    // [MoleWorld iOS · 性能] 方法解析缓存(见 ObjC::method_cache):村里每秒 94 万条消息,原来每条
    // 都沿超类链逐级 get_host_object + methods.get(两次 SipHash/级,cocos2d 层级 3~6 级)。
    // 现在:缓存命中 → 直接从"实现所在的类"开始,下面的循环第一轮就命中;未命中 → 走一遍链并回填。
    let cache_key = (orig_class.to_bits(), selector.to_bits(), super2.is_some());
    let resolved: Class = {
        let epoch = super::methods::METHOD_TABLE_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
        if env.objc.method_cache_epoch != epoch {
            env.objc.method_cache.clear();
            env.objc.method_cache_epoch = epoch;
        }
        match env.objc.method_cache.get(&cache_key) {
            Some(&c) => c,
            None => {
                let r = resolve_class_for_selector(&env.objc, orig_class, selector, super2.is_some());
                env.objc.method_cache.insert(cache_key, r);
                r
            }
        }
    };
    let mut class = resolved;
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
            // [MoleWorld iOS · 性能] ★这条原本【完全不去重】,而每条 log! 都要 format + NSLog +
            // write + fsync(见 log.rs)。只要有任何一个选择子每帧落空(实测 statusBarFrame 就是),
            // 就是每帧一次 fsync —— 这是全进程唯一【没有上界】的每次调用开销。
            // 改为按 (类名, 选择子) 去重:首次打全文,之后只累加计数、每 4096 次打一行汇总。
            {
                use std::collections::HashSet;
                use std::sync::Mutex;
                static SEEN: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);
                static SUPPRESSED: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let sel_s = selector.as_str(&env.mem).to_string();
                let first = {
                    let mut g = match SEEN.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    g.get_or_insert_with(HashSet::new)
                        .insert((name.to_string(), sel_s.clone()))
                };
                if first {
                    log!(
                        "Warning: {:?} (class \"{}\") does not respond to selector \"{}\"; treating as no-op (nil). [此后同类只计数]",
                        receiver, name, sel_s,
                    );
                } else {
                    let n = SUPPRESSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if n % 4096 == 0 {
                        log!("(已静默 {} 次重复的 \"不响应选择子\" 警告)", n);
                    }
                }
            }
            // [MoleWorld DIAG] Persist a de-duplicated list of every class+selector
            // that silently no-ops, so a normal play session leaves behind the full
            // set of missing methods to read from /tmp/mole_diag.log.
            crate::mole_diag::log_unique(&name, selector.as_str(&env.mem));
            let _ = super2;
            // MoleWorld offline port: a guest class whose superclass chain
            // doesn't reach a real -initWithCoder: (some TMMapData* saved-map
            // classes link in a way that misses it) must still return self here,
            // exactly as -[NSObject initWithCoder:] does. Returning nil instead
            // made every decoded saved-map object (buildings, farmland,
            // decorations) come back nil and vanish from a reloaded village.
            if selector.as_str(&env.mem) == "initWithCoder:" {
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
            log!(
                "Class \"{}\" ({:?}) is unimplemented; {} method \"{}\" treated as no-op (nil).",
                name,
                class,
                if is_metaclass { "class" } else { "instance" },
                selector.as_str(&env.mem),
            );
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
            ];
            if !SILENT_FAKE_CLASSES.contains(&name.as_str()) {
                log!(
                    "Call to faked class \"{}\" ({:?}) {} method \"{}\". Behaving as if message was sent to nil.",
                    name,
                    class,
                    if is_metaclass { "class" } else { "instance" },
                    selector.as_str(&env.mem),
                );
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
