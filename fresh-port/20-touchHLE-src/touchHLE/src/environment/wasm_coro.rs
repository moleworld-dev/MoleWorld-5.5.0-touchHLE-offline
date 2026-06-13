/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! WASM 专属:`corosensei` 的 API 兼容 shim(仅 `#[cfg(target_arch = "wasm32")]`)。
//!
//! `corosensei` 0.3.2 没有 wasm/emscripten 后端(它的 `-> !` 发散汇编 trampoline
//! 在 wasm 下编不过)。这里提供与 `environment.rs` 用到的同样 API 形状
//! (`Coroutine` / `Yielder` / `CoroutineResult` / `new` / `resume` / `suspend`),
//! 让 crate 在 wasm 下**编译通过**。
//!
//! ⚠️ M0 阶段:`resume`/`suspend` 的实现是占位 `unimplemented!`——一旦 guest 真要
//! 切换线程就会 panic。真正的栈切换(emscripten fibers 或 JSPI)是 **M1** 的工作。
//! 现阶段目标只是「crate 能编到 wasm + 加载」,不是「能跑 guest 线程」。

use std::marker::PhantomData;

/// 对应 `corosensei::Coroutine<Input, Yield, Return>`。
///
/// M0 stub:`new` 接收闭包但不真正建栈;`resume` 直接 panic。保留泛型形状使
/// `environment.rs` 的类型别名 `HostContext = Coroutine<Environment, Environment, Environment>`
/// 与各调用点逐字编译通过。
pub struct Coroutine<Input, Yield, Return> {
    _marker: PhantomData<(Input, Yield, Return)>,
}

impl<Input, Yield, Return> Coroutine<Input, Yield, Return> {
    /// 对应 `corosensei::Coroutine::new`。M0:接收闭包但丢弃(不建独立栈)。
    /// 闭包签名与 corosensei 一致:`FnOnce(&Yielder<Yield, Input>, Input) -> Return`。
    pub fn new<F>(_f: F) -> Self
    where
        F: FnOnce(&Yielder<Yield, Input>, Input) -> Return + 'static,
    {
        Coroutine {
            _marker: PhantomData,
        }
    }

    /// 对应 `corosensei::Coroutine::resume`。M0:未实现(切换 guest 线程触发)。
    pub fn resume(&mut self, _input: Input) -> CoroutineResult<Yield, Return> {
        unimplemented!(
            "[wasm M0] guest 协程切换尚未实现 —— 等 M1 接 emscripten fibers / JSPI"
        )
    }
}

/// 对应 `corosensei::Yielder<Yield, Resume>`。
pub struct Yielder<Yield, Resume> {
    _marker: PhantomData<(Yield, Resume)>,
}

impl<Yield, Resume> Yielder<Yield, Resume> {
    /// 对应 `corosensei::Yielder::suspend`。M0:未实现。
    pub fn suspend(&self, _val: Yield) -> Resume {
        unimplemented!(
            "[wasm M0] guest 协程 yield 尚未实现 —— 等 M1 接 emscripten fibers / JSPI"
        )
    }

    /// 对应 `corosensei::Yielder::on_parent_stack`(在父栈上执行闭包以避免子栈溢出)。
    /// M0 stub:直接在当前栈执行 f(无栈切换)。语义上够用——只在协程真正运行后才会被
    /// 调到,而协程切换本身 M1 才实现。
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
