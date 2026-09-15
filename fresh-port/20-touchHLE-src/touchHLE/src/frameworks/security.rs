/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Security framework:钥匙串(SecItem*)。
//!
//! [扫描修 2026-09-15] 由「假成功桩」改为「按文件持久化的宿主钥匙串」。
//!
//! 根因:
//! - 游戏里 5 套钥匙串封装全部走 SecItem*:淘米通行证的 TMA_SSKeychain(历史账号、记住密码、
//!   设备绑定标记、设备 ID,服务名 "Taomee.sskeychain")、SFHFKeychainUtils(内购记录)、
//!   PBSFHFKeychainUtils(PunchBox uuid)、immobKeyChain、MAKeychainItemWrapper(Miidi)。
//! - 旧桩里 SecItemCopyMatching 恒回 errSecItemNotFound,Add/Update/Delete 恒回成功却什么都不存,
//!   账号系统每次启动都「失忆」。旧桩只导出 10 个 kSec* 常量,值还是 "kSecClass" 这类假串;
//!   kSecMatchLimitAll、kSecAttrLabel、kSecAttrAccessible* 等在 dyld 里是未处理重定位(读到 0),
//!   +[TMA_SSKeychain accountsForService:error:]@0x4c3ca4 拿 0 当 kSecMatchLimitAll,查询就没了上限。
//! - +[TMA_SSKeychain allAccounts]@0x4c35cc 用 objectForKey:@"acct" 过滤返回的属性字典,
//!   所以常量必须用苹果原值("acct"/"svce"/"genp"/"v_Data"/"r_Attributes"/"m_LimitAll" …)。
//!
//! 实现:
//! - 条目 = (kSecClass, 属性表, kSecValueData)。唯一键:genp 按 (acct, svce);inet 按
//!   acct/sdmn/srvr/ptcl/atyp/port/path;其它类不查重(游戏没用到)。SecItemAdd 撞键回 errSecDuplicateItem。
//! - 匹配:kSecClass 必须相等(缺失回 errSecParam,与真机一致);查询里其余属性必须在条目里存在且相等。
//!   agrp/pdmn/sync/cdat/mdat 等访问控制、同步、时间戳属性不参与匹配:单应用沙盒里没有意义,而
//!   MAKeychainItemWrapper 会把 CopyMatching 回来的整张属性表原样当查询条件再传回来。
//! - 返回:kSecReturnData → NSData;kSecReturnAttributes → 属性 NSDictionary(同时要数据时带 v_Data);
//!   kSecMatchLimitAll(或数字上限)→ NSArray;缺省或 kSecMatchLimitOne → 单个对象。返回对象按
//!   Create 规则 +1(原版调用方用 CFRelease 或 autorelease 收尾,见 0x4c3e60 / 0x4c3d64)。
//!   条目保持插入顺序:TMA 的 allAccounts 会把结果整体倒序,「最近登录在前」依赖这个;
//!   TMA 的 setPassword 是先删后加,重新保存的账号会移到末尾。
//! - SecItemUpdate 改所有匹配条目(与真机一致),改完撞唯一键回 errSecDuplicateItem 且不落盘;
//!   SecItemDelete 删所有匹配条目,一条没删到回 errSecItemNotFound。
//! - 持久化:整个钥匙串存成 XML plist,原子写到 guest 路径 <沙盒>/Library/Keychain/keychain.plist
//!   (宿主上位于 user_data_base_path()/touchHLE_sandbox/<应用>/Library/Keychain/keychain.plist,
//!   只在本机用户数据目录内)。每次调用现读现写,不留全局状态;调用量很小(登录期几十次)。
//! - 与真机一致:密码明文存放(真机钥匙串对 App 本身也是明文返回)。日志只打印操作名、类、服务名、
//!   状态码与条数,绝不打印账号和数据内容。
//! - 未实现(游戏未用到):kSecReturnRef/kSecReturnPersistentRef、证书/密钥类条目、cdat/mdat 自动生成。
//!
//! 注意:TMA_SSKeychain 是游戏二进制自带的类(在 __objc_classlist 里,方法实现在 0x4c35cc~0x4c422c),
//! 不是外部框架类,宿主 objc_classes! 替代不了它。默认它仍被 objc/classes.rs 的 substitute_classes
//! 整类 fake 成 nil;设环境变量 MOLE_REAL_KEYCHAIN=1 才不 fake,淘米账号链才会真正走到这里(试验开关)。
//! SFHF/PBSFHF/immob/MAKeychainItemWrapper 没被 fake,本改动对它们立即生效。

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::CFTypeRef;
use crate::frameworks::foundation::{ns_array, ns_dictionary, ns_string, NSUInteger};
use crate::fs::GuestPath;
use crate::mem::{ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr};
use crate::objc::{id, msg, msg_class, nil, release, retain, Class};
use crate::Environment;
use std::io::Cursor;

/// errSecSuccess
const ERR_SEC_SUCCESS: i32 = 0;
/// errSecIO:钥匙串文件写入失败。
const ERR_SEC_IO: i32 = -36;
/// errSecParam:参数不合法(字典为 NULL、缺 kSecClass)。
const ERR_SEC_PARAM: i32 = -50;
/// errSecDuplicateItem:唯一键已存在。
const ERR_SEC_DUPLICATE_ITEM: i32 = -25299;
/// errSecItemNotFound — "the item could not be found".
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

// 苹果 Security.framework 常量的原值(dyld 导出与宿主解析共用同一份)。
const KEY_CLASS: &str = "class";
const KEY_VALUE_DATA: &str = "v_Data";
const KEY_RETURN_DATA: &str = "r_Data";
const KEY_RETURN_ATTRIBUTES: &str = "r_Attributes";
const KEY_MATCH_LIMIT: &str = "m_Limit";
const VALUE_MATCH_LIMIT_ONE: &str = "m_LimitOne";
const VALUE_MATCH_LIMIT_ALL: &str = "m_LimitAll";
const CLASS_GENERIC_PASSWORD: &str = "genp";
const CLASS_INTERNET_PASSWORD: &str = "inet";
const ATTR_ACCOUNT: &str = "acct";
const ATTR_SERVICE: &str = "svce";

/// 通用密码条目的唯一键属性。
const GENP_UNIQUE_ATTRS: &[&str] = &[ATTR_ACCOUNT, ATTR_SERVICE];
/// 互联网密码条目的唯一键属性。
const INET_UNIQUE_ATTRS: &[&str] = &[ATTR_ACCOUNT, "sdmn", "srvr", "ptcl", "atyp", "port", "path"];
/// 不参与匹配的属性(访问控制、同步、时间戳等)。
const MATCH_IGNORED_ATTRS: &[&str] = &[
    "agrp", "pdmn", "sync", "cdat", "mdat", "tomb", "musr", "sha1",
];

/// 钥匙串属性值(只支持游戏实际会放进属性表的几种类型)。
#[derive(Clone)]
enum KcValue {
    Str(String),
    Data(Vec<u8>),
    Int(i64),
    Real(f64),
}

impl KcValue {
    /// 宽松相等:字符串与 NSData 按 UTF-8 字节比较(真机上 gena 等属性会以 NSData 形式返回)。
    fn loosely_equals(&self, other: &KcValue) -> bool {
        match (self, other) {
            (KcValue::Str(a), KcValue::Str(b)) => a == b,
            (KcValue::Data(a), KcValue::Data(b)) => a == b,
            (KcValue::Str(s), KcValue::Data(d)) | (KcValue::Data(d), KcValue::Str(s)) => {
                s.as_bytes() == d.as_slice()
            }
            (KcValue::Int(a), KcValue::Int(b)) => a == b,
            (KcValue::Real(a), KcValue::Real(b)) => a == b,
            (KcValue::Int(i), KcValue::Real(r)) | (KcValue::Real(r), KcValue::Int(i)) => {
                (*i as f64) == *r
            }
            _ => false,
        }
    }

    fn to_plist(&self) -> plist::Value {
        match self {
            KcValue::Str(s) => plist::Value::String(s.clone()),
            KcValue::Data(d) => plist::Value::Data(d.clone()),
            KcValue::Int(i) => plist::Value::Integer((*i).into()),
            KcValue::Real(r) => plist::Value::Real(*r),
        }
    }

    fn from_plist(value: &plist::Value) -> Option<KcValue> {
        if let Some(s) = value.as_string() {
            Some(KcValue::Str(s.to_string()))
        } else if let Some(d) = value.as_data() {
            Some(KcValue::Data(d.to_vec()))
        } else if let Some(i) = value.as_signed_integer() {
            Some(KcValue::Int(i))
        } else {
            value.as_real().map(KcValue::Real)
        }
    }
}

/// 一条钥匙串条目。
#[derive(Clone)]
struct KcItem {
    class: String,
    /// 属性表,保持插入顺序。
    attrs: Vec<(String, KcValue)>,
    /// kSecValueData(密码等机密数据)。
    data: Option<Vec<u8>>,
}

impl KcItem {
    fn attr(&self, key: &str) -> Option<&KcValue> {
        self.attrs
            .iter()
            .find(|entry| entry.0 == key)
            .map(|entry| &entry.1)
    }

    fn set_attr(&mut self, key: &str, value: KcValue) {
        if let Some(entry) = self.attrs.iter_mut().find(|entry| entry.0 == key) {
            entry.1 = value;
        } else {
            self.attrs.push((key.to_string(), value));
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MatchLimit {
    One,
    All,
    Count(usize),
}

/// 解析后的查询/属性字典。
struct ParsedDict {
    class: Option<String>,
    attrs: Vec<(String, KcValue)>,
    data: Option<Vec<u8>>,
    return_data: bool,
    return_attributes: bool,
    limit: MatchLimit,
}

/// 只取服务名用于日志(服务名不敏感;账号与数据一律不打印)。
fn service_for_log(attrs: &[(String, KcValue)]) -> String {
    attrs
        .iter()
        .find(|entry| entry.0 == ATTR_SERVICE)
        .and_then(|entry| match &entry.1 {
            KcValue::Str(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "-".to_string())
}

fn is_kind_of(env: &mut Environment, object: id, class_name: &str) -> bool {
    if object == nil {
        return false;
    }
    let class: Class = env.objc.get_known_class(class_name, &mut env.mem);
    msg![env; object isKindOfClass:class]
}

fn nsdata_to_vec(env: &mut Environment, data: id) -> Vec<u8> {
    let length: NSUInteger = msg![env; data length];
    if length == 0 {
        return Vec::new();
    }
    let bytes: ConstVoidPtr = msg![env; data bytes];
    if bytes.is_null() {
        return Vec::new();
    }
    env.mem.bytes_at(bytes.cast(), length).to_vec()
}

fn guest_to_value(env: &mut Environment, object: id) -> Option<KcValue> {
    if is_kind_of(env, object, "NSString") {
        Some(KcValue::Str(
            ns_string::to_rust_string(env, object).to_string(),
        ))
    } else if is_kind_of(env, object, "NSData") {
        Some(KcValue::Data(nsdata_to_vec(env, object)))
    } else if is_kind_of(env, object, "NSNumber") {
        let as_int: i64 = msg![env; object longLongValue];
        let as_real: f64 = msg![env; object doubleValue];
        if (as_int as f64) == as_real {
            Some(KcValue::Int(as_int))
        } else {
            Some(KcValue::Real(as_real))
        }
    } else {
        None
    }
}

fn guest_to_bool(env: &mut Environment, object: id) -> bool {
    if is_kind_of(env, object, "NSNumber") {
        msg![env; object boolValue]
    } else {
        false
    }
}

fn parse_dict(env: &mut Environment, dict: id) -> ParsedDict {
    let mut parsed = ParsedDict {
        class: None,
        attrs: Vec::new(),
        data: None,
        return_data: false,
        return_attributes: false,
        limit: MatchLimit::One,
    };
    let keys: id = msg![env; dict allKeys];
    let count: NSUInteger = msg![env; keys count];
    for i in 0..count {
        let key: id = msg![env; keys objectAtIndex:i];
        if !is_kind_of(env, key, "NSString") {
            continue;
        }
        let value: id = msg![env; dict objectForKey:key];
        if value == nil {
            continue;
        }
        let key = ns_string::to_rust_string(env, key).to_string();
        match key.as_str() {
            KEY_CLASS => {
                if let Some(KcValue::Str(s)) = guest_to_value(env, value) {
                    parsed.class = Some(s);
                }
            }
            KEY_VALUE_DATA => {
                parsed.data = match guest_to_value(env, value) {
                    Some(KcValue::Data(d)) => Some(d),
                    Some(KcValue::Str(s)) => Some(s.into_bytes()),
                    _ => None,
                };
            }
            KEY_RETURN_DATA => {
                parsed.return_data = guest_to_bool(env, value);
            }
            KEY_RETURN_ATTRIBUTES => {
                parsed.return_attributes = guest_to_bool(env, value);
            }
            KEY_MATCH_LIMIT => {
                parsed.limit = match guest_to_value(env, value) {
                    Some(KcValue::Str(s)) if s == VALUE_MATCH_LIMIT_ALL => MatchLimit::All,
                    Some(KcValue::Int(n)) if n > 1 => MatchLimit::Count(n as usize),
                    _ => MatchLimit::One,
                };
            }
            // 其余 r_/m_/u_/v_ 前缀是返回类型、匹配选项、使用选项(kSecReturnRef、
            // kSecMatchCaseInsensitive、kSecUseOperationPrompt …),游戏未用到,忽略。
            _ if key.starts_with("r_")
                || key.starts_with("m_")
                || key.starts_with("u_")
                || key.starts_with("v_") => {}
            _ => {
                if let Some(v) = guest_to_value(env, value) {
                    parsed.attrs.push((key.clone(), v));
                }
            }
        }
    }
    parsed
}

fn item_matches(item: &KcItem, query: &ParsedDict) -> bool {
    if query.class.as_deref() != Some(item.class.as_str()) {
        return false;
    }
    query.attrs.iter().all(|(key, value)| {
        MATCH_IGNORED_ATTRS.contains(&key.as_str())
            || item
                .attr(key)
                .is_some_and(|stored| stored.loosely_equals(value))
    })
}

fn same_unique_key(a: &KcItem, b: &KcItem) -> bool {
    if a.class != b.class {
        return false;
    }
    let keys = if a.class == CLASS_GENERIC_PASSWORD {
        GENP_UNIQUE_ATTRS
    } else if a.class == CLASS_INTERNET_PASSWORD {
        INET_UNIQUE_ATTRS
    } else {
        return false;
    };
    keys.iter().all(|key| match (a.attr(key), b.attr(key)) {
        (None, None) => true,
        (Some(x), Some(y)) => x.loosely_equals(y),
        _ => false,
    })
}

/// (目录, 文件) 的 guest 路径。
fn keychain_paths(env: &Environment) -> (String, String) {
    let home = env.fs.home_directory().as_str();
    (
        format!("{}/Library/Keychain", home),
        format!("{}/Library/Keychain/keychain.plist", home),
    )
}

fn load_items(env: &mut Environment) -> Vec<KcItem> {
    let (_, file) = keychain_paths(env);
    let Ok(bytes) = env.fs.read(GuestPath::new(file.as_str())) else {
        // 还没有钥匙串文件 = 空钥匙串。
        return Vec::new();
    };
    let root = match plist::Value::from_reader(Cursor::new(bytes)) {
        Ok(root) => root,
        Err(e) => {
            log!("[Keychain] 钥匙串文件无法解析,按空钥匙串处理: {}", e);
            return Vec::new();
        }
    };
    let Some(entries) = root
        .as_dictionary()
        .and_then(|d| d.get("items"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(entry) = entry.as_dictionary() else {
            continue;
        };
        let Some(class) = entry.get("class").and_then(|v| v.as_string()) else {
            continue;
        };
        let mut item = KcItem {
            class: class.to_string(),
            attrs: Vec::new(),
            data: entry
                .get("data")
                .and_then(|v| v.as_data())
                .map(|d| d.to_vec()),
        };
        if let Some(attrs) = entry.get("attrs").and_then(|v| v.as_dictionary()) {
            for (key, value) in attrs.iter() {
                if let Some(value) = KcValue::from_plist(value) {
                    item.set_attr(key, value);
                }
            }
        }
        items.push(item);
    }
    items
}

/// 整体原子写回钥匙串文件,成功返回 true。
fn save_items(env: &mut Environment, items: &[KcItem]) -> bool {
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let mut attrs = plist::Dictionary::new();
        for (key, value) in &item.attrs {
            attrs.insert(key.clone(), value.to_plist());
        }
        let mut entry = plist::Dictionary::new();
        entry.insert(
            "class".to_string(),
            plist::Value::String(item.class.clone()),
        );
        entry.insert("attrs".to_string(), plist::Value::Dictionary(attrs));
        if let Some(data) = &item.data {
            entry.insert("data".to_string(), plist::Value::Data(data.clone()));
        }
        entries.push(plist::Value::Dictionary(entry));
    }
    let mut root = plist::Dictionary::new();
    root.insert("version".to_string(), plist::Value::Integer(1i64.into()));
    root.insert("items".to_string(), plist::Value::Array(entries));

    let mut buf: Vec<u8> = Vec::new();
    if let Err(e) = plist::Value::Dictionary(root).to_writer_xml(&mut buf) {
        log!("[Keychain] 钥匙串序列化失败: {}", e);
        return false;
    }
    let (dir, file) = keychain_paths(env);
    if let Err(e) = env.fs.create_dir_all(GuestPath::new(dir.as_str())) {
        log!("[Keychain] 无法创建钥匙串目录 {}: {:?}", dir, e);
        return false;
    }
    match env.fs.write_atomic(GuestPath::new(file.as_str()), &buf) {
        Ok(()) => true,
        Err(e) => {
            log!("[Keychain] 钥匙串文件写入失败 {}: {:?}", file, e);
            false
        }
    }
}

/// 新建 +1 的 NSData(拷贝一份字节到 guest 堆,由 NSData 负责释放)。
fn new_nsdata(env: &mut Environment, bytes: &[u8]) -> id {
    let len = bytes.len() as GuestUSize;
    // 长度 0 也分配 1 字节,避免把空指针交给 NSData;length 仍然是 0。
    let buf: MutVoidPtr = env.mem.alloc(len.max(1));
    if len != 0 {
        env.mem.bytes_at_mut(buf.cast(), len).copy_from_slice(bytes);
    }
    let data: id = msg_class![env; NSData alloc];
    msg![env; data initWithBytesNoCopy:buf length:len]
}

/// 属性值转成 +1 的 guest 对象。
fn value_to_guest(env: &mut Environment, value: &KcValue) -> id {
    match value {
        KcValue::Str(s) => ns_string::from_rust_string(env, s.clone()),
        KcValue::Data(d) => new_nsdata(env, d),
        KcValue::Int(i) => {
            let number: id = msg_class![env; NSNumber numberWithLongLong:(*i)];
            retain(env, number)
        }
        KcValue::Real(r) => {
            let number: id = msg_class![env; NSNumber numberWithDouble:(*r)];
            retain(env, number)
        }
    }
}

/// 按查询要求的返回类型为单个条目构造 +1 结果对象;没要求任何返回类型时为 nil。
fn build_result(env: &mut Environment, item: &KcItem, query: &ParsedDict) -> id {
    if query.return_attributes {
        let mut pairs: Vec<(id, id)> = Vec::with_capacity(item.attrs.len() + 1);
        for (key, value) in &item.attrs {
            let key = ns_string::from_rust_string(env, key.clone());
            let value = value_to_guest(env, value);
            pairs.push((key, value));
        }
        if query.return_data {
            let key = ns_string::from_rust_string(env, KEY_VALUE_DATA.to_string());
            let value = new_nsdata(env, item.data.as_deref().unwrap_or(&[]));
            pairs.push((key, value));
        }
        // dict_from_keys_and_objects 会拷贝键、retain 值,这里放掉自己持有的引用。
        let dict = ns_dictionary::dict_from_keys_and_objects(env, &pairs);
        for (key, value) in pairs {
            release(env, key);
            release(env, value);
        }
        dict
    } else if query.return_data {
        new_nsdata(env, item.data.as_deref().unwrap_or(&[]))
    } else {
        nil
    }
}

fn SecItemCopyMatching(env: &mut Environment, query: CFTypeRef, result: MutPtr<CFTypeRef>) -> i32 {
    if !result.is_null() {
        env.mem.write(result, nil);
    }
    if query == nil {
        return ERR_SEC_PARAM;
    }
    let parsed = parse_dict(env, query);
    let service = service_for_log(&parsed.attrs);
    if parsed.class.is_none() {
        log!(
            "[Keychain] SecItemCopyMatching svce={}:缺少 kSecClass -> errSecParam",
            service
        );
        return ERR_SEC_PARAM;
    }
    let items = load_items(env);
    let matched: Vec<&KcItem> = items
        .iter()
        .filter(|item| item_matches(item, &parsed))
        .collect();
    if matched.is_empty() {
        log!(
            "[Keychain] SecItemCopyMatching svce={} -> errSecItemNotFound",
            service
        );
        return ERR_SEC_ITEM_NOT_FOUND;
    }
    if !result.is_null() && (parsed.return_data || parsed.return_attributes) {
        let object = match parsed.limit {
            MatchLimit::One => build_result(env, matched[0], &parsed),
            MatchLimit::All | MatchLimit::Count(_) => {
                let max = match parsed.limit {
                    MatchLimit::Count(n) => n,
                    _ => usize::MAX,
                };
                let mut objects: Vec<id> = Vec::new();
                for item in matched.iter().take(max) {
                    objects.push(build_result(env, item, &parsed));
                }
                // from_vec 接管这些 +1 引用,返回的数组本身也是 +1。
                ns_array::from_vec(env, objects)
            }
        };
        env.mem.write(result, object);
    }
    log!(
        "[Keychain] SecItemCopyMatching svce={} -> errSecSuccess(匹配 {} 条)",
        service,
        matched.len()
    );
    ERR_SEC_SUCCESS
}

fn SecItemAdd(env: &mut Environment, attributes: CFTypeRef, result: MutPtr<CFTypeRef>) -> i32 {
    if !result.is_null() {
        env.mem.write(result, nil);
    }
    if attributes == nil {
        return ERR_SEC_PARAM;
    }
    let parsed = parse_dict(env, attributes);
    let service = service_for_log(&parsed.attrs);
    let Some(class) = parsed.class.clone() else {
        log!(
            "[Keychain] SecItemAdd svce={}:缺少 kSecClass -> errSecParam",
            service
        );
        return ERR_SEC_PARAM;
    };
    let mut item = KcItem {
        class,
        attrs: parsed.attrs.clone(),
        data: parsed.data.clone(),
    };
    // 真机上通用密码条目总带 acct/svce(缺省为空串);补上后唯一键和匹配行为才与真机一致。
    if item.class == CLASS_GENERIC_PASSWORD {
        for key in GENP_UNIQUE_ATTRS {
            if item.attr(key).is_none() {
                item.set_attr(key, KcValue::Str(String::new()));
            }
        }
    }
    let mut items = load_items(env);
    if items
        .iter()
        .any(|existing| same_unique_key(existing, &item))
    {
        log!(
            "[Keychain] SecItemAdd class={} svce={} -> errSecDuplicateItem",
            item.class,
            service
        );
        return ERR_SEC_DUPLICATE_ITEM;
    }
    items.push(item.clone());
    if !save_items(env, &items) {
        return ERR_SEC_IO;
    }
    if !result.is_null() && (parsed.return_data || parsed.return_attributes) {
        let object = build_result(env, &item, &parsed);
        env.mem.write(result, object);
    }
    log!(
        "[Keychain] SecItemAdd class={} svce={} -> errSecSuccess(钥匙串共 {} 条)",
        item.class,
        service,
        items.len()
    );
    ERR_SEC_SUCCESS
}

fn SecItemUpdate(env: &mut Environment, query: CFTypeRef, attributes_to_update: CFTypeRef) -> i32 {
    if query == nil || attributes_to_update == nil {
        return ERR_SEC_PARAM;
    }
    let parsed = parse_dict(env, query);
    let service = service_for_log(&parsed.attrs);
    if parsed.class.is_none() {
        log!(
            "[Keychain] SecItemUpdate svce={}:缺少 kSecClass -> errSecParam",
            service
        );
        return ERR_SEC_PARAM;
    }
    // kSecClass 出现在待更新字典里时被 parse_dict 放进 class 字段,这里不使用,等于忽略。
    let update = parse_dict(env, attributes_to_update);
    let mut items = load_items(env);
    let indices: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item_matches(item, &parsed))
        .map(|(index, _)| index)
        .collect();
    if indices.is_empty() {
        log!(
            "[Keychain] SecItemUpdate svce={} -> errSecItemNotFound",
            service
        );
        return ERR_SEC_ITEM_NOT_FOUND;
    }
    for &index in &indices {
        let item = &mut items[index];
        for (key, value) in &update.attrs {
            item.set_attr(key, value.clone());
        }
        if let Some(data) = &update.data {
            item.data = Some(data.clone());
        }
    }
    for &index in &indices {
        let clash = items
            .iter()
            .enumerate()
            .any(|(other, item)| other != index && same_unique_key(item, &items[index]));
        if clash {
            log!(
                "[Keychain] SecItemUpdate svce={} -> errSecDuplicateItem",
                service
            );
            return ERR_SEC_DUPLICATE_ITEM;
        }
    }
    if !save_items(env, &items) {
        return ERR_SEC_IO;
    }
    log!(
        "[Keychain] SecItemUpdate svce={} -> errSecSuccess(更新 {} 条)",
        service,
        indices.len()
    );
    ERR_SEC_SUCCESS
}

fn SecItemDelete(env: &mut Environment, query: CFTypeRef) -> i32 {
    if query == nil {
        return ERR_SEC_PARAM;
    }
    let parsed = parse_dict(env, query);
    let service = service_for_log(&parsed.attrs);
    if parsed.class.is_none() {
        log!(
            "[Keychain] SecItemDelete svce={}:缺少 kSecClass -> errSecParam",
            service
        );
        return ERR_SEC_PARAM;
    }
    let mut items = load_items(env);
    let before = items.len();
    items.retain(|item| !item_matches(item, &parsed));
    let removed = before - items.len();
    if removed == 0 {
        log!(
            "[Keychain] SecItemDelete svce={} -> errSecItemNotFound",
            service
        );
        return ERR_SEC_ITEM_NOT_FOUND;
    }
    if !save_items(env, &items) {
        return ERR_SEC_IO;
    }
    log!(
        "[Keychain] SecItemDelete svce={} -> errSecSuccess(删除 {} 条)",
        service,
        removed
    );
    ERR_SEC_SUCCESS
}

// [扫描修 2026-09-15] 常量值改为苹果原值,并补上二进制里仍是未处理重定位的
// kSecMatchLimitAll/kSecAttrLabel/kSecAttrDescription/kSecAttrAccessGroup/kSecAttrAccessible*。
// 刻意不导出 kSecImportExportPassphrase/kSecImportItem*:对应的 SecPKCS12Import 没实现,
// 常量变成非空后,「按常量地址判断是否可用」的弱链接代码反而会去调未实现函数。
pub const CONSTANTS: ConstantExports = &[
    ("_kSecClass", HostConstant::NSString(KEY_CLASS)),
    (
        "_kSecClassGenericPassword",
        HostConstant::NSString(CLASS_GENERIC_PASSWORD),
    ),
    ("_kSecAttrAccount", HostConstant::NSString(ATTR_ACCOUNT)),
    ("_kSecAttrService", HostConstant::NSString(ATTR_SERVICE)),
    ("_kSecAttrGeneric", HostConstant::NSString("gena")),
    ("_kSecAttrLabel", HostConstant::NSString("labl")),
    ("_kSecAttrDescription", HostConstant::NSString("desc")),
    ("_kSecAttrAccessGroup", HostConstant::NSString("agrp")),
    ("_kSecAttrAccessible", HostConstant::NSString("pdmn")),
    (
        "_kSecAttrAccessibleWhenUnlocked",
        HostConstant::NSString("ak"),
    ),
    (
        "_kSecAttrAccessibleAfterFirstUnlock",
        HostConstant::NSString("ck"),
    ),
    (
        "_kSecAttrAccessibleAlwaysThisDeviceOnly",
        HostConstant::NSString("dku"),
    ),
    ("_kSecValueData", HostConstant::NSString(KEY_VALUE_DATA)),
    ("_kSecReturnData", HostConstant::NSString(KEY_RETURN_DATA)),
    (
        "_kSecReturnAttributes",
        HostConstant::NSString(KEY_RETURN_ATTRIBUTES),
    ),
    ("_kSecMatchLimit", HostConstant::NSString(KEY_MATCH_LIMIT)),
    (
        "_kSecMatchLimitOne",
        HostConstant::NSString(VALUE_MATCH_LIMIT_ONE),
    ),
    (
        "_kSecMatchLimitAll",
        HostConstant::NSString(VALUE_MATCH_LIMIT_ALL),
    ),
];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(SecItemCopyMatching(_, _)),
    export_c_func!(SecItemAdd(_, _)),
    export_c_func!(SecItemUpdate(_, _)),
    export_c_func!(SecItemDelete(_)),
];

pub const DYLIB: crate::dyld::HostDylib = crate::dyld::HostDylib {
    path: "/System/Library/Frameworks/Security.framework/Security",
    aliases: &[],
    class_exports: &[],
    constant_exports: &[CONSTANTS],
    function_exports: &[FUNCTIONS],
};
