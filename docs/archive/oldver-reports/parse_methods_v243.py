#!/usr/bin/env python3
# 解析 32位 ARMv7 ObjC2 元数据: __objc_classlist -> class_t -> class_ro_t -> method_list_t
# 同时解析 categories(__objc_catlist)。输出 类\t方法\t地址\t(±实例/类方法)
import sys, struct

path = sys.argv[1]
f = open(path, "rb").read()

# 解析 load commands,建立 section vmaddr->fileoff 映射表
magic, cput, cpsub, ftype, ncmds, scmds, flags = struct.unpack_from("<IiiIIII", f, 0)
off = 28
sections = []  # (name, segname, addr, size, offset)
seg_map = []   # (vmaddr, vmsize, fileoff)
named = {}
for _ in range(ncmds):
    cmd, csize = struct.unpack_from("<II", f, off)
    if cmd == 1:  # LC_SEGMENT
        segname = f[off+8:off+24].split(b'\0')[0].decode()
        vmaddr, vmsize, foff, fsize = struct.unpack_from("<IIII", f, off+24)
        seg_map.append((vmaddr, vmsize, foff))
        nsects = struct.unpack_from("<I", f, off+48)[0]
        so = off + 56
        for _s in range(nsects):
            sn = f[so:so+16].split(b'\0')[0].decode()
            addr, size = struct.unpack_from("<II", f, so+32)
            secoff = struct.unpack_from("<I", f, so+40)[0]
            sections.append((sn, segname, addr, size, secoff))
            named[(segname, sn)] = (addr, size, secoff)
            so += 68
    off += csize

def v2o(vaddr):
    # vmaddr -> fileoff via segment map
    for vm, vs, fo in seg_map:
        if vm <= vaddr < vm + vs:
            return fo + (vaddr - vm)
    return None

def rd32(vaddr):
    o = v2o(vaddr)
    if o is None or o+4 > len(f): return 0
    return struct.unpack_from("<I", f, o)[0]

def cstr(vaddr):
    o = v2o(vaddr)
    if o is None: return ""
    b = bytearray()
    while 0 <= o < len(f) and f[o] != 0 and len(b) < 256:
        b.append(f[o]); o += 1
    return b.decode("ascii", "replace")

def parse_method_list(mlist_addr, cls_name, is_class, out):
    if not mlist_addr: return 0
    o = v2o(mlist_addr)
    if o is None: return 0
    entsize, count = struct.unpack_from("<II", f, o)
    entsize &= 0xffff  # low bits = flags in newer; mask
    if entsize == 0: entsize = 12
    n = 0
    for i in range(count):
        base = mlist_addr + 8 + i*entsize
        name_ptr = rd32(base)
        # types = rd32(base+4)
        imp = rd32(base+8)
        sel = cstr(name_ptr)
        if sel:
            sign = "+" if is_class else "-"
            out.write(f"{cls_name}\t{sel}\t0x{imp:08x}\t{sign}\n")
            n += 1
    return n

out = open(sys.argv[2], "w")
cls_out = open(sys.argv[3], "w") if len(sys.argv) > 3 else None

# class_ro_t (32位): flags(0) start(4) size(8) reserved? ... ivarLayout, name@16? -> 实际布局:
# struct class_ro_t { uint32 flags; uint32 instanceStart; uint32 instanceSize;
#   uint32 ivarLayout; uint32 name; uint32 baseMethods; uint32 baseProtocols;
#   uint32 ivars; uint32 weakIvarLayout; uint32 baseProperties; }
# 偏移: name@16, baseMethods@20

total_classes = 0
total_methods = 0

cl_addr, cl_size, _ = named.get(("__DATA","__objc_classlist"), (0,0,0))
for i in range(cl_size // 4):
    cls = rd32(cl_addr + i*4)
    if not cls: continue
    # 实例方法
    data = rd32(cls + 16) & ~3
    name = cstr(rd32(data + 16))
    if not (name and name.isprintable()): continue
    total_classes += 1
    if cls_out: cls_out.write(f"0x{cls:08x}\t{name}\n")
    base_methods = rd32(data + 20)
    total_methods += parse_method_list(base_methods, name, False, out)
    # 类方法: metaclass = class_t.isa @ +0
    meta = rd32(cls + 0)
    if meta:
        mdata = rd32(meta + 16) & ~3
        m_methods = rd32(mdata + 20)
        total_methods += parse_method_list(m_methods, name, True, out)

# categories
cat_addr, cat_size, _ = named.get(("__DATA","__objc_catlist"), (0,0,0))
cat_count = 0
for i in range(cat_size // 4):
    cat = rd32(cat_addr + i*4)
    if not cat: continue
    # category_t { name; cls; instanceMethods; classMethods; protocols; props }
    cat_name = cstr(rd32(cat + 0))
    inst_m = rd32(cat + 8)
    cls_m = rd32(cat + 12)
    label = f"({cat_name})"
    total_methods += parse_method_list(inst_m, label, False, out)
    total_methods += parse_method_list(cls_m, label, True, out)
    cat_count += 1

out.close()
if cls_out: cls_out.close()
print(f"classlist@0x{cl_addr:x} size0x{cl_size:x}; classes={total_classes} methods={total_methods} categories={cat_count}", file=sys.stderr)
