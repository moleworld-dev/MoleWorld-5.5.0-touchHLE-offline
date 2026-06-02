/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIWebView`.
//!
//! touchHLE has no real HTML rendering engine. For the common case of a local
//! help/about document (e.g. MoleWorld's `mole_help.html`, a Word-exported
//! page loaded via `loadRequest:` with a `file://` URL), we approximate it:
//! strip the HTML to plain text and show it in a scrollable multi-line label so
//! the user at least sees the original text instead of a blank page.

use crate::frameworks::core_graphics::CGRect;
use crate::frameworks::foundation::ns_string::{from_rust_string, to_rust_string};
use crate::fs::GuestPath;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil, objc_classes, release,
    ClassExports, NSZonePtr,
};
use crate::Environment;

struct UIWebViewHostObject {
    superclass: super::UIViewHostObject,
    /// The label subview that displays the extracted text, if any.
    text_label: id,
    /// Weak reference to the delegate (not retained, like real UIKit).
    delegate: id,
}
impl_HostObject_with_superclass!(UIWebViewHostObject);
impl Default for UIWebViewHostObject {
    fn default() -> Self {
        UIWebViewHostObject {
            superclass: Default::default(),
            text_label: nil,
            delegate: nil,
        }
    }
}

/// Very small HTML -> plain text reducer: drops tags, collapses whitespace,
/// turns block-level tags into line breaks, decodes a few common entities.
fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut chars = html.chars().peekable();
    // Track whether we are inside <style>/<script>/<head> we want to skip.
    let lower = html.to_ascii_lowercase();
    // Quick skip of <head>...</head> and <style>...</style> blocks by working on
    // a filtered copy: simplest is to remove those spans first.
    fn strip_span(s: &str, open: &str, close: &str) -> String {
        let mut res = String::new();
        let mut rest = s;
        let lo = s.to_ascii_lowercase();
        let mut base = 0usize;
        loop {
            let lo_rest = &lo[base..];
            if let Some(i) = lo_rest.find(open) {
                let start = base + i;
                res.push_str(&rest[..start - (base - (s.len() - rest.len()))]);
                // find close after start
                if let Some(j) = lo[start..].find(close) {
                    let end = start + j + close.len();
                    rest = &s[end..];
                    base = end;
                } else {
                    rest = "";
                    break;
                }
            } else {
                res.push_str(rest);
                break;
            }
        }
        res
    }
    let _ = (&mut chars, &lower);
    let no_head = strip_span(html, "<head", "</head>");
    let no_style = strip_span(&no_head, "<style", "</style>");
    let cleaned = strip_span(&no_style, "<script", "</script>");

    let mut in_tag = false;
    let mut last_was_space = true;
    let lc = cleaned.to_ascii_lowercase();
    let bytes_lc: Vec<char> = lc.chars().collect();
    let chs: Vec<char> = cleaned.chars().collect();
    let mut i = 0;
    while i < chs.len() {
        let c = chs[i];
        if c == '<' {
            // Block-level tags -> newline
            let tagslice: String = bytes_lc[i..(i + 8).min(bytes_lc.len())].iter().collect();
            if tagslice.starts_with("<p")
                || tagslice.starts_with("<br")
                || tagslice.starts_with("<div")
                || tagslice.starts_with("<tr")
                || tagslice.starts_with("<li")
                || tagslice.starts_with("<h1")
                || tagslice.starts_with("<h2")
                || tagslice.starts_with("<h3")
                || tagslice.starts_with("</p")
            {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
                last_was_space = true;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if c == '>' {
            in_tag = false;
            i += 1;
            continue;
        }
        if in_tag {
            i += 1;
            continue;
        }
        if c == '&' {
            // decode a few entities
            let ent: String = chs[i..(i + 8).min(chs.len())].iter().collect();
            if ent.starts_with("&nbsp;") {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
                i += 6;
                continue;
            } else if ent.starts_with("&amp;") {
                out.push('&');
                i += 5;
                last_was_space = false;
                continue;
            } else if ent.starts_with("&lt;") {
                out.push('<');
                i += 4;
                last_was_space = false;
                continue;
            } else if ent.starts_with("&gt;") {
                out.push('>');
                i += 4;
                last_was_space = false;
                continue;
            }
        }
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            i += 1;
            continue;
        }
        out.push(c);
        last_was_space = false;
        i += 1;
    }
    // Collapse runs of blank lines.
    let mut result = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let t = line.trim();
        if t.is_empty() {
            blank += 1;
            if blank <= 1 {
                result.push('\n');
            }
        } else {
            blank = 0;
            result.push_str(t);
            result.push('\n');
        }
    }
    result.trim().to_string()
}

/// Read the file a `file://` request points at and decode it to a Rust string.
/// Handles the gb2312/GBK charset MoleWorld's help docs use, falling back to
/// UTF-8 then lossy.
fn read_document(env: &mut Environment, path: &str) -> Option<String> {
    let bytes = env.fs.read(GuestPath::new(path)).ok()?;
    // Sniff charset from a <meta charset=...> hint, default to GBK for these
    // Word-exported docs (charset=gb2312).
    let head: String = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let use_gbk = head.contains("gb2312") || head.contains("gbk");
    let text = if use_gbk {
        let (cow, _, _) = encoding_rs::GBK.decode(&bytes);
        cow.into_owned()
    } else {
        match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(_) => {
                let (cow, _, _) = encoding_rs::GBK.decode(&bytes);
                cow.into_owned()
            }
        }
    };
    Some(text)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIWebView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIWebViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    this
}

- (())setScalesPageToFit:(bool)_scales {
    // TODO
}
- (())setDelegate:(id)delegate {
    // Stored weakly (not retained), like real UIKit. loadRequest: messages it
    // with webViewDidFinishLoad: once the document is shown.
    env.objc.borrow_mut::<UIWebViewHostObject>(this).delegate = delegate;
}
- (())loadHTMLString:(id)html_string baseURL:(id)_base_url {
    let html = to_rust_string(env, html_string).to_string();
    let white: id = msg_class![env; UIColor whiteColor];
    show_text(env, this, &html_to_text(&html), white);
}
- (())loadRequest:(id)request { // NSURLRequest*
    if request == nil {
        return;
    }
    let url: id = msg![env; request URL];
    if url == nil {
        return;
    }
    let path_ns: id = msg![env; url path];
    if path_ns == nil {
        return;
    }
    let path = to_rust_string(env, path_ns).to_string();
    if path.contains("mole_help") {
        // [MoleWorld] 原版帮助页(mole_help.html:FAQ + 官方技术支持 + 版权)替换为
        // 本移植项目的「关于」页:作者 / 官方交流群 / 当前运行硬件(CPU/GPU/分辨率/
        // 系统时间时区)/ 贡献者 / 版本 / 版权。背景沿用原版暖黄色。
        let text = crate::mole_sysinfo::mole_help_text(env);
        let (r, g, b) = crate::mole_sysinfo::HELP_BG_RGB;
        let bg: id = msg_class![env; UIColor colorWithRed:r green:g blue:b alpha:1.0f32];
        log!("UIWebView: [MoleWorld] 渲染自定义「关于」页(替代 {})", path);
        show_text(env, this, &text, bg);
    } else {
        match read_document(env, &path) {
            Some(html) => {
                let text = html_to_text(&html);
                log!("UIWebView: rendering {} ({} chars of extracted text)", path, text.len());
                let white: id = msg_class![env; UIColor whiteColor];
                show_text(env, this, &text, white);
            }
            None => {
                log!("UIWebView: couldn't read {}", path);
            }
        }
    }
    // Notify the delegate that loading "finished" so the host layer proceeds.
    let delegate: id = msg![env; this delegate];
    if delegate != nil {
        let sel = env.objc.lookup_selector("webViewDidFinishLoad:");
        if let Some(sel) = sel {
            if msg![env; delegate respondsToSelector:sel] {
                () = msg![env; delegate webViewDidFinishLoad:this];
            }
        }
    }
}
- (id)delegate {
    env.objc.borrow::<UIWebViewHostObject>(this).delegate
}

- (())dealloc {
    let &UIWebViewHostObject { superclass: _, text_label, delegate: _ } = env.objc.borrow(this);
    if text_label != nil {
        release(env, text_label);
    }
    msg_super![env; this dealloc]
}

@end

};

/// Build (or refresh) a multi-line label child showing `text`, filling this
/// web view's bounds on a white background.
fn show_text(env: &mut Environment, web_view: id, text: &str, bg_color: id) {
    // Remove any previous label.
    let old: id = env.objc.borrow::<UIWebViewHostObject>(web_view).text_label;
    if old != nil {
        () = msg![env; old removeFromSuperview];
        release(env, old);
        env.objc.borrow_mut::<UIWebViewHostObject>(web_view).text_label = nil;
    }

    let bounds: CGRect = msg![env; web_view bounds];

    // 背景:抽取文档用白底;MoleWorld「关于」页用原版暖黄(由调用方传入)。
    () = msg![env; web_view setBackgroundColor:bg_color];

    let label: id = msg_class![env; UILabel alloc];
    let label: id = msg![env; label initWithFrame:bounds];
    let black: id = msg_class![env; UIColor blackColor];
    () = msg![env; label setTextColor:black];
    let clear: id = msg_class![env; UIColor clearColor];
    () = msg![env; label setBackgroundColor:clear];
    // 0 = unlimited lines (wrap as needed).
    () = msg![env; label setNumberOfLines:0i32];

    let text_ns = from_rust_string(env, text.to_string());
    () = msg![env; label setText:text_ns];
    release(env, text_ns);

    () = msg![env; web_view addSubview:label];
    env.objc.borrow_mut::<UIWebViewHostObject>(web_view).text_label = label;
}
