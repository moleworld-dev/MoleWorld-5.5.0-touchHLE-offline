/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! WASM 专属:`corosensei` 的 API 兼容实现,基于 **emscripten fibers**(仅 `wasm32`)。
//!
//! `corosensei` 没有 wasm 后端。这里用 emscripten 的 fiber API
//! (`emscripten_fiber_init` / `emscripten_fiber_swap`,需链接 `-sASYNCIFY`)实现
//! 与 `corosensei` 同形状的 `Coroutine` / `Yielder` / `CoroutineResult`,让 guest 线程
//! 调度(`environment.rs` 用)在浏览器里真正能栈切换。
//!
//! 模型(非嵌套调度,touchHLE 的 host↔单协程往返 + 调度器逐个 resume):
//! - 一个 host fiber(代表 host 主上下文,首次 resume 时 `init_from_current_context`)。
//! - 每个 `Coroutine` 一个 fiber(独立 C 栈 + asyncify 栈),entry = `coro_entry`。
//! - `resume(input)`:host→coro 栈切换,把 input 放进槽;coro `suspend`/返回时切回 host。
//! - 传值用协程内的三个槽 `input`/`yielded`/`returned`(避免把 Return 类型塞进 Yielder)。

use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;

// ===========================================================================
// emscripten fiber FFI
// ===========================================================================

/// 对应 C 的 `emscripten_fiber_t`(emscripten/fiber.h)。7 个指针 = 28 字节(wasm32)。
/// 字段由 emscripten 填写,我们只需保证布局/大小正确且地址稳定。
#[repr(C)]
struct EmFiber {
    stack_base: *mut c_void,
    stack_limit: *mut c_void,
    stack_ptr: *mut c_void,
    entry: *mut c_void,
    user_data: *mut c_void,
    asyncify_stack_ptr: *mut c_void,
    asyncify_stack_limit: *mut c_void,
}

impl EmFiber {
    const fn zeroed() -> EmFiber {
        EmFiber {
            stack_base: std::ptr::null_mut(),
            stack_limit: std::ptr::null_mut(),
            stack_ptr: std::ptr::null_mut(),
            entry: std::ptr::null_mut(),
            user_data: std::ptr::null_mut(),
            asyncify_stack_ptr: std::ptr::null_mut(),
            asyncify_stack_limit: std::ptr::null_mut(),
        }
    }
}

extern "C" {
    fn emscripten_fiber_init(
        fiber: *mut EmFiber,
        entry_func: extern "C" fn(*mut c_void),
        entry_func_arg: *mut c_void,
        c_stack: *mut c_void,
        c_stack_size: usize,
        asyncify_stack: *mut c_void,
        asyncify_stack_size: usize,
    );
    fn emscripten_fiber_init_from_current_context(
        fiber: *mut EmFiber,
        asyncify_stack: *mut c_void,
        asyncify_stack_size: usize,
    );
    fn emscripten_fiber_swap(old_fiber: *mut EmFiber, new_fiber: *mut EmFiber);
}

/// 每个 fiber 的 C 栈大小(host 侧 Rust:解释器 + frameworks 调用深度可观)。
const C_STACK_SIZE: usize = 8 * 1024 * 1024;
/// 每个 fiber 的 asyncify 栈大小(保存 async 展开/重绕状态)。
const ASYNCIFY_STACK_SIZE: usize = 1024 * 1024;

// ===========================================================================
// host fiber(thread-local,代表 host 主上下文)
// ===========================================================================

thread_local! {
    static HOST_FIBER: Cell<*mut EmFiber> = const { Cell::new(std::ptr::null_mut()) };
}

/// 取(必要时惰性创建)当前线程的 host fiber。首次调用时把"当前上下文"捕获成一个 fiber,
/// 之后协程 `suspend`/返回都切回它。fiber 结构体与其 asyncify 栈都 leak 成 'static(进程内存活)。
fn host_fiber_ptr() -> *mut EmFiber {
    HOST_FIBER.with(|cell| {
        let p = cell.get();
        if !p.is_null() {
            return p;
        }
        // 首次:分配 host fiber + asyncify 栈,从当前上下文初始化。
        let fiber = Box::into_raw(Box::new(EmFiber::zeroed()));
        let asyncify = vec![0u8; ASYNCIFY_STACK_SIZE].into_boxed_slice();
        let asyncify = Box::leak(asyncify);
        unsafe {
            emscripten_fiber_init_from_current_context(
                fiber,
                asyncify.as_mut_ptr() as *mut c_void,
                ASYNCIFY_STACK_SIZE,
            );
        }
        cell.set(fiber);
        fiber
    })
}

// ===========================================================================
// Coroutine
// ===========================================================================

/// 协程内部状态(堆分配,地址稳定——fiber/栈指针/user_data 全指向它)。
struct CoroutineImpl<Input, Yield, Return> {
    fiber: EmFiber,
    // 栈:保持所有权使其与 fiber 同生命周期(地址稳定)。
    _c_stack: Box<[u8]>,
    _asyncify_stack: Box<[u8]>,
    /// 入口闭包(首次 resume 时被 coro_entry 取走运行)。
    closure: Option<Box<dyn FnOnce(&Yielder<Yield, Input>, Input) -> Return>>,
    /// host→coro:resume 传入的值。
    input: Option<Input>,
    /// coro→host:suspend 让出的值。
    yielded: Option<Yield>,
    /// coro→host:闭包返回的值。
    returned: Option<Return>,
}

/// 对应 `corosensei::Coroutine<Input, Yield, Return>`。
pub struct Coroutine<Input, Yield, Return> {
    imp: Box<CoroutineImpl<Input, Yield, Return>>,
}

/// fiber 入口:泛型实例化后取其函数指针交给 emscripten。运行入口闭包,结束后把返回值放进
/// `returned` 槽并永久切回 host(fiber entry 不允许返回)。
extern "C" fn coro_entry<Input, Yield, Return>(arg: *mut c_void) {
    let imp = unsafe { &mut *(arg as *mut CoroutineImpl<Input, Yield, Return>) };
    let f = imp.closure.take().expect("coroutine entry without closure");
    let input = imp.input.take().expect("coroutine entry without input");
    let yielder = Yielder {
        coro_fiber: &mut imp.fiber as *mut EmFiber,
        yielded_slot: &mut imp.yielded as *mut Option<Yield>,
        input_slot: &mut imp.input as *mut Option<Input>,
        _phantom: PhantomData,
    };
    let ret = f(&yielder, input);
    imp.returned = Some(ret);
    // 闭包已返回:永久切回 host。若被误 resume(corosensei 语义禁止),再切回。
    let coro_fiber = &mut imp.fiber as *mut EmFiber;
    loop {
        unsafe { emscripten_fiber_swap(coro_fiber, host_fiber_ptr()) };
    }
}

impl<Input, Yield, Return> Coroutine<Input, Yield, Return> {
    /// 对应 `corosensei::Coroutine::new`。闭包签名与 corosensei 一致。
    pub fn new<F>(f: F) -> Self
    where
        F: FnOnce(&Yielder<Yield, Input>, Input) -> Return + 'static,
        Input: 'static,
        Yield: 'static,
        Return: 'static,
    {
        let c_stack = vec![0u8; C_STACK_SIZE].into_boxed_slice();
        let asyncify_stack = vec![0u8; ASYNCIFY_STACK_SIZE].into_boxed_slice();
        let mut imp = Box::new(CoroutineImpl {
            fiber: EmFiber::zeroed(),
            _c_stack: c_stack,
            _asyncify_stack: asyncify_stack,
            closure: Some(Box::new(f)),
            input: None,
            yielded: None,
            returned: None,
        });
        // 初始化 fiber:entry = coro_entry::<I,Y,R>,user_data = imp 自身地址。
        let user_data = (&mut *imp) as *mut CoroutineImpl<Input, Yield, Return> as *mut c_void;
        let c_ptr = imp._c_stack.as_mut_ptr() as *mut c_void;
        let a_ptr = imp._asyncify_stack.as_mut_ptr() as *mut c_void;
        let fiber_ptr = &mut imp.fiber as *mut EmFiber;
        unsafe {
            emscripten_fiber_init(
                fiber_ptr,
                coro_entry::<Input, Yield, Return>,
                user_data,
                c_ptr,
                C_STACK_SIZE,
                a_ptr,
                ASYNCIFY_STACK_SIZE,
            );
        }
        Coroutine { imp }
    }

    /// 对应 `corosensei::Coroutine::resume`。host→coro 栈切换。
    pub fn resume(&mut self, input: Input) -> CoroutineResult<Yield, Return> {
        self.imp.input = Some(input);
        let host = host_fiber_ptr();
        let coro = &mut self.imp.fiber as *mut EmFiber;
        unsafe { emscripten_fiber_swap(host, coro) };
        // 协程让出或返回后回到这里。
        if let Some(ret) = self.imp.returned.take() {
            CoroutineResult::Return(ret)
        } else if let Some(y) = self.imp.yielded.take() {
            CoroutineResult::Yield(y)
        } else {
            unreachable!("[wasm_coro] 协程切回但既未 yield 也未 return")
        }
    }
}

// ===========================================================================
// Yielder
// ===========================================================================

/// 对应 `corosensei::Yielder<Yield, Resume>`。只持有 suspend 所需的槽与 fiber 指针
/// (不含 Return 类型),由 coro_entry 构造后传给闭包。
pub struct Yielder<Yield, Resume> {
    coro_fiber: *mut EmFiber,
    yielded_slot: *mut Option<Yield>,
    input_slot: *mut Option<Resume>,
    _phantom: PhantomData<(Yield, Resume)>,
}

impl<Yield, Resume> Yielder<Yield, Resume> {
    /// 对应 `corosensei::Yielder::suspend`。coro→host 栈切换,返回下一次 resume 的输入。
    pub fn suspend(&self, val: Yield) -> Resume {
        unsafe { *self.yielded_slot = Some(val) };
        let host = host_fiber_ptr();
        unsafe { emscripten_fiber_swap(self.coro_fiber, host) };
        unsafe { (*self.input_slot).take().expect("[wasm_coro] resume 未提供输入") }
    }

    /// 对应 `corosensei::Yielder::on_parent_stack`。emscripten fiber 模型下,直接在当前
    /// (协程)栈执行 f——C 栈已给得很大(8MiB),不需要真正切到父栈避免溢出。
    pub fn on_parent_stack<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        f()
    }
}

/// 对应 `corosensei::CoroutineResult<Yield, Return>`。
pub enum CoroutineResult<Yield, Return> {
    Yield(Yield),
    Return(Return),
}
