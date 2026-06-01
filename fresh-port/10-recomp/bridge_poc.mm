// MoleWorld 移植引擎 —— Brick 3b
// 验证最高风险的一环:32位 guest 的 objc_msgSend 转发到 64位原生 ObjC 框架。
// 机制:句柄表(32位 guest id ↔ 64位原生 id)+ 原生 objc_msgSend 按签名调用。
// 这里用句柄手工模拟"解释器在每个 bl objc_msgSend 处会做的事"。
#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#import <objc/message.h>
#include <cstdio>
#include <cstdint>
#include <vector>

// ===== guest 句柄表:32位句柄 <-> 64位原生对象 =====
static std::vector<id> g_handles;
static uint32_t H(id obj){ if(!obj) return 0; g_handles.push_back(obj); return (uint32_t)(0x80000000u | (uint32_t)(g_handles.size()-1)); }
static id        U(uint32_t h){ if(!h) return nil; if(h&0x80000000u){ uint32_t i=h&0x7fffffffu; if(i<g_handles.size()) return g_handles[i]; } return nil; }

// ===== 桥接入口(解释器命中 objc_msgSend 时调用)=====
static uint32_t b_class(const char* n){ return H((id)objc_getClass(n)); }
static uint32_t b_msg(uint32_t s,const char* sel){ return H(((id(*)(id,SEL))objc_msgSend)(U(s),sel_registerName(sel))); }
static uint32_t b_msg_id(uint32_t s,const char* sel,uint32_t a){ return H(((id(*)(id,SEL,id))objc_msgSend)(U(s),sel_registerName(sel),U(a))); }
static uint32_t b_msg_cstr(uint32_t s,const char* sel,const char* c){ return H(((id(*)(id,SEL,const char*))objc_msgSend)(U(s),sel_registerName(sel),c)); }
static uint32_t b_msg_ul(uint32_t s,const char* sel,unsigned long v){ return H(((id(*)(id,SEL,unsigned long))objc_msgSend)(U(s),sel_registerName(sel),v)); }
static unsigned long b_msg_retul(uint32_t s,const char* sel){ return ((unsigned long(*)(id,SEL))objc_msgSend)(U(s),sel_registerName(sel)); }
static const char*   b_msg_retcstr(uint32_t s,const char* sel){ return ((const char*(*)(id,SEL))objc_msgSend)(U(s),sel_registerName(sel)); }

int main(){
    @autoreleasepool {
        printf("=== Brick3b:guest(32位)objc_msgSend 转发到原生(64位)框架 ===\n\n");

        // 模拟 guest 代码: arr = [[NSMutableArray alloc] init]
        uint32_t cArr = b_class("NSMutableArray");
        uint32_t arr  = b_msg(b_msg(cArr,"alloc"),"init");
        printf("guest句柄 cArr=0x%08x  arr=0x%08x  ->  原生类 %s\n",
               cArr, arr, class_getName(object_getClass(U(arr))));

        // s = [NSString stringWithUTF8String:"你好 摩尔庄园"]
        uint32_t s = b_msg_cstr(b_class("NSString"),"stringWithUTF8String:","你好 摩尔庄园");
        printf("guest句柄 s=0x%08x  ->  原生 NSString *%p\n", s, (void*)U(s));

        // [arr addObject:s] 两次
        b_msg_id(arr,"addObject:",s);
        b_msg_id(arr,"addObject:",s);

        // n = [arr count]   (返回整型)
        unsigned long n = b_msg_retul(arr,"count");
        printf("[arr count] = %lu   (期望 2) %s\n", n, n==2?"✅":"❌");

        // first = [arr objectAtIndex:0]; back = [first UTF8String]  (整型入参 + C串返回)
        uint32_t first = b_msg_ul(arr,"objectAtIndex:",0);
        const char* back = b_msg_retcstr(first,"UTF8String");
        printf("[[arr objectAtIndex:0] UTF8String] = \"%s\"   %s\n",
               back, (back&&!strcmp(back,"你好 摩尔庄园"))?"✅ 字符串原样往返":"❌");

        printf("\n句柄表规模=%zu。结论:32↔64 转发机制成立——解释器在每个 objc_msgSend\n"
               "处只需(self句柄→原生id, selector→SEL, 按签名 marshaling)即可复用 iOS26 原生框架。\n",
               g_handles.size());
    }
    return 0;
}
