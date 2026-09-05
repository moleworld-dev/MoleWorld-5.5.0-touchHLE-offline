/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Paths for host files used by touchHLE: settings, fonts, etc.
//!
//! There are three categories of files:
//!
//! * Resources bundled with touchHLE that neither touchHLE nor the user should
//!   modify: [DYLIBS_DIR], [FONTS_DIR], [DEFAULT_OPTIONS_FILE]. Depending on
//!   the platform these may or may not be ordinary files, and must be accessed
//!   through [ResourceFile].
//! * Files the user is expected to modify, but not touchHLE: [APPS_DIR],
//!   [USER_OPTIONS_FILE], [WALLPAPER_FILES]. These are ordinary files and are
//!   found in [user_data_base_path].
//! * Files that touchHLE will create and modify, and the user may modify if
//!   they want to: [SANDBOX_DIR]. These are ordinary files and are found in
//!   [user_data_base_path].
//!
//! See also [crate::fs], which provides a virtual filesystem for the guest app
//! and defines path types.

use std::borrow::Cow;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

/// Name of the directory containing ARMv6 dynamic libraries bundled with
/// touchHLE.
pub const DYLIBS_DIR: &str = "touchHLE_dylibs";

/// Name of the directory containing fonts bundled with touchHLE.
pub const FONTS_DIR: &str = "touchHLE_fonts";

/// Name of the file containing touchHLE's default options for various apps.
pub const DEFAULT_OPTIONS_FILE: &str = "touchHLE_default_options.txt";

/// macOS-only: If touchHLE is located in a .app bundle, return the path of the
/// Resources directory. If touchHLE is not located in a .app bundle, return
/// [None].
#[allow(dead_code)]
fn get_macos_bundled_resources_path() -> Option<PathBuf> {
    // [MoleWorld iOS] iOS 的 .app bundle 是扁平结构:SDL 的 base_path() 就是 bundle
    // 根目录,touchHLE 的资源(字体等)和内置游戏都直接放在那里(不像 macOS 的
    // Contents/Resources)。把它当资源根返回 → ResourceFile 从 bundle 读资源,且
    // user_data_base_path 因这里 is_some() 会改用可写的 pref_path 存档(iOS 沙盒里
    // bundle 只读,存档必须落到 Documents/Library)。
    #[cfg(target_os = "ios")]
    {
        return sdl2::filesystem::base_path().ok().map(PathBuf::from);
    }
    #[cfg(not(target_os = "ios"))]
    {
        if std::env::consts::OS != "macos" {
            return None;
        }
        let base_path = PathBuf::from(sdl2::filesystem::base_path().ok()?);
        if base_path.file_name().is_some_and(|p| p == "Resources") {
            Some(base_path)
        } else {
            None
        }
    }
}

/// Abstraction over a platform-specific type for accessing a resource bundled
/// with touchHLE.
pub struct ResourceFile {
    #[cfg(target_os = "android")]
    file: sdl2::rwops::RWops<'static>,
    #[cfg(not(target_os = "android"))]
    file: std::fs::File,
}
impl ResourceFile {
    pub fn open(path: &str) -> Result<Self, String> {
        // On Android, these resources are included as "assets" within the APK.
        // We access them via SDL2's wrapper of Android's assets API.
        #[cfg(target_os = "android")]
        let file = sdl2::rwops::RWops::from_file(path, "r")?;

        // On other OSes, resources are accessed as ordinary files.
        #[cfg(not(target_os = "android"))]
        let file = {
            let base_path = get_macos_bundled_resources_path();
            // When not in a bundle, look in the current directory.
            let base = base_path.as_deref().unwrap_or(Path::new("."));
            match std::fs::File::open(base.join(path)) {
                Ok(f) => f,
                // [MoleWorld iOS] App Store 不允许 bundle 里有松散 .dylib(guest 库会触发
                // ITMS-90171「不允许独立库」/ 90209「段对齐」)。所以 TestFlight 包把这些
                // guest 库打进 touchHLE_dylibs.zip(上传校验不扫 zip 内部),松散文件就不存在
                // 了——这里按 basename 从该 zip 提取到可写临时目录再打开。开发侧载仍带松散
                // 文件(命中上面的 Ok),此回退不触发,无回归。
                Err(e) => {
                    if path.starts_with(DYLIBS_DIR) {
                        open_from_dylibs_zip(base, path).ok_or_else(|| e.to_string())?
                    } else {
                        return Err(e.to_string());
                    }
                }
            }
        };
        Ok(Self { file })
    }
    pub fn get(&mut self) -> &mut (impl Read + Seek) {
        &mut self.file
    }
}

/// [MoleWorld iOS] 从 bundle 根的 touchHLE_dylibs.zip 里按 basename 提取一个 guest 动态库
/// 到可写临时目录并打开(见 ResourceFile::open 的注释)。提取结果缓存,后续直接复用。
#[cfg(not(target_os = "android"))]
fn open_from_dylibs_zip(base: &Path, path: &str) -> Option<std::fs::File> {
    let name = Path::new(path).file_name()?.to_str()?;
    let cache = std::env::temp_dir().join("touchHLE_dylibs").join(name);
    if cache.is_file() {
        return std::fs::File::open(&cache).ok();
    }
    let zip_file = std::fs::File::open(base.join("touchHLE_dylibs.zip")).ok()?;
    let mut archive = zip::ZipArchive::new(zip_file).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    if let Some(dir) = cache.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    std::fs::write(&cache, &buf).ok()?;
    std::fs::File::open(&cache).ok()
}
impl std::fmt::Debug for ResourceFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "ResourceFile")
    }
}

/// Whether various resources are in user-accessible files. If they aren't,
/// touchHLE has to be able to display their license terms.
pub const RESOURCES_ARE_EXTERNAL_FILES: bool = cfg!(not(target_os = "android"));

/// Name of the directory where the user can put apps if they want them to
/// appear in the app picker.
pub const APPS_DIR: &str = "touchHLE_apps";

/// Name of the file intended for the user's own options.
pub const USER_OPTIONS_FILE: &str = "touchHLE_options.txt";

/// Names of files the user can put a wallpaper image (for the app picker) in.
#[allow(unused)]
pub const WALLPAPER_FILES: &[&str] = &[
    "touchHLE_wallpaper.png",
    "touchHLE_wallpaper.jpg",
    "touchHLE_wallpaper.jpeg",
];

/// Name of the directory where touchHLE will store sandboxed app data, e.g.
/// the `Documents` directory.
pub const SANDBOX_DIR: &str = "touchHLE_sandbox";

/// Get a platform-specific base path needed for accessing touchHLE's
/// user-modifiable files. This is empty on platforms other than Android.
pub fn user_data_base_path() -> Cow<'static, Path> {
    #[cfg(target_os = "android")]
    unsafe {
        // This is an exception to the rule that SDL2 should only be used
        // directly from src/window.rs. This is just too distant from windowing
        // to belong there.

        // Android storage has evolved in a quite messy fashion. Both "internal
        // storage" and "external storage" (aka the "SD card") are likely to be
        // internal on a modern device, as absurd as that might sound. SDL2 has
        // APIs to get paths for both. We use the "external storage" because
        // it's more likely to be user-accessible.
        extern "C" {
            fn SDL_AndroidGetExternalStoragePath() -> *const std::ffi::c_char;
        }
        let path = SDL_AndroidGetExternalStoragePath();
        if path.is_null() {
            log!("Couldn't get Android external storage path!");
            panic!();
        }
        Cow::from(Path::new(std::ffi::CStr::from_ptr(path).to_str().unwrap()))
    }
    // [MoleWorld iOS] Put user data (touchHLE_log.txt + the save sandbox) in the
    // app's Documents directory so the iOS Files app can see / import / export it
    // (requires UIFileSharingEnabled + LSSupportsOpeningDocumentsInPlace in the
    // Info.plist; see make-ios-ipa.sh). $HOME is the app sandbox root on iOS.
    #[cfg(target_os = "ios")]
    {
        let docs = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
            .join("Documents");
        let _ = std::fs::create_dir_all(&docs);
        return Cow::from(docs);
    }
    #[cfg(all(not(target_os = "android"), not(target_os = "ios")))]
    {
        // When touchHLE is run from a .app bundle on macOS, the user might not
        // be able to control the current directory, so user data needs to go in
        // a standard location.
        if get_macos_bundled_resources_path().is_some() {
            return Cow::from(PathBuf::from(
                sdl2::filesystem::pref_path("touchhle.org", "touchHLE").unwrap(),
            ));
        }
        Cow::from(Path::new("."))
    }
}

/// Get a URI that can be used to open a file manager or similar for the path
/// that [user_data_base_path] represents.
pub fn url_for_opening_user_data_dir() -> Result<String, String> {
    if std::env::consts::OS == "android" {
        // See DocumentsProvider.kt, app/build.gradle and AndroidManifest.xml
        let brand = crate::branding();
        Ok(format!(
            "content://org.touchhle.android{}{}.provider/root/root",
            if brand.is_empty() { "" } else { "." },
            brand.to_lowercase()
        ))
    } else if std::env::consts::OS == "ios" {
        // [MoleWorld iOS] Open the Files app at the app's Documents folder (where
        // the log + saves now live). `shareddocuments://<path>` is the Files-app
        // scheme; SDL_OpenURL routes it to UIApplication openURL.
        let path = user_data_base_path()
            .canonicalize()
            .map_err(|e| format!("Can't canonicalize user data directory: {e}"))?;
        let path = path
            .to_str()
            .ok_or_else(|| "User data directory path is not UTF-8".to_string())?;
        // [MoleWorld iOS] iOS 把 /var 软链到 /private/var;canonicalize() 把路径解析成
        // /private/var/mobile/...,但 Files app 的 shareddocuments:// 期望不带 /private 前缀的
        // 真实路径,否则 openURL 静默失败(SDL error 为空)。去掉 /private 前缀再拼 URL。
        let path = path.strip_prefix("/private").unwrap_or(path);
        Ok(format!("shareddocuments://{path}"))
    } else {
        let path = user_data_base_path()
            .join(".")
            .canonicalize()
            .map_err(|e| format!("Can't canonicalize path to user data directory: {e}"))?;
        let path = path
            .to_str()
            .ok_or_else(|| "User data directory path is not UTF-8".to_string())?;
        // std::fs::canonicalize() on Windows uses the extended-length path
        // syntax, but Windows Explorer doesn't understand it.
        let path = if std::env::consts::OS == "windows" {
            path.strip_prefix("\\\\?\\").unwrap_or(path)
        } else {
            path
        };
        Ok(format!("file://{path}"))
    }
}

/// Only meaningful on certain OSes: create the user data directory if it
/// doesn't exist, and populate it with templates or README files. (On other
/// platforms these are simply bundled with touchHLE in a ZIP file.)
pub fn prepopulate_user_data_dir() {
    if std::env::consts::OS != "android" && std::env::consts::OS != "macos" {
        return;
    }
    let base_path = user_data_base_path();
    if base_path == Path::new(".") {
        return;
    }

    let apps_dir = base_path.join(APPS_DIR);
    if !apps_dir.is_dir() {
        match std::fs::create_dir(&apps_dir) {
            Ok(()) => {
                log!("Created: {}", apps_dir.display());
            }
            Err(e) => {
                log!("Warning: Couldn't create {}: {}", apps_dir.display(), e);
            }
        }
    }

    fn create_file(path: &Path, content: &str) {
        match std::fs::write(path, content) {
            Ok(()) => {
                log!("Created: {}", path.display());
            }
            Err(e) => {
                log!("Warning: Couldn't create {}: {}", path.display(), e);
            }
        }
    }

    let apps_dir_readme = apps_dir.join("README.txt");
    if !apps_dir_readme.is_file() {
        let content = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/touchHLE_apps/README.txt"
        ));
        create_file(&apps_dir_readme, content);
    }

    let user_options = base_path.join(USER_OPTIONS_FILE);
    if !user_options.is_file() {
        let content = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/touchHLE_options.txt"));
        create_file(&user_options, content);
    }

    let options_help = base_path.join("OPTIONS_HELP.txt");
    if !options_help.is_file() {
        create_file(&options_help, crate::options::OPTIONS_HELP);
    }
}
