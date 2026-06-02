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

/// 从驱动描述里取出 GPU(renderer)那一段。`driver_description` 用 " / " 连接
/// VERSION / VENDOR / RENDERER,所以取最后一段即 renderer(GPU 名)。
/// 注意按 " / "(带空格)切分:renderer 自身可能含无空格的 '/'(如
/// "NVIDIA GeForce RTX 3080/PCIe/SSE2")。
fn gpu_name() -> String {
    let desc = GPU_DESC
        .lock()
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
    let ver = crate::VERSION;

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
