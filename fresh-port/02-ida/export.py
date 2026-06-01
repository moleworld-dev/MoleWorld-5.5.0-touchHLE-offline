# IDA 9.3 headless: 导出函数表/导入面 + Hex-Rays 反编译关键函数
# 全新自做,不依赖任何旧产物
import idautils, idc, ida_funcs, ida_hexrays, ida_nalt, ida_bytes, ida_segment
import os, traceback

OUT = os.environ.get("MOLE_OUT", "/tmp/mole_ida_out")
os.makedirs(OUT, exist_ok=True)

def log(msg):
    with open(os.path.join(OUT, "script.log"), "a") as f:
        f.write(msg + "\n")

try:
    idc.auto_wait()  # 等自动分析跑完

    # 1) 全部函数(含 IDA 解析出的 ObjC 方法名 -[Class sel] / +[Class sel])
    funcs = [(ea, idc.get_func_name(ea) or "") for ea in idautils.Functions()]
    with open(os.path.join(OUT, "functions.txt"), "w") as f:
        for ea, name in funcs:
            f.write("0x%08x\t%s\n" % (ea, name))
    log("functions: %d" % len(funcs))

    # 2) ObjC 类名(从 -[...]/+[...] 方法名抽取)
    classes = set()
    for ea, name in funcs:
        if name[:2] in ("-[", "+["):
            inner = name[2:].split(" ")[0]
            classes.add(inner)
    with open(os.path.join(OUT, "objc_classes.txt"), "w") as f:
        for c in sorted(classes):
            f.write(c + "\n")
    log("objc classes: %d" % len(classes))

    # 3) 导入面(按 framework/dylib 分组)
    with open(os.path.join(OUT, "imports.txt"), "w") as f:
        for i in range(ida_nalt.get_import_module_qty()):
            mod = ida_nalt.get_import_module_name(i) or "?"
            def cb(ea, name, ordn, _mod=mod, _f=f):
                _f.write("%s\t0x%08x\t%s\n" % (_mod, ea, name or ("ord_%d" % ordn)))
                return True
            ida_nalt.enum_import_names(i, cb)
    log("imports dumped")

    # 4) Hex-Rays 反编译关键函数
    hx = ida_hexrays.init_hexrays_plugin()
    log("hexrays available: %s" % hx)

    def decomp(ea):
        try:
            cf = ida_hexrays.decompile(ea)
            return str(cf) if cf else "// (decompile returned None)"
        except Exception as e:
            return "// decompile error: %s" % e

    # 目标:main / 启动方法 / 越狱检测相关 / cocos2d director 启动
    targets = []
    for ea, name in funcs:
        low = name.lower()
        if name in ("_main", "main"):
            targets.append((ea, name))
        elif "didfinishlaunching" in low:
            targets.append((ea, name))
        elif any(k in low for k in ["jailbr", "jailbroken", "cydia", "isjail",
                                     "substrate", "checkjail", "piracy", "crack"]):
            targets.append((ea, name))
        elif "applicationdidbecomeactive" in low:
            targets.append((ea, name))
        elif "rundirector" in low or "startscene" in low or "rootviewcontroller" in low:
            targets.append((ea, name))

    if hx:
        with open(os.path.join(OUT, "decomp_key.c"), "w") as f:
            for ea, name in targets[:60]:
                f.write("// ===== 0x%08x  %s =====\n" % (ea, name))
                f.write(decomp(ea))
                f.write("\n\n")
        log("decompiled targets: %d" % len(targets))

    with open(os.path.join(OUT, "DONE.txt"), "w") as f:
        f.write("OK funcs=%d classes=%d hexrays=%s targets=%d\n"
                % (len(funcs), len(classes), hx, len(targets)))
except Exception:
    log("FATAL:\n" + traceback.format_exc())

idc.qexit(0)
