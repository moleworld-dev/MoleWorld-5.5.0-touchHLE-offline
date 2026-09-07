/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [MoleWorld iOS · 性能观测底座]
//!
//! 目的:把"70% CPU / 110 秒涨 156MB"从估算变成**数字**。只用原子计数器(零锁、零分配、
//! 热路径上每次一条 relaxed 原子加),每 5 秒在 `drawScene` 处打**一行**汇总,绝不每帧打日志。
//!
//! 四组指标:
//! 1. GL 调用直方图:每秒 draw / TexImage2D / CompressedTexImage2D / BindTexture 次数。
//!    → 回答"驱动开销是调用量大还是单次贵"。
//! 2. GL 纹理字节记账:上传字节累计 / 当前存活估算(TexImage 加、DeleteTextures 减)。
//!    → 回答"内存增长里 GL 纹理占多少"。
//! 3. objc 派发计数:每秒消息数。→ 派发固定开销的基数。
//! 4. iOS 进程内存四分量(`task_vm_info`):phys_footprint / internal / compressed / external。
//!    → **internal 大 = guest 脏页高水位;external 大 = GL/驱动**。一眼分账。
//!    另打 objc 活对象数:**若单调增长,说明游戏自身的 ~100 处纹理清理根本没跑到**。

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub static DRAWS: AtomicU64 = AtomicU64::new(0);
pub static TEXIMAGE: AtomicU64 = AtomicU64::new(0);
pub static TEXSUBIMAGE: AtomicU64 = AtomicU64::new(0);
pub static COMPRESSED_TEX: AtomicU64 = AtomicU64::new(0);
pub static BINDTEX: AtomicU64 = AtomicU64::new(0);
pub static BINDTEX_SKIPPED: AtomicU64 = AtomicU64::new(0);
/// 所有经 gles_guest 进入的 guest GL 调用总数(含被跳过的)。
pub static GLCALLS: AtomicU64 = AtomicU64::new(0);
pub static TEX_BYTES_UPLOADED: AtomicU64 = AtomicU64::new(0);
/// 当前存活纹理字节估算(上传加、删除减;删除时用最后一次上传该 id 的大小)。
pub static TEX_BYTES_LIVE: AtomicU64 = AtomicU64::new(0);
pub static MSGSEND: AtomicU64 = AtomicU64::new(0);
/// [MoleWorld iOS · JIT 值不值得的判据] 花在**执行 guest ARM 指令**上的纳秒累计
/// (即 `Cpu::run_or_step` 内部;宿主框架实现、objc 派发、GL 驱动都不在内)。
/// 它除以墙钟就是「解释器占比」——也就是**换成一个无限快的 JIT 最多能省掉的比例**(Amdahl 上限)。
/// 自身开销:每次 run_or_step 两次 Instant::now(),实测量级约 1%,已知并接受。
pub static GUEST_NS: AtomicU64 = AtomicU64::new(0);
/// ★默认**关闭**:计时本身有代价——run_or_step 在每次 SVC(每个宿主函数调用,不只是 objc 消息)返回一次,
/// 每秒被调数百万次,两次 Instant::now() 实测把 60fps 压到 42fps。所以只在需要取证时开:
/// 容器里放 `Documents/mole_cpu_share` → 下次启动生效。常态成本 = 每次调用一次 relaxed 原子读。
pub static MEASURE_GUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 启动时读一次触发文件。
pub fn init_guest_measure() {
    let on = crate::paths::user_data_base_path().join("mole_cpu_share").exists();
    MEASURE_GUEST.store(on, Relaxed);
    if on {
        log!("[PERF] guest 指令占比测量已开启(会拖慢约三成,仅用于取证)");
    }
}
pub static FRAMES: AtomicU64 = AtomicU64::new(0);

/// 纹理 id → 最近一次上传的字节数(供 DeleteTextures 扣减)。只在 GL 线程访问。
thread_local! {
    static TEX_SIZE: std::cell::RefCell<std::collections::HashMap<u32, u64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 估算一次 TexImage2D 的字节数(按 format/type 粗算;够用于记账)。
pub fn tex_bytes(width: i32, height: i32, format: u32, type_: u32) -> u64 {
    use crate::gles::gles11_raw as gl;
    let px = (width.max(0) as u64) * (height.max(0) as u64);
    let bpp: u64 = match type_ {
        gl::UNSIGNED_SHORT_5_6_5 | gl::UNSIGNED_SHORT_4_4_4_4 | gl::UNSIGNED_SHORT_5_5_5_1 => 2,
        _ => match format {
            gl::RGBA | gl::BGRA_EXT => 4,
            gl::RGB => 3,
            gl::LUMINANCE_ALPHA => 2,
            gl::ALPHA | gl::LUMINANCE => 1,
            _ => 4,
        },
    };
    px * bpp
}

pub fn note_teximage(texture: u32, bytes: u64) {
    TEXIMAGE.fetch_add(1, Relaxed);
    TEX_BYTES_UPLOADED.fetch_add(bytes, Relaxed);
    TEX_SIZE.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(old) = m.insert(texture, bytes) {
            // 同一 id 重传:先扣旧的
            TEX_BYTES_LIVE.fetch_sub(old.min(TEX_BYTES_LIVE.load(Relaxed)), Relaxed);
        }
    });
    TEX_BYTES_LIVE.fetch_add(bytes, Relaxed);
}

pub fn note_delete_texture(texture: u32) {
    TEX_SIZE.with(|m| {
        if let Some(b) = m.borrow_mut().remove(&texture) {
            TEX_BYTES_LIVE.fetch_sub(b.min(TEX_BYTES_LIVE.load(Relaxed)), Relaxed);
        }
    });
}

/// iOS 进程内存四分量(MB):(phys_footprint, internal, compressed, external)。非 iOS 返回 None。
#[cfg(target_os = "ios")]
fn vm_info_mb() -> Option<(f64, f64, f64, f64)> {
    // task_vm_info 的布局(mach/task_info.h),只声明到我们要的字段之后即可,
    // 用 count 限定内核填多少。
    #[repr(C)]
    #[derive(Default)]
    struct TaskVmInfo {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(target: u32, flavor: u32, info: *mut u8, count: *mut u32) -> i32;
    }
    const TASK_VM_INFO: u32 = 22;
    let mut info = TaskVmInfo::default();
    let mut count: u32 = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            TASK_VM_INFO,
            (&mut info as *mut TaskVmInfo).cast::<u8>(),
            &mut count,
        )
    };
    if kr != 0 {
        return None;
    }
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    Some((
        mb(info.phys_footprint),
        mb(info.internal),
        mb(info.compressed),
        mb(info.external),
    ))
}
#[cfg(not(target_os = "ios"))]
fn vm_info_mb() -> Option<(f64, f64, f64, f64)> {
    None
}

/// 每帧(drawScene)调一次;内部按 5 秒节流打一行汇总。`objc_objects` = 当前 objc 活对象数。
pub fn tick(objc_objects: usize) {
    use std::sync::Mutex;
    use std::time::Instant;
    FRAMES.fetch_add(1, Relaxed);
    static LAST: Mutex<Option<(Instant, [u64; 10])>> = Mutex::new(None);
    let now = Instant::now();
    let cur = [
        FRAMES.load(Relaxed),
        DRAWS.load(Relaxed),
        TEXIMAGE.load(Relaxed),
        COMPRESSED_TEX.load(Relaxed),
        BINDTEX.load(Relaxed),
        MSGSEND.load(Relaxed),
        TEXSUBIMAGE.load(Relaxed),
        GLCALLS.load(Relaxed),
        BINDTEX_SKIPPED.load(Relaxed),
        GUEST_NS.load(Relaxed),
    ];
    let mut g = match LAST.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some((t0, prev)) = *g else {
        *g = Some((now, cur));
        return;
    };
    let dt = now.duration_since(t0).as_secs_f64();
    if dt < 5.0 {
        return;
    }
    *g = Some((now, cur));
    drop(g);
    let per_s = |i: usize| (cur[i] - prev[i]) as f64 / dt;
    let fps = per_s(0);
    let mem = vm_info_mb()
        .map(|(f, i, c, e)| format!("footprint={f:.0}MB internal={i:.0} compressed={c:.0} external={e:.0}"))
        .unwrap_or_else(|| "mem=n/a".into());
    // guest 指令执行占墙钟的比例 = 完美 JIT 的收益上限
    let guest_pct = (cur[9] - prev[9]) as f64 / (dt * 1e9) * 100.0;
    let guest_txt = if MEASURE_GUEST.load(Relaxed) {
        format!(" | guest指令={guest_pct:.1}%(JIT收益上限)")
    } else {
        String::new()
    };
    echo!(
        "[PERF] fps={fps:.1}{guest_txt} | 每帧: gl={:.0} draw={:.0} texImg={:.1} texSub={:.1} cmpTex={:.1} bindTex={:.0}(跳过{:.0}) msg={:.0} | 每秒 msg={:.0} | 纹理上传累计={:.0}MB 存活≈{:.0}MB | objc对象={} | {mem}",
        per_s(7) / fps.max(0.01),
        per_s(1) / fps.max(0.01),
        per_s(2) / fps.max(0.01),
        per_s(6) / fps.max(0.01),
        per_s(3) / fps.max(0.01),
        per_s(4) / fps.max(0.01),
        per_s(8) / fps.max(0.01),
        per_s(5) / fps.max(0.01),
        per_s(5),
        TEX_BYTES_UPLOADED.load(Relaxed) as f64 / 1048576.0,
        TEX_BYTES_LIVE.load(Relaxed) as f64 / 1048576.0,
        objc_objects,
    );
}
