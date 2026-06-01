#!/usr/bin/env python3
# 解析 32位 ARMv7 ObjC2 元数据:__objc_classlist -> 每个 class_t -> class_ro_t.name
# vmaddr - 0x4000 = fileoff(本二进制 __TEXT/__DATA/__LINKEDIT 统一)
import sys, struct
f = open(sys.argv[1], "rb").read()
DIFF = 0x4000
def rd32(vaddr):
    o = vaddr - DIFF
    return struct.unpack_from("<I", f, o)[0] if 0 <= o+4 <= len(f) else 0
def cstr(vaddr):
    o = vaddr - DIFF; b = bytearray()
    while 0 <= o < len(f) and f[o] != 0 and len(b) < 128:
        b.append(f[o]); o += 1
    return b.decode("ascii", "replace")

# 找 __objc_classlist section(解析 load commands)
magic, cput, cpsub, ftype, ncmds, scmds, flags = struct.unpack_from("<IiiIIII", f, 0)
off = 28  # 32位 mach_header 大小
classlist_addr = classlist_size = 0
for _ in range(ncmds):
    cmd, csize = struct.unpack_from("<II", f, off)
    if cmd == 1:  # LC_SEGMENT
        segname = f[off+8:off+24].split(b'\0')[0].decode()
        nsects = struct.unpack_from("<I", f, off+48)[0]
        so = off + 56
        for _s in range(nsects):
            sn = f[so:so+16].split(b'\0')[0].decode()
            addr, size = struct.unpack_from("<II", f, so+32)
            if sn == "__objc_classlist":
                classlist_addr, classlist_size = addr, size
            so += 68
    off += csize

print(f"__objc_classlist @ 0x{classlist_addr:x} size 0x{classlist_size:x} -> {classlist_size//4} 类", file=sys.stderr)

out = open(sys.argv[2], "w")
n = 0
for i in range(classlist_size // 4):
    cls = rd32(classlist_addr + i*4)
    if not cls: continue
    data = rd32(cls + 16) & ~3      # class_t.data -> class_ro_t
    name = cstr(rd32(data + 16))    # class_ro_t.name @ +16
    if name and name.isprintable() and not name.startswith("�"):
        out.write(f"0x{cls:08x}\t{name}\n"); n += 1
        if n <= 18: print(f"  0x{cls:08x} -> {name}", file=sys.stderr)
out.close()
print(f"写出 {n} 条 class_t->name", file=sys.stderr)
