/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIImage`.

use crate::frameworks::core_graphics::cg_context::CGContextDrawImage;
use crate::frameworks::core_graphics::cg_image::{
    self, CGImageGetHeight, CGImageGetWidth, CGImageRef, CGImageRelease, CGImageRetain,
};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::{ns_data, ns_string, NSInteger};
use crate::frameworks::uikit::ui_graphics::UIGraphicsGetCurrentContext;
use crate::fs::GuestPath;
use crate::image::Image;
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};
use crate::Environment;
use std::collections::HashMap;

const CACHE_SIZE: usize = 10;

#[derive(Default)]
pub struct State {
    /// Cache of images for `[UIImage imageNamed:]` method.
    /// Images are explicitly retained.
    cached_images: HashMap<String, id>,
}
impl State {
    fn get(env: &Environment) -> &Self {
        &env.framework_state.uikit.ui_image
    }
    fn get_mut(env: &mut Environment) -> &mut Self {
        &mut env.framework_state.uikit.ui_image
    }
}

struct UIImageHostObject {
    cg_image: CGImageRef,
}
impl HostObject for UIImageHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIImage: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIImageHostObject { cg_image: nil });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)imageWithCGImage:(CGImageRef)cg_image {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCGImage:cg_image];
    autorelease(env, new)
}

+ (id)imageNamed:(id)name { // NSString*
    // TODO: figure out whether this is actually correct in all cases
    let bundle: id = msg_class![env; NSBundle mainBundle];
    let path: id = msg![env; bundle pathForResource:name ofType:nil];
    let name_str = ns_string::to_rust_string(env, name).to_string();
    if path == nil {
        log!("Warning: [UIImage imageNamed:{:?}] => nil", name_str);
        return nil;
    }
    // TODO: find a better eviction policy
    if State::get(env).cached_images.len() > CACHE_SIZE {
        let cache = std::mem::take(&mut State::get_mut(env).cached_images);
        log_dbg!("Evicting {} images from UIImage cache.", cache.len());
        for (_, img) in cache {
            release(env, img);
        }
    }
    if !State::get(env).cached_images.contains_key(&name_str) {
        let img = msg![env; this imageWithContentsOfFile:path];
        retain(env, img);
        State::get_mut(env).cached_images.insert(name_str.clone(), img);
    }
    *State::get(env).cached_images.get(&name_str).unwrap()
}

+ (id)imageWithContentsOfFile:(id)path { // NSString*
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithContentsOfFile:path];
    autorelease(env, new)
}

+ (id)imageWithData:(id)data { // NSData*
    // Guard against nil or dangling data. MoleWorld's
    // -[ManagerViewController covertSprite2UIImage:withColor:] feeds the result
    // of a GL-buffer read (getUIImageAsDataFromBuffer:) here; offline/in HLE
    // that can be an invalid (unregistered) object id, and NSData -bytes/-length
    // would then panic in ObjC::borrow (objects.rs unwrap). UIKit returns nil
    // for unusable data, so do the same instead of crashing.
    if data == nil || env.objc.get_host_object(data).is_none() {
        log_dbg!("[UIImage imageWithData:] nil/invalid data, returning nil");
        return nil;
    }
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithData:data];
    autorelease(env, new)
}

- (())dealloc {
    let &UIImageHostObject { cg_image } = env.objc.borrow(this);
    CGImageRelease(env, cg_image);

    env.objc.dealloc_object(this, &mut env.mem)
}

- (id)initWithCGImage:(CGImageRef)cg_image {
    CGImageRetain(env, cg_image);
    env.objc.borrow_mut::<UIImageHostObject>(this).cg_image = cg_image;
    this
}

- (id)initWithContentsOfFile:(id)path { // NSString*
    if path == nil {
        return nil;
    }
    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    let Ok(bytes) = env.fs.read(GuestPath::new(&path)) else {
        log!("Warning: couldn't read image file at {:?}, returning nil", path);
        release(env, this);
        return nil;
    };
    // TODO: Real error handling. For now, most errors are likely to be caused
    //       by a functionality gap in touchHLE, not the app actually trying to
    //       load a broken file, so panicking is most useful.
    let image = Image::from_bytes(&bytes).unwrap();
    let cg_image = cg_image::from_image(env, image);
    env.objc.borrow_mut::<UIImageHostObject>(this).cg_image = cg_image;
    this
}

- (id)initWithData:(id)data { // NSData*
    let slice = ns_data::to_rust_slice(env, data);
    // TODO: refactor common parts
    let image = Image::from_bytes(slice).unwrap();
    let cg_image = cg_image::from_image(env, image);
    env.objc.borrow_mut::<UIImageHostObject>(this).cg_image = cg_image;
    this
}

- (id)stretchableImageWithLeftCapWidth:(NSInteger)_leftCapWidth
                          topCapHeight:(NSInteger)_topCapHeight {
    log!("TODO: properly support stretchableImageWithLeftCapWidth:topCapHeight:");
    retain(env, this)
}

// TODO: more init methods
// TODO: more accessors

- (CGImageRef)CGImage {
    env.objc.borrow::<UIImageHostObject>(this).cg_image
}

// TODO: should have UIImageOrientation type
- (NSInteger)imageOrientation {
    // FIXME: load image orientation info from file?
    0 // UIImageOrientationUp
}

- (CGSize)size {
    let image = env.objc.borrow::<UIImageHostObject>(this).cg_image;
    let (width, height) = cg_image::borrow_image(&env.objc, image).dimensions();
    CGSize {
        width: width as _,
        height: height as _,
    }
}

- (CGFloat)scale {
    // TODO: support other scales, such as @2x
    1.0
}

- (())drawInRect:(CGRect)rect {
    let context = UIGraphicsGetCurrentContext(env);
    let image = env.objc.borrow::<UIImageHostObject>(this).cg_image;
    CGContextDrawImage(env, context, rect, image);
}

- (())drawAtPoint:(CGPoint)point {
    let context = UIGraphicsGetCurrentContext(env);
    if context == nil {
        log!("Warning: [(UIImage*){:?} drawAtPoint:{:?}] is called with nil context, ignoring.", this, point);
        return;
    }
    let image = env.objc.borrow::<UIImageHostObject>(this).cg_image;
    let rect = CGRect {
        origin: point,
        size: CGSize {
            width: CGImageGetWidth(env, image) as CGFloat,
            height: CGImageGetHeight(env, image) as CGFloat,
        }
    };
    CGContextDrawImage(env, context, rect, image);
}

@end

// Undocumented class used in NIBs
// TODO: It's not clear _why_ placeholder is needed?
@implementation UIImageNibPlaceholder: UIImage

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    release(env, this);

    // TODO: decode other attributes
    let key_ns_string = get_static_str(env, "UIResourceName");
    let resource_name: id = msg![env; coder decodeObjectForKey:key_ns_string];

    let res = msg_class![env; UIImage imageNamed:resource_name];
    // TODO: It is not clear if we need to additionally retain here?
    retain(env, res)
}

@end

};

/// CRC-32 (IEEE, as PNG requires). Small table-less implementation.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Encode RGBA8 pixels (top-to-bottom, width*height*4 bytes) into a PNG file.
fn encode_png_rgba(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), no interlace.
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    // IDAT: zlib-compressed scanlines, each prefixed with a filter byte (0=None).
    let row_bytes = (width as usize) * 4;
    let mut raw = Vec::with_capacity((row_bytes + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        let start = y * row_bytes;
        raw.extend_from_slice(&pixels[start..start + row_bytes]);
    }
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&raw).unwrap();
    let compressed = enc.finish().unwrap();
    chunk(&mut out, b"IDAT", &compressed);

    chunk(&mut out, b"IEND", &[]);
    out
}

/// `NSData *UIImagePNGRepresentation(UIImage *image)`
///
/// Returns PNG-encoded data for the image, or nil. MoleWorld's screenshot
/// feature (+[Screenshot takeAsUIImage] -> CameraLayer save/share) calls this;
/// it was unimplemented (no-op returning 0), so saving a screenshot crashed.
fn UIImagePNGRepresentation(env: &mut Environment, image: id) -> id {
    if image == nil {
        return nil;
    }
    let cg_image: CGImageRef = msg![env; image CGImage];
    if cg_image == nil {
        return nil;
    }
    let (width, height, pixels) = {
        let img = cg_image::borrow_image(&env.objc, cg_image);
        let (w, h) = img.dimensions();
        (w, h, img.pixels().to_vec())
    };
    if pixels.len() < (width as usize) * (height as usize) * 4 {
        log!("UIImagePNGRepresentation: pixel buffer too small, returning nil");
        return nil;
    }
    let png = encode_png_rgba(&pixels, width, height);

    let len: crate::mem::GuestUSize = png.len().try_into().unwrap();
    let buf = env.mem.alloc(len);
    env.mem.bytes_at_mut(buf.cast(), len).copy_from_slice(&png);
    msg_class![env; NSData dataWithBytesNoCopy:buf length:len]
}

/// [扫描修 2026-09-15] F12-4:本地时间戳 `YYYYMMDD_HHMMSS`,用作照片文件名。
/// 用宿主真实时间(不叠加开发工具的时间旅行偏移,文件名对玩家才有意义),时区与游戏内
/// 一致走 `local_utc_offset_at`(默认北京时间,MOLE_TZ=host 跟随宿主)。
fn photo_timestamp_string() -> String {
    let now = crate::libc::time::host_now_unix_secs();
    let local = now + crate::libc::time::local_utc_offset_at(now) as i64;
    let tm = crate::libc::time::timestamp_to_calendar_date_i64(local);
    // tm 是 packed 结构体,格式化宏会取字段引用,先拷到局部变量。
    let (year, mon, mday, hour, min, sec) =
        (tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec);
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        year + 1900,
        mon + 1,
        mday,
        hour,
        min,
        sec
    )
}

/// [扫描修 2026-09-15] F12-4:把 UIImage 编成 PNG,原子写到
/// `user_data_base_path()/screenshots/摩尔庄园_YYYYMMDD_HHMMSS.png`。成功返回宿主完整路径。
///
/// - 不写 guest 沙盒 Documents(免得被存档扫描误伤);macOS .app 下 user_data_base_path
///   是可写的 pref_path,不会写进只读的应用包。
/// - 相册照片没有透明通道(iOS 相册存为 JPEG),这里强制 alpha=255;像素本身是预乘的,
///   等价于"合成在黑底上",与屏幕上看到的一致(glReadPixels 读回的 alpha 通道不可信)。
/// - 写临时文件再 rename,进程中途被杀也不会留下残缺 PNG;同一秒多张时追加 _2、_3。
fn save_image_to_host_album(env: &mut Environment, image: id) -> Result<std::path::PathBuf, String> {
    if image == nil {
        return Err("image 为 nil".to_string());
    }
    let cg_image: CGImageRef = msg![env; image CGImage];
    if cg_image == nil {
        return Err("UIImage 没有 CGImage".to_string());
    }
    let (width, height, mut pixels) = {
        let img = cg_image::borrow_image(&env.objc, cg_image);
        let (w, h) = img.dimensions();
        (w, h, img.pixels().to_vec())
    };
    if width == 0 || height == 0 {
        return Err(format!("图像尺寸为 {}×{}", width, height));
    }
    if pixels.len() < (width as usize) * (height as usize) * 4 {
        return Err("像素缓冲区过小".to_string());
    }
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 0xFF;
    }
    let png = encode_png_rgba(&pixels, width, height);

    let dir = crate::paths::user_data_base_path().join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录 {} 失败:{}", dir.display(), e))?;
    let stamp = photo_timestamp_string();
    let mut path = dir.join(format!("摩尔庄园_{}.png", stamp));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("摩尔庄园_{}_{}.png", stamp, n));
        n += 1;
    }
    let tmp = dir.join(format!(".摩尔庄园_{}_{}.png.tmp", stamp, n));
    std::fs::write(&tmp, &png).map_err(|e| format!("写临时文件 {} 失败:{}", tmp.display(), e))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("重命名到 {} 失败:{}", path.display(), e));
    }
    Ok(path)
}

/// `void UIImageWriteToSavedPhotosAlbum(UIImage *image, id completionTarget,
///   SEL completionSelector, void *contextInfo)`
///
/// [扫描修 2026-09-15] F12-4 / F9-8(保存照片那一半):原先是"离线无相册,当作成功的空操作",
/// 玩家点相机/分享/魔法密码截图后听到快门、看到提示,硬盘上却什么都没有。现在真正落盘
/// (见 save_image_to_host_album),echo 打印完整路径。
/// 完成回调按原签名 image:didFinishSavingWithError:contextInfo: 三参调用(原先用
/// performSelector:withObject:withObject: 漏传 contextInfo,第三参是寄存器残值);
/// 写盘失败时传非 nil 的 NSError(ALAssetsLibraryErrorDomain / -3301 写入失败),
/// 让原版走失败分支。
fn UIImageWriteToSavedPhotosAlbum(
    env: &mut Environment,
    image: id,
    completion_target: id,
    completion_selector: crate::objc::SEL,
    context_info: crate::mem::MutVoidPtr,
) {
    let error: id = match save_image_to_host_album(env, image) {
        Ok(path) => {
            echo!("UIImageWriteToSavedPhotosAlbum: 照片已保存到 {}", path.display());
            nil
        }
        Err(reason) => {
            log!("UIImageWriteToSavedPhotosAlbum: 保存照片失败:{}", reason);
            let domain: id = ns_string::get_static_str(env, "ALAssetsLibraryErrorDomain");
            msg_class![env; NSError errorWithDomain:domain
                                               code:(-3301 as NSInteger)
                                           userInfo:nil]
        }
    };
    if completion_target != nil && !completion_selector.is_null() {
        let responds: bool = msg![env; completion_target respondsToSelector:completion_selector];
        if responds {
            () = crate::objc::msg_send(
                env,
                (completion_target, completion_selector, image, error, context_info),
            );
        }
    }
}

pub const FUNCTIONS: crate::dyld::FunctionExports = &[
    crate::dyld::export_c_func!(UIImagePNGRepresentation(_)),
    crate::dyld::export_c_func!(UIImageWriteToSavedPhotosAlbum(_, _, _, _)),
];
