//! 最小化 guest 内存后端,替代原版 src/mem.rs。
//!
//! 关键点:`Ptr<T, MUT>` 指针类型机制是从原版 mem.rs 逐字搬来的(保证解释器看到的
//! 指针 ABI 完全一致);唯一的改动是 `Mem` 的 backing store —— 原版是
//! `[u8; 1<<32]` 4GiB 单体数组(mmap/VirtualAlloc 懒提交,wasm 装不下),这里换成一块
//! 小的 `Vec<u8>`。这正是 WASM 移植要做的 "mem.rs 手术",在这个 PoC 里先验证它对解释器
//! 透明(解释器源码零改动即可对接)。
//!
//! 解释器实际只调用 Mem 的三个方法:
//!   - `get_bytes_fallible(ConstVoidPtr, u32) -> Option<&[u8]>`
//!   - `get_bytes_fallible_mut(ConstVoidPtr, u32) -> Option<&mut [u8]>`
//!   - `bytes_at_mut(MutPtr<u8>, u32) -> &mut [u8]`
//! 以及 `Ptr::from_bits` / `to_bits`。其余原版 Mem API(alloc/heap/read/write 泛型等)
//! 解释器不碰,故此处不实现。

// ===========================================================================
// 以下指针类型机制 = 原版 src/mem.rs 第 25-208 行逐字搬运(仅省略 wcstr 相关)。
// ===========================================================================

/// Equivalent of `usize` for guest memory.
pub type GuestUSize = u32;

/// Equivalent of `isize` for guest memory.
#[allow(dead_code)]
pub type GuestISize = i32;

/// [std::mem::size_of], but returning a [GuestUSize].
pub const fn guest_size_of<T: Sized>() -> GuestUSize {
    assert!(std::mem::size_of::<T>() <= u32::MAX as usize);
    std::mem::size_of::<T>() as u32
}

/// Internal type for representing an untyped virtual address.
type VAddr = GuestUSize;

/// Pointer type for guest memory, or the "guest pointer" type.
#[repr(transparent)]
pub struct Ptr<T, const MUT: bool>(VAddr, std::marker::PhantomData<T>);

impl<T, const MUT: bool> Clone for Ptr<T, MUT> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T, const MUT: bool> Copy for Ptr<T, MUT> {}
impl<T, const MUT: bool> PartialEq for Ptr<T, MUT> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T, const MUT: bool> Eq for Ptr<T, MUT> {}
impl<T, const MUT: bool> std::hash::Hash for Ptr<T, MUT> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// Constant guest pointer type (like Rust's `*const T`).
pub type ConstPtr<T> = Ptr<T, false>;
/// Mutable guest pointer type (like Rust's `*mut T`).
pub type MutPtr<T> = Ptr<T, true>;
#[allow(dead_code)]
/// Constant guest pointer-to-void type (like C's `const void *`)
pub type ConstVoidPtr = ConstPtr<std::ffi::c_void>;
#[allow(dead_code)]
/// Mutable guest pointer-to-void type (like C's `void *`)
pub type MutVoidPtr = MutPtr<std::ffi::c_void>;

impl<T, const MUT: bool> Ptr<T, MUT> {
    pub const fn null() -> Self {
        Ptr(0, std::marker::PhantomData)
    }

    pub fn to_bits(self) -> VAddr {
        self.0
    }
    pub const fn from_bits(bits: VAddr) -> Self {
        Ptr(bits, std::marker::PhantomData)
    }

    pub fn cast<U>(self) -> Ptr<U, MUT> {
        Ptr::<U, MUT>::from_bits(self.to_bits())
    }

    pub fn cast_void(self) -> Ptr<std::ffi::c_void, MUT> {
        self.cast()
    }

    pub fn is_null(self) -> bool {
        self.to_bits() == 0
    }
}

impl<T> ConstPtr<T> {
    #[allow(dead_code)]
    pub fn cast_mut(self) -> MutPtr<T> {
        Ptr::from_bits(self.to_bits())
    }
}
impl<T> MutPtr<T> {
    #[allow(dead_code)]
    pub fn cast_const(self) -> ConstPtr<T> {
        Ptr::from_bits(self.to_bits())
    }
}

impl<T, const MUT: bool> Default for Ptr<T, MUT> {
    fn default() -> Self {
        Self::null()
    }
}

impl<T, const MUT: bool> std::fmt::Debug for Ptr<T, MUT> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() {
            write!(f, "(null)")
        } else {
            write!(f, "{:#x}", self.to_bits())
        }
    }
}

// C-like pointer arithmetic
impl<T, const MUT: bool> std::ops::Add<GuestUSize> for Ptr<T, MUT> {
    type Output = Self;

    fn add(self, other: GuestUSize) -> Self {
        let size: GuestUSize = guest_size_of::<T>();
        assert_ne!(size, 0);
        Self::from_bits(
            self.to_bits()
                .checked_add(other.checked_mul(size).unwrap())
                .unwrap(),
        )
    }
}
impl<T, const MUT: bool> std::ops::AddAssign<GuestUSize> for Ptr<T, MUT> {
    fn add_assign(&mut self, rhs: GuestUSize) {
        *self = *self + rhs;
    }
}
impl<T, const MUT: bool> std::ops::Sub<GuestUSize> for Ptr<T, MUT> {
    type Output = Self;

    fn sub(self, other: GuestUSize) -> Self {
        let size: GuestUSize = guest_size_of::<T>();
        assert_ne!(size, 0);
        Self::from_bits(
            self.to_bits()
                .checked_sub(other.checked_mul(size).unwrap())
                .unwrap(),
        )
    }
}
impl<T, const MUT: bool> std::ops::SubAssign<GuestUSize> for Ptr<T, MUT> {
    fn sub_assign(&mut self, rhs: GuestUSize) {
        *self = *self - rhs;
    }
}

/// Marker trait for types that can be safely read from guest memory.
/// (原版 SafeRead/SafeWrite,解释器虽不直接调用泛型 read/write,但保留以备扩展。)
pub unsafe trait SafeRead: Sized {}
unsafe impl SafeRead for bool {}
unsafe impl SafeRead for i8 {}
unsafe impl SafeRead for u8 {}
unsafe impl SafeRead for i16 {}
unsafe impl SafeRead for u16 {}
unsafe impl SafeRead for i32 {}
unsafe impl SafeRead for u32 {}
unsafe impl SafeRead for i64 {}
unsafe impl SafeRead for u64 {}
unsafe impl SafeRead for f32 {}
unsafe impl SafeRead for f64 {}
unsafe impl<T, const MUT: bool> SafeRead for Ptr<T, MUT> {}

// ===========================================================================
// 以下 Mem 是本 PoC 新写的最小后端(Vec backing),不是原版代码。
// 方法签名与语义(null 段检查、小端切片)对齐原版,保证解释器透明对接。
// ===========================================================================

pub struct Mem {
    data: Vec<u8>,
    null_segment_size: VAddr,
}

impl Mem {
    /// 分配一块 `size` 字节的 guest 内存。`null_page_count` 个 4KiB 页作为 null 陷阱区
    /// (与原版 `InterpreterCpu::new(null_page_count)` 的语义一致)。
    pub fn new(size: usize, null_page_count: u32) -> Mem {
        Mem {
            data: vec![0u8; size],
            null_segment_size: null_page_count * 0x1000,
        }
    }

    /// 把一段机器码/数据写入 guest 地址(基准初始化用,非热路径)。
    pub fn write_blob(&mut self, addr: u32, bytes: &[u8]) {
        let start = addr as usize;
        self.data[start..start + bytes.len()].copy_from_slice(bytes);
    }

    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    // ---- 解释器实际调用的三个方法(签名与原版 mem.rs 完全一致)----

    pub fn get_bytes_fallible(&self, addr: ConstVoidPtr, count: GuestUSize) -> Option<&[u8]> {
        if addr.to_bits() < self.null_segment_size {
            return None;
        }
        self.data
            .get(addr.to_bits() as usize..)?
            .get(..count as usize)
    }

    pub fn get_bytes_fallible_mut(
        &mut self,
        addr: ConstVoidPtr,
        count: GuestUSize,
    ) -> Option<&mut [u8]> {
        if addr.to_bits() < self.null_segment_size {
            return None;
        }
        self.data
            .get_mut(addr.to_bits() as usize..)?
            .get_mut(..count as usize)
    }

    pub fn bytes_at_mut(&mut self, ptr: MutPtr<u8>, count: GuestUSize) -> &mut [u8] {
        if ptr.to_bits() < self.null_segment_size {
            panic!(
                "Attempted null-page access at {:#x} ({:#x} bytes)",
                ptr.to_bits(),
                count
            )
        }
        &mut self.data[ptr.to_bits() as usize..][..count as usize]
    }
}
