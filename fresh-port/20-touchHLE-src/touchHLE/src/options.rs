/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Parsing and management of user-configurable options, e.g. for input methods.

use crate::gles::GLESImplementation;
use crate::window::{DeviceFamily, DeviceOrientation};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::num::NonZeroU32;
use std::path::PathBuf;

pub const OPTIONS_HELP: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/OPTIONS_HELP.txt"));

/// Game controller button for `--button-to-touch=` option.
#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
pub enum Button {
    DPadLeft,
    DPadUp,
    DPadRight,
    DPadDown,
    Start,
    A,
    B,
    X,
    Y,
    LeftShoulder,
}

/// Struct containing all user-configurable options.
#[derive(Clone)]
pub struct Options {
    pub fullscreen: bool,
    pub device_family: Option<DeviceFamily>,
    pub initial_orientation: DeviceOrientation,
    pub scale_hack: NonZeroU32,
    /// [MoleWorld] 窗口模式锁定宽高比(等比 letterbox 黑边);默认 false=自由拉伸铺满。
    pub lock_aspect: bool,
    /// [MoleWorld 智能分辨率] `--logical-size=WxH`:显式指定 guest 逻辑屏(点)。传 1366x768 或
    /// 768x1366 皆可,内部归一成 portrait=(短,长)。None=不覆盖。见 window::apply_cli_resolution。
    pub logical_size: Option<(u32, u32)>,
    /// [MoleWorld 智能分辨率] `--fill-screen`:按目标屏宽高比自动算 guest 逻辑屏(FixedHeight Hor+),
    /// 物理满屏不黑边、不拉伸。默认 false=零回归。
    pub fill_screen: bool,
    /// [MoleWorld 智能分辨率] `--max-aspect=F`:自动适配时 guest landscape 宽高比上限(默认 2.4)。
    /// 夹在 [4:3, 4.0]。None=用默认。仅超宽屏会被钳(留极小 pillarbox 防变形)。
    pub max_aspect: Option<f32>,
    /// [MoleWorld 智能分辨率]「4:3 完美模式」环境补边:`--ambient-fill`。guest 保持原生 4:3(所有
    /// UI 场景像素级完美、零错位),在宽屏上等比居中,letterbox 空白处不留黑边,而用【画面本身
    /// 横向拉伸+压暗】填充(视频播放器 ambient 风)。默认 false=原生 letterbox 黑边。
    pub ambient_fill: bool,
    pub deadzone: f32,
    pub analog_stick_tilt_controls: bool,
    pub x_tilt_range: f32,
    pub y_tilt_range: f32,
    pub x_tilt_offset: f32,
    pub y_tilt_offset: f32,
    pub button_to_touch: HashMap<Button, (f32, f32)>,
    pub dpad_to_touch: Option<(f32, f32, f32, f32)>,
    pub stick_to_touch: Option<(f32, f32, f32, f32)>,
    pub stabilize_virtual_cursor: Option<(f32, f32)>,
    pub gles1_implementation: Option<GLESImplementation>,
    pub direct_memory_access: bool,
    pub gdb_listen_addrs: Option<Vec<SocketAddr>>,
    pub preferred_languages: Option<Vec<String>>,
    pub headless: bool,
    pub print_fps: bool,
    pub fps_limit: Option<f64>,
    pub force_composition: bool,
    pub network_access: bool,
    pub popup_errors: bool,
    pub dumping_options: DumpingOptions,
    pub dumping_file: PathBuf,
    pub ignore_gl_errors: bool,
    pub zero_stack_after_guest_to_host_call: Option<u32>,
    /// [2026-09-16] A1-03 `--log-modules=a,b`:运行时打开这些模块(按前缀匹配)的 log_dbg!。
    /// 能写进 touchHLE_options.txt,安卓 / iOS 设不了环境变量 TOUCHHLE_LOG_MODULES 时靠它。默认空 = 行为不变。
    /// 由 lib.rs 在全部选项应用完之后交给 log::init_dbg_modules。
    pub log_modules: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            fullscreen: false,
            device_family: None,
            initial_orientation: DeviceOrientation::Portrait,
            scale_hack: NonZeroU32::new(1).unwrap(),
            lock_aspect: false,
            logical_size: None,
            fill_screen: false,
            max_aspect: None,
            ambient_fill: false,
            analog_stick_tilt_controls: true,
            deadzone: 0.1,
            x_tilt_range: 60.0,
            y_tilt_range: 60.0,
            x_tilt_offset: 0.0,
            y_tilt_offset: 0.0,
            button_to_touch: HashMap::new(),
            dpad_to_touch: None,
            stick_to_touch: None,
            stabilize_virtual_cursor: None,
            gles1_implementation: None,
            direct_memory_access: true,
            gdb_listen_addrs: None,
            preferred_languages: None,
            headless: false,
            print_fps: false,
            fps_limit: Some(60.0), // Original iPhone is 60Hz and uses v-sync,
            force_composition: false,
            network_access: false,
            popup_errors: true,
            dumping_options: Default::default(),
            dumping_file: crate::paths::user_data_base_path().join("DUMP.txt"),
            ignore_gl_errors: false,
            zero_stack_after_guest_to_host_call: None,
            log_modules: Vec::new(),
        }
    }
}

impl Options {
    /// Parse the command-line argument syntax for an option. Returns `Ok(true)`
    /// if the option was valid and has been applied, or `Ok(false)` if the
    /// option was not recognized.
    pub fn parse_argument(&mut self, arg: &str) -> Result<bool, String> {
        fn parse_degrees(arg: &str, name: &str) -> Result<f32, String> {
            let arg: f32 = arg
                .parse()
                .map_err(|_| format!("Value for {name} is invalid"))?;
            if !arg.is_finite() || !(-360.0..=360.0).contains(&arg) {
                return Err(format!("Value for {name} is out of range"));
            }
            Ok(arg)
        }

        if arg == "--fullscreen" {
            self.fullscreen = true;
        } else if arg == "--upside-down" {
            self.initial_orientation = DeviceOrientation::PortraitUpsideDown;
        } else if arg == "--landscape-left" {
            self.initial_orientation = DeviceOrientation::LandscapeLeft;
        } else if arg == "--landscape-right" {
            self.initial_orientation = DeviceOrientation::LandscapeRight;
        } else if let Some(value) = arg.strip_prefix("--device-family=") {
            let parsed =
                DeviceFamily::try_from(value).map_err(|_| "Invalid device family".to_string())?;
            self.device_family = Some(parsed);
        } else if let Some(value) = arg.strip_prefix("--scale-hack=") {
            self.scale_hack = value
                .parse()
                .map_err(|_| "Invalid scale hack factor".to_string())?;
        } else if arg == "--lock-aspect" {
            // [MoleWorld] 窗口锁定宽高比:窗口模式改为等比 letterbox(四周黑边),不自由拉伸。
            self.lock_aspect = true;
        } else if arg == "--fill-screen" {
            // [MoleWorld 智能分辨率] 按目标屏宽高比自动铺满(FixedHeight Hor+),不黑边不拉伸。
            self.fill_screen = true;
        } else if arg == "--ambient-fill" {
            // [MoleWorld 智能分辨率]「4:3 完美模式」:等比居中 + 环境延伸补边(代替黑边)。
            self.ambient_fill = true;
        } else if let Some(value) = arg.strip_prefix("--logical-size=") {
            // [MoleWorld 智能分辨率] 显式 guest 逻辑屏。WxH,传 1366x768 或 768x1366 皆可。
            let (w, h) = value
                .split_once('x')
                .ok_or_else(|| "--logical-size= expects WxH (e.g. 1366x768)".to_string())?;
            let w: u32 = w
                .trim()
                .parse()
                .map_err(|_| "Invalid width for --logical-size=".to_string())?;
            let h: u32 = h
                .trim()
                .parse()
                .map_err(|_| "Invalid height for --logical-size=".to_string())?;
            if w == 0 || h == 0 {
                return Err("--logical-size= dimensions must be greater than 0".to_string());
            }
            self.logical_size = Some((w, h));
        } else if let Some(value) = arg.strip_prefix("--max-aspect=") {
            // [MoleWorld 智能分辨率] 自动适配时 guest landscape 宽高比上限(夹在 [4:3, 4.0])。
            let a: f32 = value
                .trim()
                .parse()
                .ok()
                .filter(|v: &f32| v.is_finite() && *v > 0.0)
                .ok_or_else(|| "Invalid value for --max-aspect=".to_string())?;
            self.max_aspect = Some(a);
        } else if arg == "--disable-analog-stick-tilt-controls" {
            self.analog_stick_tilt_controls = false;
        } else if let Some(value) = arg.strip_prefix("--deadzone=") {
            self.deadzone = parse_degrees(value, "deadzone")?;
        } else if let Some(value) = arg.strip_prefix("--x-tilt-range=") {
            self.x_tilt_range = parse_degrees(value, "X tilt range")?;
        } else if let Some(value) = arg.strip_prefix("--y-tilt-range=") {
            self.y_tilt_range = parse_degrees(value, "Y tilt range")?;
        } else if let Some(value) = arg.strip_prefix("--x-tilt-offset=") {
            self.x_tilt_offset = parse_degrees(value, "X tilt offset")?;
        } else if let Some(value) = arg.strip_prefix("--y-tilt-offset=") {
            self.y_tilt_offset = parse_degrees(value, "Y tilt offset")?;
        } else if let Some(values) = arg.strip_prefix("--button-to-touch=") {
            let (button, coords) = values
                .split_once(',')
                .ok_or_else(|| "--button-to-touch= requires three values".to_string())?;
            let (x, y) = coords
                .split_once(',')
                .ok_or_else(|| "--button-to-touch= requires three values".to_string())?;
            let button = match button {
                "DPadLeft" => Ok(Button::DPadLeft),
                "DPadUp" => Ok(Button::DPadUp),
                "DPadRight" => Ok(Button::DPadRight),
                "DPadDown" => Ok(Button::DPadDown),
                "Start" => Ok(Button::Start),
                "A" => Ok(Button::A),
                "B" => Ok(Button::B),
                "X" => Ok(Button::X),
                "Y" => Ok(Button::Y),
                "LeftShoulder" => Ok(Button::LeftShoulder),
                _ => Err("Invalid button for --button-to-touch=".to_string()),
            }?;
            let x: f32 = x
                .parse()
                .map_err(|_| "Invalid X co-ordinate for --button-to-touch=".to_string())?;
            let y: f32 = y
                .parse()
                .map_err(|_| "Invalid Y co-ordinate for --button-to-touch=".to_string())?;
            self.button_to_touch.insert(button, (x, y));
        } else if let Some(values) = arg.strip_prefix("--stick-to-touch=") {
            let nums: [f32; 4] = values
                .split(',')
                .map(|s| s.parse::<f32>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "invalid --stick-to-touch".to_string())?
                .try_into()
                .map_err(|_| "--stick-to-touch= requires four values".to_string())?;

            self.stick_to_touch = Some((nums[0], nums[1], nums[2], nums[3]));
        } else if let Some(values) = arg.strip_prefix("--dpad-to-touch=") {
            let nums: [f32; 4] = values
                .split(',')
                .map(|s| s.parse::<f32>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "invalid --dpad-to-touch".to_string())?
                .try_into()
                .map_err(|_| "--dpad-to-touch= requires four values".to_string())?;

            self.dpad_to_touch = Some((nums[0], nums[1], nums[2], nums[3]));
        } else if let Some(value) = arg.strip_prefix("--stabilize-virtual-cursor=") {
            let (smoothing_strength, sticky_radius) = value
                .split_once(',')
                .ok_or_else(|| "--stabilize-virtual-cursor= requires two values".to_string())?;
            let smoothing_strength: f32 = smoothing_strength
                .parse()
                .ok()
                .and_then(|s| if s < 0.0 { None } else { Some(s) })
                .ok_or_else(|| {
                    "Invalid smoothing strength for --stabilize-virtual-cursor=".to_string()
                })?;
            let sticky_radius: f32 = sticky_radius
                .parse()
                .ok()
                .and_then(|s| if s < 0.0 { None } else { Some(s) })
                .ok_or_else(|| {
                    "Invalid sticky radius for --stabilize-virtual-cursor=".to_string()
                })?;
            self.stabilize_virtual_cursor = Some((smoothing_strength, sticky_radius));
        } else if let Some(value) = arg.strip_prefix("--gles1=") {
            self.gles1_implementation = Some(
                GLESImplementation::from_short_name(value)
                    .map_err(|_| "Unrecognized --gles1= value".to_string())?,
            );
        } else if arg == "--disable-direct-memory-access" {
            self.direct_memory_access = false;
        } else if let Some(address) = arg.strip_prefix("--gdb=") {
            let addrs = address
                .to_socket_addrs()
                .map_err(|e| format!("Could not resolve GDB server listen address: {e}"))?
                .collect();
            self.gdb_listen_addrs = Some(addrs);
        } else if let Some(value) = arg.strip_prefix("--preferred-languages=") {
            self.preferred_languages = Some(value.split(',').map(ToOwned::to_owned).collect());
        } else if arg == "--headless" {
            self.headless = true;
            // Can't show the dialog box when headless!
            self.popup_errors = false;
        } else if arg == "--print-fps" {
            self.print_fps = true;
        } else if let Some(value) = arg.strip_prefix("--fps-limit=") {
            if value == "off" {
                self.fps_limit = None;
            } else {
                let limit: f64 = value
                    .parse()
                    .ok()
                    .and_then(|v| if v <= 0.0 { None } else { Some(v) })
                    .ok_or_else(|| "Invalid value for --fps-limit=".to_string())?;
                self.fps_limit = Some(limit);
            }
        } else if arg == "--force-composition" {
            self.force_composition = true;
        } else if arg == "--allow-network-access" {
            self.network_access = true;
        } else if arg == "--no-error-popup" {
            self.popup_errors = false;
        } else if let Some(values) = arg.strip_prefix("--dump=") {
            self.dumping_options = parse_dump_options(values)?;
        } else if let Some(path) = arg.strip_prefix("--dump-file=") {
            self.dumping_file = crate::paths::user_data_base_path().join(path);
        } else if arg == "--ignore-gl-errors" {
            self.ignore_gl_errors = true;
        } else if let Some(value) = arg.strip_prefix("--zero-stack-after-guest-to-host-call=") {
            self.zero_stack_after_guest_to_host_call = Some(value.parse().map_err(|_| {
                "Invalid value for --zero-stack-after-guest-to-host-call=".to_string()
            })?);
        } else if let Some(value) = arg.strip_prefix("--log-modules=") {
            // [2026-09-16] A1-03 逗号分隔的模块路径前缀。后出现的整体覆盖先出现的(同 --preferred-languages=):
            // main() 在应用选项文件之后会把命令行重放一遍,追加式写法会让同一批模块重复叠加。
            self.log_modules = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        } else {
            return Ok(false);
        };
        Ok(true)
    }
}

/// Try to get app-specific options from a file.
///
/// Returns [Ok] if there is no error when reading the file, otherwise [Err].
/// The [Ok] value is a [Some] with the options if they could be found, or
/// [None] if no options were found for this app.
pub fn get_options_from_file<F: Read>(file: F, app_id: &str) -> Result<Option<String>, String> {
    let file = BufReader::new(file);
    for (line_no, line) in BufRead::lines(file).enumerate() {
        // Line numbering usually starts from 1
        let line_no = line_no + 1;

        let line = line.map_err(|e| format!("Error while reading line {line_no}: {e}"))?;

        // # for single-line comments
        let line = if let Some((rest, _)) = line.split_once('#') {
            rest
        } else {
            &line
        };

        // Empty/all-comment lines ignored
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let (line_app_id, line_options) = line.split_once(':').ok_or_else(|| format!("Line {line_no} is not a comment and is missing a colon (:) to separate the app ID from the options"))?;
        let line_app_id = line_app_id.trim();

        if line_app_id != app_id {
            continue;
        }

        let line_options = line_options.trim();
        if line_options.is_empty() {
            return Ok(None);
        } else {
            return Ok(Some(line_options.to_string()));
        }
    }
    Ok(None)
}

#[derive(Default, Clone)]
pub struct DumpingOptions {
    pub linking_info: bool,
    pub symbols: bool,
}

impl DumpingOptions {
    /// Check if any of the dumping options are active.
    pub fn any(&self) -> bool {
        self.linking_info || self.symbols
    }
}

fn parse_dump_options(options: &str) -> Result<DumpingOptions, String> {
    let mut dumping_options = DumpingOptions::default();
    for opt in options.split(",") {
        if opt == "linking-info" {
            // Dumps linked symbols, classes and selectors for the given app
            dumping_options.linking_info = true;
        } else if opt == "symbols" {
            // Dumps touchHLE provided symbols and exits
            dumping_options.symbols = true;
        } else {
            return Err(format!("Unrecognized option {opt} for --dump=..."));
        }
    }
    Ok(dumping_options)
}
