// SPDX-FileCopyrightText: Copyright (c) 2022 merryhime <https://mary.rs>
// SPDX-License-Identifier: MIT

#pragma once

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <new>

#if defined(_WIN32)
#    define NOMINMAX
#    include <windows.h>
#elif defined(__APPLE__)
#    include <TargetConditionals.h>
#    include <libkern/OSCacheControl.h>
#    include <pthread.h>
#    include <sys/mman.h>
#    include <unistd.h>
#else
#    include <sys/mman.h>
#endif

namespace oaknut {

class CodeBlock {
public:
    explicit CodeBlock(std::size_t size)
        : m_size(size)
    {
#if defined(_WIN32)
        m_memory = (std::uint32_t*)VirtualAlloc(nullptr, size, MEM_COMMIT, PAGE_EXECUTE_READWRITE);
#elif defined(__APPLE__)
#    if TARGET_OS_IPHONE
        // [MoleWorld iOS] iOS:纯 RWX(不带 MAP_JIT)。实测 mmap(...MAP_JIT) 在未授权 JIT 的 iOS
        // 第三方 app 上返回 MAP_FAILED(-1),dynarmic 随后往 -1 写码 → EmitPrelude 崩(code=1)。
        // 所以不能用 MAP_JIT。纯 RWX 页:写恒可行;能否执行取决于进程是否被置上 CS_DEBUGGED
        // (调试器)——这是「带调试器 JIT」在 iOS 上的标准做法(UTM 等同款)。
        m_memory = (std::uint32_t*)mmap(nullptr, size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_ANON | MAP_PRIVATE, -1, 0);
#    else
        m_memory = (std::uint32_t*)mmap(nullptr, size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_ANON | MAP_PRIVATE | MAP_JIT, -1, 0);
#    endif
#elif defined(__NetBSD__)
        m_memory = (std::uint32_t*)mmap(nullptr, size, PROT_MPROTECT(PROT_READ | PROT_WRITE | PROT_EXEC), MAP_ANON | MAP_PRIVATE, -1, 0);
#elif defined(__OpenBSD__)
        m_memory = (std::uint32_t*)mmap(nullptr, size, PROT_READ | PROT_EXEC, MAP_ANON | MAP_PRIVATE, -1, 0);
#else
        m_memory = (std::uint32_t*)mmap(nullptr, size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_ANON | MAP_PRIVATE, -1, 0);
#endif

        if (m_memory == nullptr)
            throw std::bad_alloc{};
    }

    ~CodeBlock()
    {
        if (m_memory == nullptr)
            return;

#if defined(_WIN32)
        VirtualFree((void*)m_memory, 0, MEM_RELEASE);
#else
        munmap(m_memory, m_size);
#endif
    }

    CodeBlock(const CodeBlock&) = delete;
    CodeBlock& operator=(const CodeBlock&) = delete;
    CodeBlock(CodeBlock&&) = delete;
    CodeBlock& operator=(CodeBlock&&) = delete;

    std::uint32_t* ptr() const
    {
        return m_memory;
    }

    void protect()
    {
        // [MoleWorld iOS] iOS(TARGET_OS_IPHONE)故意空操作:JIT 页恒为 RWX,不做 W^X 翻转
        // (iOS 上 pthread_jit_write_protect_np 不可用;RWX 写后执行由 CS_DEBUGGED 放行)。
#if defined(__APPLE__) && !TARGET_OS_IPHONE
        pthread_jit_write_protect_np(1);
#elif defined(__NetBSD__) || defined(__OpenBSD__)
        mprotect(m_memory, m_size, PROT_READ | PROT_EXEC);
#endif
    }

    void unprotect()
    {
        // [MoleWorld iOS] 见 protect():iOS 空操作(页恒 RWX)。
#if defined(__APPLE__) && !TARGET_OS_IPHONE
        pthread_jit_write_protect_np(0);
#elif defined(__NetBSD__) || defined(__OpenBSD__)
        mprotect(m_memory, m_size, PROT_READ | PROT_WRITE);
#endif
    }

    void invalidate(std::uint32_t* mem, std::size_t size)
    {
#if defined(__APPLE__)
        sys_icache_invalidate(mem, size);
#elif defined(_WIN32)
        FlushInstructionCache(GetCurrentProcess(), mem, size);
#else
        static std::size_t icache_line_size = 0x10000, dcache_line_size = 0x10000;

        std::uint64_t ctr;
        __asm__ volatile("mrs %0, ctr_el0"
                         : "=r"(ctr));

        const std::size_t isize = icache_line_size = std::min<std::size_t>(icache_line_size, 4 << ((ctr >> 0) & 0xf));
        const std::size_t dsize = dcache_line_size = std::min<std::size_t>(dcache_line_size, 4 << ((ctr >> 16) & 0xf));

        const std::uintptr_t end = (std::uintptr_t)mem + size;

        for (std::uintptr_t addr = ((std::uintptr_t)mem) & ~(dsize - 1); addr < end; addr += dsize) {
            __asm__ volatile("dc cvau, %0"
                             :
                             : "r"(addr)
                             : "memory");
        }
        __asm__ volatile("dsb ish\n"
                         :
                         :
                         : "memory");

        for (std::uintptr_t addr = ((std::uintptr_t)mem) & ~(isize - 1); addr < end; addr += isize) {
            __asm__ volatile("ic ivau, %0"
                             :
                             : "r"(addr)
                             : "memory");
        }
        __asm__ volatile("dsb ish\nisb\n"
                         :
                         :
                         : "memory");
#endif
    }

    void invalidate_all()
    {
        invalidate(m_memory, m_size);
    }

protected:
    std::uint32_t* m_memory;
    std::size_t m_size = 0;
};

}  // namespace oaknut
