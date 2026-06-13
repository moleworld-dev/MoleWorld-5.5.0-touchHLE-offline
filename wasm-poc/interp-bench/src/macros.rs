//! `echo!` 宏的桩实现。
//!
//! 原版 touchHLE 的 `echo!`(在 src/log.rs)会同时写入 stdout 和日志文件。
//! 解释器仅用它输出诊断(derail trace / unimplemented 指令 / heartbeat)。
//! 在这个独立基准 crate 里我们不需要日志系统,做成可选的 eprintln(wasm 下 no-op)。
//!
//! 注意:这是本 PoC 新写的桩,不是从原仓复制的代码。解释器源文件保持逐字不改。

#[macro_export]
macro_rules! echo {
    ($($arg:tt)*) => {{
        // 原生(测试/调试)下打到 stderr;wasm32 下 stderr 不可用,直接吞掉。
        #[cfg(all(not(target_arch = "wasm32"), feature = "echo_stderr"))]
        {
            eprintln!($($arg)*);
        }
        // 即使关闭输出也要"使用"参数,避免 unused 警告。
        #[cfg(not(all(not(target_arch = "wasm32"), feature = "echo_stderr")))]
        {
            let _ = format_args!($($arg)*);
        }
    }};
}
