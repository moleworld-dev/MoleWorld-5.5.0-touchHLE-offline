// MoleWorld 移植引擎 —— 第一块砖
// 原生 arm64:Mach-O(ARMv7 32位)加载器 + 按 vmaddr 映射 + Thumb 反汇编
// 这是后续「ARMv7 解释器 / 静态重编译器」的地基。全新自写,不依赖任何旧产物。
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <vector>
#include <mach-o/loader.h>
#include <capstone/capstone.h>

static std::vector<uint8_t> readfile(const char* p) {
    FILE* f = fopen(p, "rb");
    if (!f) { perror("open"); exit(1); }
    fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
    std::vector<uint8_t> b(n);
    if (fread(b.data(), 1, n, f) != (size_t)n) { perror("read"); exit(1); }
    fclose(f);
    return b;
}

int main(int argc, char** argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <macho> [vaddr_hex] [count] [arm|thumb]\n", argv[0]);
        return 1;
    }
    uint32_t vaddr = (argc >= 3) ? (uint32_t)strtoul(argv[2], 0, 16) : 0xb290;
    int count = (argc >= 4) ? atoi(argv[3]) : 24;
    bool thumb = !(argc >= 5 && strcmp(argv[4], "arm") == 0);

    auto buf = readfile(argv[1]);
    auto* mh = (struct mach_header*)buf.data();
    if (mh->magic != MH_MAGIC) {
        fprintf(stderr, "not 32-bit mach-o (magic=%08x)\n", mh->magic);
        return 1;
    }
    printf("[machO] cputype=%d ncmds=%u\n", mh->cputype, mh->ncmds);

    // 第一遍:求 guest 地址空间上界
    uint64_t top = 0;
    uint8_t* p = buf.data() + sizeof(struct mach_header);
    for (uint32_t i = 0; i < mh->ncmds; i++) {
        auto* lc = (struct load_command*)p;
        if (lc->cmd == LC_SEGMENT) {
            auto* sc = (struct segment_command*)p;
            if ((uint64_t)sc->vmaddr + sc->vmsize > top) top = (uint64_t)sc->vmaddr + sc->vmsize;
        }
        p += lc->cmdsize;
    }
    std::vector<uint8_t> g(top, 0);

    // 第二遍:按 vmaddr 把各段拷进 guest 内存
    p = buf.data() + sizeof(struct mach_header);
    for (uint32_t i = 0; i < mh->ncmds; i++) {
        auto* lc = (struct load_command*)p;
        if (lc->cmd == LC_SEGMENT) {
            auto* sc = (struct segment_command*)p;
            if ((uint64_t)sc->fileoff + sc->filesize <= buf.size() &&
                (uint64_t)sc->vmaddr + sc->filesize <= g.size())
                memcpy(g.data() + sc->vmaddr, buf.data() + sc->fileoff, sc->filesize);
            printf("[seg] %-14s vmaddr=%08x vmsize=%08x fileoff=%08x filesize=%08x\n",
                   sc->segname, sc->vmaddr, sc->vmsize, sc->fileoff, sc->filesize);
        }
        p += lc->cmdsize;
    }

    if (vaddr >= g.size()) { fprintf(stderr, "vaddr out of range\n"); return 1; }

    csh h;
    cs_mode mode = thumb ? CS_MODE_THUMB : CS_MODE_ARM;
    if (cs_open(CS_ARCH_ARM, mode, &h) != CS_ERR_OK) { fprintf(stderr, "cs_open fail\n"); return 1; }
    cs_insn* insn;
    size_t maxbytes = (size_t)count * 4 + 8;
    if (vaddr + maxbytes > g.size()) maxbytes = g.size() - vaddr;
    size_t n = cs_disasm(h, g.data() + vaddr, maxbytes, vaddr, count, &insn);
    printf("[disasm %s @ 0x%x]  %zu insns\n", thumb ? "THUMB" : "ARM", vaddr, n);
    for (size_t i = 0; i < n; i++)
        printf("  0x%08llx:  %-9s %s\n", (unsigned long long)insn[i].address,
               insn[i].mnemonic, insn[i].op_str);
    cs_free(insn, n);
    cs_close(&h);
    return 0;
}
