/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [MoleWorld] 自定义「帮助 / 关于」页内容。
//!
//! 游戏原本的帮助页是一个 UIWebView 加载 Word 导出的 `mole_help.html`(FAQ +
//! 官方技术支持 + 版权)。touchHLE 没有 HTML 引擎(见 `ui_web_view.rs`),原本只是
//! 把 HTML 抽成纯文本显示在白底上。这里在加载到 `mole_help` 时,改为渲染本移植
//! 项目的「关于」信息:作者 / 官方交流群 / 当前运行硬件(CPU、GPU、分辨率、系统
//! 时间与时区)/ 贡献者 / 版本 / 版权。背景沿用原版暖黄色(从原版截图采样)。

use crate::Environment;
use std::sync::Mutex;

/// 原版帮助页背景的暖黄色(从原版截图 `screenshot/..._IMG_0031.PNG` 采样:
/// RGB 239,170,66)。
pub const HELP_BG_RGB: (f32, f32, f32) = (239.0 / 255.0, 170.0 / 255.0, 66.0 / 255.0);

/// 启动时缓存的 GL 驱动描述("VERSION / VENDOR / RENDERER")。由 `window.rs` 在
/// 创建 GLES1 上下文时(那时上下文已 current,`glGetString` 安全)写入,避免在
/// 渲染帮助页时再去碰 GL(可能没有 current 上下文)。
static GPU_DESC: Mutex<Option<String>> = Mutex::new(None);

/// 由 `window.rs` 在拿到 `driver_description()` 后调用,缓存起来供「关于」页用。
pub fn set_gpu_desc(desc: String) {
    if let Ok(mut g) = GPU_DESC.lock() {
        *g = Some(desc);
    }
}

/// 启动时缓存的「游戏本体」标识(显示名 + 版本 + bundle id)。由 `lib.rs` 在打印
/// App bundle info 后写入,供诊断块 / panic 日志使用(那时还没有 `Environment`)。
static GAME_VERSION: Mutex<Option<String>> = Mutex::new(None);

/// 由 `lib.rs` 在拿到 bundle 信息后调用,缓存「游戏本体」标识。
pub fn set_game_version(v: String) {
    if let Ok(mut g) = GAME_VERSION.lock() {
        *g = Some(v);
    }
}

fn game_version() -> String {
    GAME_VERSION
        .try_lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "MoleWorld 5.5.0 (com.taomee.MoleWorld)".to_string())
}

/// [crash log] 运行「足迹」环形缓冲:在关键生命周期节点记一条,崩溃时(panic 钩子)
/// 回放最近若干条,快速看出「崩溃前都干了什么」。用 `Vec`(`Vec::new()` 是 const)而非
/// `VecDeque`(其 const new 较新),容量到顶就丢最旧一条。
static CRUMBS: Mutex<Vec<String>> = Mutex::new(Vec::new());
const CRUMB_CAP: usize = 32;

/// 记录一个运行里程碑(面包屑)。① 存入有界缓冲供 panic 回放;② 立即 `echo!` 落盘,
/// 这样即使是原生崩溃(Windows SEH,不便在崩溃处读锁),`[足迹]` 行也已在日志里、
/// 可肉眼回溯到最后一步。里程碑应是低频事件(生命周期节点),不要放进每帧热路径。
pub fn milestone(msg: &str) {
    let (date, _) = local_time_and_tz();
    // date = "YYYY-MM-DD HH:MM:SS";取后段 "HH:MM:SS"(ASCII,按字节切安全)。
    let hms = if date.len() >= 19 { &date[11..19] } else { date.as_str() };
    if let Ok(mut q) = CRUMBS.lock() {
        if q.len() >= CRUMB_CAP {
            q.remove(0);
        }
        q.push(format!("{hms}  {msg}"));
    }
    echo!("[足迹] {}", msg);
}

/// 回放最近的足迹(供 panic 钩子)。用 `try_lock` 避免万一在持锁时 panic 造成死锁。
pub fn breadcrumbs_dump() -> String {
    match CRUMBS.try_lock() {
        Ok(q) if !q.is_empty() => q.join("\n"),
        Ok(_) => "(无足迹记录)".to_string(),
        Err(_) => "(足迹缓冲忙)".to_string(),
    }
}

/// 从驱动描述里取出 GPU(renderer)那一段。`driver_description` 用 " / " 连接
/// VERSION / VENDOR / RENDERER,所以取最后一段即 renderer(GPU 名)。
/// 注意按 " / "(带空格)切分:renderer 自身可能含无空格的 '/'(如
/// "NVIDIA GeForce RTX 3080/PCIe/SSE2")。
fn gpu_name() -> String {
    let desc = GPU_DESC
        .try_lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    if desc.is_empty() {
        return "(未知)".to_string();
    }
    desc.rsplit(" / ").next().unwrap_or(&desc).trim().to_string()
}

/// 宿主 CPU 型号(尽力而为;失败回退到架构 + 核心数)。
fn cpu_model() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Some(s) = sysctl_string("machdep.cpu.brand_string") {
            if !s.is_empty() {
                return s;
            }
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if let Ok(txt) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in txt.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let k = k.trim();
                    if k == "model name" || k == "Hardware" || k == "Processor" {
                        let v = v.trim();
                        if !v.is_empty() {
                            return v.to_string();
                        }
                    }
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(v) = std::env::var("PROCESSOR_IDENTIFIER") {
            if !v.is_empty() {
                return v;
            }
        }
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    format!("{} · {} 核", std::env::consts::ARCH, cores)
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut size: libc::size_t = 0;
    unsafe {
        if libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buf = vec![0u8; size];
        if libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

/// 宿主操作系统名称 + 版本(尽力而为)。用于诊断块,方便定位「哪类系统出问题」。
#[cfg(target_os = "macos")]
fn os_name_version() -> String {
    let arch = std::env::consts::ARCH;
    let ver = sysctl_string("kern.osproductversion").unwrap_or_default();
    let build = sysctl_string("kern.osversion").unwrap_or_default();
    match (ver.is_empty(), build.is_empty()) {
        (false, false) => format!("macOS {ver} ({build}, {arch})"),
        (false, true) => format!("macOS {ver} ({arch})"),
        _ => format!("macOS ({arch})"),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn os_name_version() -> String {
    let arch = std::env::consts::ARCH;
    if let Ok(txt) = std::fs::read_to_string("/etc/os-release") {
        for line in txt.lines() {
            if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    return format!("{v} ({arch})");
                }
            }
        }
    }
    // 回退:uname。Android 没有 /etc/os-release 时也能给出内核串。
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) == 0 {
            let sys = cstr_to_string(u.sysname.as_ptr());
            let rel = cstr_to_string(u.release.as_ptr());
            if !sys.is_empty() {
                return format!("{sys} {rel} ({arch})");
            }
        }
    }
    format!("Linux ({arch})")
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn cstr_to_string(p: *const libc::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// Windows:从已加载的 ntdll 动态解析 `RtlGetVersion`(`GetVersionExW` 在无 manifest 时
/// 会被系统「撒谎」封顶到 6.2,`RtlGetVersion` 给真实版本/构建号)。只读现成模块句柄,
/// 不新增加载;任何一步失败都回退到通用串。
#[cfg(target_os = "windows")]
fn os_name_version() -> String {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    #[repr(C)]
    #[allow(dead_code)] // 部分字段仅为匹配 OSVERSIONINFOW 的内存布局/大小。
    struct OsVersionInfoW {
        dw_os_version_info_size: u32,
        dw_major_version: u32,
        dw_minor_version: u32,
        dw_build_number: u32,
        dw_platform_id: u32,
        sz_csd_version: [u16; 128],
    }
    let arch = std::env::consts::ARCH;
    let fallback = || format!("Windows ({arch})");
    unsafe {
        // "ntdll.dll\0" 的 UTF-16。ntdll 始终已加载,GetModuleHandleW 拿现成句柄。
        let name: Vec<u16> = "ntdll.dll".encode_utf16().chain(std::iter::once(0)).collect();
        let h = GetModuleHandleW(name.as_ptr());
        if h.is_null() {
            return fallback();
        }
        // PCSTR = *const u8;字节串带结尾 NUL。
        let Some(proc) = GetProcAddress(h, b"RtlGetVersion\0".as_ptr()) else {
            return fallback();
        };
        let rtl_get_version: unsafe extern "system" fn(*mut OsVersionInfoW) -> i32 =
            std::mem::transmute(proc);
        let mut info: OsVersionInfoW = std::mem::zeroed();
        info.dw_os_version_info_size = std::mem::size_of::<OsVersionInfoW>() as u32;
        if rtl_get_version(&mut info) != 0 {
            return fallback();
        }
        let (major, minor, build) = (
            info.dw_major_version,
            info.dw_minor_version,
            info.dw_build_number,
        );
        let name = if major == 10 && build >= 22000 {
            "Windows 11"
        } else if major == 10 {
            "Windows 10"
        } else if major == 6 && minor == 3 {
            "Windows 8.1"
        } else if major == 6 && minor == 2 {
            "Windows 8"
        } else if major == 6 && minor == 1 {
            "Windows 7"
        } else {
            "Windows"
        };
        format!("{name} {major}.{minor} (build {build}, {arch})")
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
)))]
fn os_name_version() -> String {
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}

/// 本地日期时间 + 时区。unix(macOS / Linux / Android)用 libc(含 `tm_gmtoff`,
/// 能给出 UTC 偏移);其它平台(Windows)回退到 UTC 民用时间。
fn local_time_and_tz() -> (String, String) {
    #[cfg(unix)]
    unsafe {
        let t: libc::time_t = libc::time(std::ptr::null_mut());
        let mut tmv: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tmv);
        let date = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tmv.tm_year + 1900,
            tmv.tm_mon + 1,
            tmv.tm_mday,
            tmv.tm_hour,
            tmv.tm_min,
            tmv.tm_sec
        );
        let off = tmv.tm_gmtoff as i64;
        let sign = if off >= 0 { '+' } else { '-' };
        let a = off.abs();
        let tz = format!("UTC{}{:02}:{:02}", sign, a / 3600, (a % 3600) / 60);
        (date, tz)
    }
    #[cfg(not(unix))]
    {
        // 纯 std 回退:用 epoch 秒算 UTC 民用日期(Howard Hinnant days->civil)。
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let days = secs.div_euclid(86400);
        let rem = secs.rem_euclid(86400);
        let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        let z = days + 719468;
        let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        let date = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, mi, s);
        (date, "UTC".to_string())
    }
}

/// 组装「关于」页全文(供 `ui_web_view` 在加载 `mole_help` 时渲染)。
pub fn mole_help_text(env: &mut Environment) -> String {
    let (_, _, vw, vh) = env.window().viewport();
    let cpu = cpu_model();
    let gpu = gpu_name();
    let (time, tz) = local_time_and_tz();
    // 移植发布版本号:CI 在 tag 上构建时 GITHUB_REF_NAME = 该 tag(如 v0.0.3-beta),
    // 天然「随 GitHub build 更新」;本地 / 分支构建回退到常量。
    // (刻意不用 crate::VERSION/git describe:touchHLE 子目录残留内嵌 .git,本地
    //  git describe 会命中 touchHLE 上游版本 v0.2.3 而非本项目 tag。)
    let ver = crate::GITHUB_REF_NAME
        .filter(|s| s.starts_with('v'))
        .unwrap_or("v0.0.3 beta");

    format!(
        "\
摩尔庄园HD 移动版 · 离线移植(基于 touchHLE)

作者:@小丑猫      GitHub:https://github.com/Shad0w23333
官方交流群:【摩尔庄园HD·庄园钉子户】 578867042

—— 运行环境 ——
CPU:{cpu}
GPU:{gpu}
分辨率:{vw} × {vh}
系统时间:{time}({tz})

—— 贡献者 ——
· 哔哩哔哩 @萌新迎风听雨(安装包与思路)
· Never.(教程)   ·   touchHLE(模拟器内核)
· @Ross74U(Arch Linux 测试 / 编译)
· 小小摩尔 QQ 群 @EdmundDHow(赞助)
· 哔哩哔哩 @叔权(B 站 / 小红书宣发、镜像打包)
· 平行摩尔(52摩尔) · 小小摩尔 · 摩尔新桃源社区
· 淘米《摩尔庄园》原作团队

Version 5.5.0 · 移植版 {ver}

Made with love by @Shad0w2333 aka 小丑猫 · 2026.6.1

版权所有 上海淘米公司,如有侵权,尽请谅解!
Copyright © 2012 Shanghai Shengran Information Technology Co., Ltd. All Rights Reserved"
    )
}

/// [crash log] 启动 / 崩溃诊断块:把「移植版本 / 游戏本体 / 操作系统 / CPU / GPU /
/// 系统时间时区」汇成一个方便用户复制粘贴的可读方框。`window.rs` 在 GL 初始化后输出一次;
/// panic 钩子也会输出一次(确保崩溃日志自带机器信息)。
pub fn diag_block() -> String {
    let ver = crate::GITHUB_REF_NAME
        .filter(|s| s.starts_with('v'))
        .unwrap_or("v0.0.3 beta");
    let game = game_version();
    let os = os_name_version();
    let cpu = cpu_model();
    let gpu = gpu_name();
    let (time, tz) = local_time_and_tz();
    format!(
        "\
┌──────────── 摩尔庄园HD · 离线移植运行诊断(touchHLE)────────────
│ 移植版本 : {ver}
│ 游戏本体 : {game}
│ 操作系统 : {os}
│ CPU      : {cpu}
│ GPU      : {gpu}
│ 系统时间 : {time} ({tz})
└─────────────────────────────────────────────────────────────────"
    )
}
