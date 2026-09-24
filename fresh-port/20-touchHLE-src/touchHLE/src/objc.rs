/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Objective-C runtime.
//!
//! Apple's [Programming with Objective-C](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/ProgrammingWithObjectiveC/Introduction/Introduction.html)
//! is a useful introduction to the language from a user's perspective.
//! There are further resources in the child modules of this module, but they
//! are more implementation-specific.
//!
//! The strategy for this emulator will be to provide our own implementations of
//! an Objective-C runtime and libraries for it (Foundation etc). These
//! implementations will be "host code": Rust code forming part of the emulator,
//! not emulated code. The runtime will need to be able to handle classes that
//! originate from the guest app, classes defined by the host, and sometimes
//! classes that are both (considering Objective-C's support for inheritance,
//! categories and dynamic class editing).

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant, HostDylib};
use crate::objc::messages::ThreadInitializer;
use crate::MutexId;
// [MoleWorld P1] FxHashMap (faster hash for the u32-keyed ObjC dispatch tables on the
// per-message hot path; std HashMap's SipHash is DoS-resistant overkill for internal tables).
use rustc_hash::FxHashMap;

mod classes;
mod messages;
mod methods;
mod objects;
mod properties;
mod selectors;
mod synchronization;

pub use classes::{objc_classes, Class, ClassExports, ClassTemplate};
// note_present 的唯一调用方(eagl.rs 的 [PRESENT] 出帧计数)只在 interp_hb / debug 构建编译,
// release 下这个再导出没人用;保留导出(让 MSG_N 这组出帧失速计数不被当成死代码),只压掉未使用告警。
#[allow(unused_imports)]
pub use messages::{
    autorelease, msg, msg_class, msg_send, msg_send_no_type_checking, msg_send_super2, msg_super,
    note_present, objc_super, release, retain,
};
pub use methods::{HostIMP, IMP};
pub use objects::{
    id, impl_HostObject_with_superclass, nil, AnyHostObject, HostObject, TrivialHostObject,
};
pub use properties::todo_objc_setter;
pub use selectors::{selector, SEL};

use crate::mem::{ConstVoidPtr, MutPtr};
use crate::Environment;
use classes::{
    class_getInstanceSize, class_getSuperclass, objc_getClass, ClassHostObject, FakeClass,
    UnimplementedClass,
};
pub(crate) use messages::objc_msgSend;
use messages::{objc_msgSendSuper2, objc_msgSend_stret, MsgSendSignature, MsgSendSuperSignature};
use methods::method_list_t;
use objects::{objc_object, object_getClass, HostObjectEntry};
use properties::{ivar_list_t, objc_copyStruct, objc_getProperty, objc_setProperty};
use selectors::sel_registerName;
use synchronization::{objc_sync_enter, objc_sync_exit};

/// Typedef for `NSZone *`. This is a [fossil type] found in the signature of
/// `allocWithZone:` and similar methods. Its value is always ignored.
///
/// [fossil type]: https://en.wiktionary.org/wiki/fossil_word
pub type NSZonePtr = crate::mem::MutVoidPtr;

/// Main type holding Objective-C runtime state.
pub struct ObjC {
    /// Known selectors (interned method name strings).
    selectors: FxHashMap<String, SEL>,

    /// Mapping of known (guest) object pointers to their host objects.
    ///
    /// If an object isn't in this map, we will consider it not to exist.
    objects: FxHashMap<id, HostObjectEntry>,
    /// [MoleWorld iOS · 性能] 方法解析缓存:(起始类, 选择子, 是否 objc_msgSendSuper2) → 实现所在的类
    /// (nil = 整条链都没有)。派发时先查它,命中即省掉沿超类链逐级查表。`method_cache_epoch` 与
    /// [crate::objc::methods::METHOD_TABLE_EPOCH] 不一致时整表作废(方法表有变动,只发生在加载期)。
    /// (合并注:与 main 统一用 rustc_hash 的 FxHashMap,算法与 iOS 自带 crate::fxhash 同款。)
    pub(super) method_cache: FxHashMap<(u32, u32, bool), Class>,
    pub(super) method_cache_epoch: u32,

    /// Known classes.
    ///
    /// Look at the `isa` to get the metaclass for a class.
    classes: FxHashMap<String, Class>,

    /// Mutexes used in @synchronized blocks (objc_sync_enter/exit).
    sync_mutexes: FxHashMap<id, MutexId>,

    /// Mutexes for running the +initialize function.
    initializer_threads: FxHashMap<id, ThreadInitializer>,

    /// Temporary storage for optional type information when sending a message.
    /// Type information isn't part of the `objc_msgSend` ABI, so an alternative
    /// channel is needed.
    message_type_info: Option<(std::any::TypeId, &'static str)>,

    /// [MoleWorld offline port] Precomputed interned SELs for the offline-hook
    /// block in `objc::messages::objc_msgSend_inner`. Filled once, lazily, on
    /// the first dispatch (see `ObjC::is_mole_hook_sel`). Lets the hot path
    /// reject the overwhelmingly-common non-hook selector with a few integer
    /// (pointer) comparisons instead of a chain of guest-cstr reads + strcmps.
    mole_hook_sels: Option<Vec<SEL>>,
}

impl ObjC {
    /// [性能观测] 当前活着的 objc 对象数(host 侧对象表大小)。
    /// (合并复核:唯一调用点 mole_perf::tick 只在 iOS / 解释器构建上调,桌面上不用它,免未使用告警。)
    #[cfg_attr(not(any(target_os = "ios", feature = "cpu_interpreter")), allow(dead_code))]
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
    pub fn new() -> ObjC {
        ObjC {
            selectors: FxHashMap::default(),
            objects: FxHashMap::default(),
            method_cache: FxHashMap::default(),
            method_cache_epoch: 0,
            classes: FxHashMap::default(),
            sync_mutexes: FxHashMap::default(),
            initializer_threads: FxHashMap::default(),
            message_type_info: None,
            mole_hook_sels: None,
        }
    }
}

pub const DYLIB: HostDylib = HostDylib {
    path: "/usr/lib/libobjc.A.dylib",
    aliases: &["/usr/lib/libobjc.dylib"],
    class_exports: &[],
    constant_exports: &[CONSTANTS],
    function_exports: &[FUNCTIONS],
};

const CONSTANTS: ConstantExports = &[
    // We don't use these in our Objective-C runtime, but exporting useless
    // symbols for these silences the warning about the unhandled relocation,
    // and avoids a linker error for the integration tests.
    ("__objc_empty_vtable", HostConstant::NullPtr),
    ("__objc_empty_cache", HostConstant::NullPtr),
];

/// Block support is iOS 4+, but it seems like Block Runtime Helpers
/// could still be called on even if minimal iOS version is set to 3.x?
///
/// ref. <https://clang.llvm.org/docs/Block-ABI-Apple.html#runtime-helper-functions>
fn _Block_object_dispose(_env: &mut Environment, object: ConstVoidPtr, flags: i32) {
    // `BLOCK_FIELD_IS_BYREF` flag defines an on stack structure holding
    // the __block variable. It is _probably_ safe to ignore.
    // TODO: properly implement for block support
    assert!(flags == 8); // BLOCK_FIELD_IS_BYREF
    log!(
        "Warning: Ignoring _Block_object_dispose({:?}, BLOCK_FIELD_IS_BYREF)",
        object
    );
}

// ARC (Automatic Reference Counting) runtime support. These are plain C
// functions (not objc_msgSend) emitted by the ARC-aware compiler; MoleWorld's
// JSONKit and other ARC code call them directly. Implemented via the normal
// retain/release/autorelease messages.
#[allow(non_snake_case)]
fn objc_retain(env: &mut Environment, obj: id) -> id {
    if obj == nil { return nil; }
    retain(env, obj)
}
#[allow(non_snake_case)]
fn objc_release(env: &mut Environment, obj: id) {
    if obj == nil { return; }
    release(env, obj)
}
#[allow(non_snake_case)]
fn objc_autorelease(env: &mut Environment, obj: id) -> id {
    if obj == nil { return nil; }
    autorelease(env, obj)
}
#[allow(non_snake_case)]
fn objc_retainAutoreleasedReturnValue(env: &mut Environment, obj: id) -> id {
    objc_retain(env, obj)
}
#[allow(non_snake_case)]
fn objc_autoreleaseReturnValue(env: &mut Environment, obj: id) -> id {
    objc_autorelease(env, obj)
}
#[allow(non_snake_case)]
fn objc_retainAutorelease(env: &mut Environment, obj: id) -> id {
    let obj = objc_retain(env, obj);
    objc_autorelease(env, obj)
}
#[allow(non_snake_case)]
fn objc_retainAutoreleaseReturnValue(env: &mut Environment, obj: id) -> id {
    objc_retainAutorelease(env, obj)
}
/// `objc_storeStrong(id *location, id obj)`: standard ARC strong-store.
#[allow(non_snake_case)]
fn objc_storeStrong(env: &mut Environment, location: MutPtr<id>, obj: id) {
    let old: id = if location.is_null() { nil } else { env.mem.read(location) };
    let obj = objc_retain(env, obj);
    if !location.is_null() {
        env.mem.write(location, obj);
    }
    objc_release(env, old);
}

const FUNCTIONS: FunctionExports = &[
    export_c_func!(class_getInstanceSize(_)),
    export_c_func!(class_getSuperclass(_)),
    export_c_func!(objc_retain(_)),
    export_c_func!(objc_release(_)),
    export_c_func!(objc_autorelease(_)),
    export_c_func!(objc_retainAutoreleasedReturnValue(_)),
    export_c_func!(objc_autoreleaseReturnValue(_)),
    export_c_func!(objc_retainAutorelease(_)),
    export_c_func!(objc_retainAutoreleaseReturnValue(_)),
    export_c_func!(objc_storeStrong(_, _)),
    export_c_func!(objc_msgSend(_, _)),
    export_c_func!(objc_msgSend_stret(_, _, _)),
    export_c_func!(objc_msgSendSuper2(_, _)),
    export_c_func!(objc_getClass(_)),
    export_c_func!(objc_getProperty(_, _, _, _)),
    export_c_func!(objc_setProperty(_, _, _, _, _, _)),
    export_c_func!(objc_copyStruct(_, _, _, _, _)),
    export_c_func!(objc_sync_enter(_)),
    export_c_func!(objc_sync_exit(_)),
    export_c_func!(object_getClass(_)),
    export_c_func!(sel_registerName(_)),
    export_c_func!(_Block_object_dispose(_, _)),
];
