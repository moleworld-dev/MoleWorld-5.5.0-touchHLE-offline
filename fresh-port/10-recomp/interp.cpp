// MoleWorld 移植引擎 —— Brick 2.5
// ARMv7(Thumb)解释器 + 桩表识别:把游戏发送的 ObjC 消息流"念"出来。
// 全新自写。这是 Brick3(框架桥)的门槛:先能看见 objc_msgSend 的 self+selector。
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <vector>
#include <algorithm>
#include <string>
#include <map>
#include <set>
#include <mach-o/loader.h>
#include <capstone/capstone.h>
#import <Foundation/Foundation.h>
#include <objc/runtime.h>
#include <objc/message.h>
#include <ffi/ffi.h>
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl.h>
#include <OpenGL/glext.h>
// 真·框架转发(Catalyst 宿主:UIKit/Foundation 在场)+ libffi 通用参数编组
// 注:本文件含 ObjC 代码,务必按 ObjC++ 编(-x objective-c++ + -framework Foundation)

static std::vector<uint8_t> G;                 // guest 内存(按 vmaddr)
static std::map<uint32_t,std::string> STUB;    // 桩地址 -> 导入名
static std::map<uint32_t,std::string> CLSREF;  // classref 槽地址 -> 框架类名
static std::vector<std::string> FW;            // 句柄 idx -> 类名/描述(与 NH 并行)
static std::vector<id> NH;                       // 句柄 idx -> 真·原生 id
static uint32_t load_macho(const char*);
static void     load_stub_map(const char*);
static void     load_classref_map(const char*);
static std::map<std::string,uint32_t> MPLUS, MMINUS;   // "类\t选择子" -> IMP(类方法+/实例方法-)
static std::map<uint32_t,std::string> CLSADDR;          // class_t 地址 -> 类名
static std::map<uint32_t,std::string> DBIND;            // 外部数据/函数符号槽地址 -> 符号名
// 非必要 SDK(广告/统计/账号/设备ID)跳过集:limp 启动到渲染路径,不影响游戏本体/引擎
static std::set<std::string> SKIPCLS = {
  "NewRelicAgent","NewRelicAgentInternal","NRNonARCMethods","NRMAHarvestController","NRMAMethodSwizzling",
  "IMAdTracker","IMNiceParamsMgr","IMAILogger","immobView","immobJavaScriptBridge",
  "TalkingDataGA","TDGAUtility","TDGADataWork","TDGAKeyChain","TDGAAccount","TDGAVirtualCurrency",
  "UIDeviceIdentifierAddition","TMA_SSKeychain","SSKeychain","TMADataManager","TMAReachability",
  "AdWallsManager","DianruManager","DomobManager","IMMobManager","TapjoyManager","ADCPowerWallManager",
  "PunchBoxAd","DianRuAdWall","DMOfferWallViewController","Tapjoy","TapjoyConnect","Flurry","iRate" };
static void     load_method_map(const char*);
static void     load_class_addr_map(const char*);
static void     load_data_bind_map(const char*);
static uint32_t mkH(id o, const std::string& nm){ NH.push_back(o); FW.push_back(nm); return 0x80000000u|(uint32_t)(NH.size()-1); } // 句柄=真id+名
static id rsH(uint32_t h){ if(!(h&0x80000000u))return (id)0; uint32_t i=h&0x7fffffffu; return i<NH.size()?NH[i]:(id)0; }

struct CPU { uint32_t r[16]; bool N,Z,C,V; uint32_t s[32]; bool fN,fZ,fC,fV; };  // s[]=VFP单精度位
static CPU cpu;
static inline float    as_f(uint32_t b){ float f; memcpy(&f,&b,4); return f; }
static inline uint32_t as_b(float f){ uint32_t b; memcpy(&b,&f,4); return b; }
static inline uint32_t rd32(uint32_t a){ return ((uint64_t)a+4<=G.size())?*(uint32_t*)(G.data()+a):0; }      // uint64 防回绕
static inline void     wr32(uint32_t a,uint32_t v){ if((uint64_t)a+4<=G.size())*(uint32_t*)(G.data()+a)=v; }
static inline uint8_t  rd8(uint32_t a){ return (uint64_t)a<G.size()?G[a]:0; }
static inline void     wr8(uint32_t a,uint8_t v){ if((uint64_t)a<G.size())G[a]=v; }
static inline uint16_t rd16(uint32_t a){ return ((uint64_t)a+2<=G.size())?*(uint16_t*)(G.data()+a):0; }
static inline void     wr16(uint32_t a,uint16_t v){ if((uint64_t)a+2<=G.size())*(uint16_t*)(G.data()+a)=v; }
static std::string rd_cstr(uint32_t a){ std::string s; for(int i=0;i<128&&a+i<G.size();i++){ char c=(char)G[a+i]; if(!c)break; s+=c; } return s; }

static int regidx(unsigned reg){
    switch(reg){
        case ARM_REG_R0:return 0; case ARM_REG_R1:return 1; case ARM_REG_R2:return 2; case ARM_REG_R3:return 3;
        case ARM_REG_R4:return 4; case ARM_REG_R5:return 5; case ARM_REG_R6:return 6; case ARM_REG_R7:return 7;
        case ARM_REG_R8:return 8; case ARM_REG_R9:return 9; case ARM_REG_R10:return 10; case ARM_REG_R11:return 11;
        case ARM_REG_R12:return 12; case ARM_REG_SP:return 13; case ARM_REG_LR:return 14; case ARM_REG_PC:return 15;
        default:return -1;
    }
}
static int sidx(unsigned r){ return (r>=ARM_REG_S0&&r<=ARM_REG_S31)?(int)(r-ARM_REG_S0):-1; }   // 单精度 s0-s31
static int didx(unsigned r){ return (r>=ARM_REG_D0&&r<=ARM_REG_D15)?(int)(r-ARM_REG_D0):-1; }   // 双精度 d0-d15
static uint32_t opval(const cs_arm_op& o,uint32_t pcval){
    if(o.type==ARM_OP_IMM) return (uint32_t)o.imm;
    if(o.type==ARM_OP_REG){ int i=regidx(o.reg); return (i==15)?pcval:(i>=0?cpu.r[i]:0); }
    return 0;
}
static bool eval_cond(const std::string& c){
    if(c.empty()||c=="al")return true; char a=c[0],b=c.size()>1?c[1]:0;
    if(a=='e'&&b=='q')return cpu.Z; if(a=='n'&&b=='e')return !cpu.Z;
    if(a=='c'&&b=='s')return cpu.C; if(a=='c'&&b=='c')return !cpu.C;
    if(a=='m'&&b=='i')return cpu.N; if(a=='p'&&b=='l')return !cpu.N;
    if(a=='v'&&b=='s')return cpu.V; if(a=='v'&&b=='c')return !cpu.V;
    if(a=='h'&&b=='i')return cpu.C&&!cpu.Z; if(a=='l'&&b=='s')return !cpu.C||cpu.Z;
    if(a=='g'&&b=='e')return cpu.N==cpu.V; if(a=='l'&&b=='t')return cpu.N!=cpu.V;
    if(a=='g'&&b=='t')return !cpu.Z&&cpu.N==cpu.V; if(a=='l'&&b=='e')return cpu.Z||cpu.N!=cpu.V;
    return true;
}
static std::string invert_cond(const std::string& c){
    static const char* p[][2]={{"eq","ne"},{"cs","cc"},{"mi","pl"},{"vs","vc"},{"hi","ls"},{"ge","lt"},{"gt","le"}};
    for(auto&x:p){ if(c==x[0])return x[1]; if(c==x[1])return x[0]; } return c;
}
// ===== 参数编组(libffi 通用)=====
static uint32_t CFSTR_LO=0, CFSTR_HI=0;
static id guest_cfstr_to_ns(uint32_t p){            // 32位 __cfstring: isa,flags,data,len
    uint32_t dataptr=rd32(p+8), len=rd32(p+12);
    if(!dataptr||dataptr>=G.size()||len>G.size()) return (id)0;
    return [[NSString alloc] initWithBytes:(G.data()+dataptr) length:len encoding:NSUTF8StringEncoding];
}
// guest 堆 + 游戏实例分配(写 isa=class_t 地址)
static uint32_t HEAP_PTR=0, HEAP_END=0;
static uint32_t guest_malloc(uint32_t sz){ uint32_t a=(HEAP_PTR+15)&~15u; if(!HEAP_END||a+sz>HEAP_END)return 0; HEAP_PTR=a+sz; return a; }
static uint32_t guest_alloc_instance(uint32_t classaddr){       // class_ro_t.instanceSize @ +8
    uint32_t ro=rd32(classaddr+16)&~3u; uint32_t isz=rd32(ro+8); if(isz<16||isz>0x100000)isz=16;
    uint32_t o=guest_malloc(isz); if(o)wr32(o,classaddr); return o;   // 实例首字 = isa = class_t 地址
}
// guest 感知的 C 函数 shim(指针/分配类绝不能转发原生 64 位!)。a0..a3 = r0..r3
static bool c_shim(const std::string& nm, uint32_t a0,uint32_t a1,uint32_t a2,uint32_t& rOut){
    auto ing=[&](uint32_t p,uint32_t n){ return (uint64_t)p+n<=G.size(); };
    if(nm=="_malloc"){ rOut=guest_malloc(a0); return true; }
    if(nm=="_calloc"){ rOut=guest_malloc(a0*a1); return true; }                       // guest_malloc 已清零
    if(nm=="_realloc"){ uint32_t p=guest_malloc(a1); if(p&&a0&&ing(a0,a1)&&ing(p,a1)) memmove(G.data()+p,G.data()+a0,a1); rOut=p; return true; }
    if(nm=="_free"){ rOut=0; return true; }                                           // 暂泄漏,无碍
    if(nm=="_memcpy"||nm=="_memmove"){ if(ing(a0,a2)&&ing(a1,a2)) memmove(G.data()+a0,G.data()+a1,a2); rOut=a0; return true; }
    if(nm=="_memset"){ if(ing(a0,a2)) memset(G.data()+a0,(int)a1,a2); rOut=a0; return true; }
    if(nm=="_bzero"||nm=="___bzero"){ if(ing(a0,a1)) memset(G.data()+a0,0,a1); rOut=0; return true; }
    if(nm=="_strlen"){ rOut=(uint32_t)rd_cstr(a0).size(); return true; }
    if(nm=="_strcmp"){ rOut=(uint32_t)(int32_t)strcmp(rd_cstr(a0).c_str(),rd_cstr(a1).c_str()); return true; }
    if(nm=="_strcpy"){ std::string s=rd_cstr(a1); for(size_t i=0;i<=s.size();i++) wr8(a0+(uint32_t)i,i<s.size()?(uint8_t)s[i]:0); rOut=a0; return true; }
    if(nm=="_strncpy"){ std::string s=rd_cstr(a1); for(uint32_t i=0;i<a2;i++) wr8(a0+i,i<s.size()?(uint8_t)s[i]:0); rOut=a0; return true; }
    // ARC 辅助:透传/空操作
    if(nm=="_objc_retain"||nm=="_objc_retainAutorelease"||nm=="_objc_retainAutoreleaseReturnValue"||nm=="_objc_retainAutoreleasedReturnValue"||nm=="_objc_autorelease"||nm=="_objc_autoreleaseReturnValue"||nm=="_objc_retainBlock"){ rOut=a0; return true; }
    if(nm=="_objc_release"||nm=="_objc_storeStrong"){ rOut=0; return true; }
    return false;
}
// ===== GL 转发层(guest GLES1 → 宿主桌面 GL,离屏 FBO)=====
static CGLContextObj GLCTX=0; static int GLN=0;
static void gl_init(){
    CGLPixelFormatAttribute attrs[]={ kCGLPFAColorSize,(CGLPixelFormatAttribute)32, kCGLPFADepthSize,(CGLPixelFormatAttribute)16,(CGLPixelFormatAttribute)0 };
    CGLPixelFormatObj pf; GLint n; if(CGLChoosePixelFormat(attrs,&pf,&n)!=kCGLNoError||!pf) return;
    if(CGLCreateContext(pf,0,&GLCTX)!=kCGLNoError){ GLCTX=0; return; } CGLSetCurrentContext(GLCTX);
    GLuint fb,rb; glGenFramebuffersEXT(1,&fb); glBindFramebufferEXT(GL_FRAMEBUFFER_EXT,fb);
    glGenRenderbuffersEXT(1,&rb); glBindRenderbufferEXT(GL_RENDERBUFFER_EXT,rb);
    glRenderbufferStorageEXT(GL_RENDERBUFFER_EXT,GL_RGBA8,1024,768);
    glFramebufferRenderbufferEXT(GL_FRAMEBUFFER_EXT,GL_COLOR_ATTACHMENT0_EXT,GL_RENDERBUFFER_EXT,rb);
    glViewport(0,0,1024,768);
}
#define RI(k) ((GLint)cpu.r[k])
#define SF(k) (as_f(cpu.s[k]))
#define GPTR(v) ((const void*)((v)&&(uint64_t)(v)<G.size()?(G.data()+(v)):(const void*)0))
static bool gl_shim(const std::string& f){
    if(!GLCTX || f.size()<3 || f[0]!='_'||f[1]!='g'||f[2]!='l') return false;
    const char* g=f.c_str()+1; GLN++; uint32_t sp=cpu.r[13];
    if(!strcmp(g,"glViewport")) glViewport(RI(0),RI(1),RI(2),RI(3));
    else if(!strcmp(g,"glClearColor")) glClearColor(SF(0),SF(1),SF(2),SF(3));
    else if(!strcmp(g,"glClear")) glClear(RI(0));
    else if(!strcmp(g,"glClearDepthf")) glClearDepth(SF(0));
    else if(!strcmp(g,"glEnable")) glEnable(RI(0));
    else if(!strcmp(g,"glDisable")) glDisable(RI(0));
    else if(!strcmp(g,"glBlendFunc")) glBlendFunc(RI(0),RI(1));
    else if(!strcmp(g,"glDepthFunc")) glDepthFunc(RI(0));
    else if(!strcmp(g,"glHint")) glHint(RI(0),RI(1));
    else if(!strcmp(g,"glPixelStorei")) glPixelStorei(RI(0),RI(1));
    else if(!strcmp(g,"glFinish")||!strcmp(g,"glFlush")) glFinish();
    else if(!strcmp(g,"glGetError")) cpu.r[0]=glGetError();
    else if(!strcmp(g,"glScissor")) glScissor(RI(0),RI(1),RI(2),RI(3));
    else if(!strcmp(g,"glColor4f")) glColor4f(SF(0),SF(1),SF(2),SF(3));
    else if(!strcmp(g,"glColor4ub")) glColor4ub(RI(0),RI(1),RI(2),RI(3));
    else if(!strcmp(g,"glMatrixMode")) glMatrixMode(RI(0));
    else if(!strcmp(g,"glLoadIdentity")) glLoadIdentity();
    else if(!strcmp(g,"glPushMatrix")) glPushMatrix();
    else if(!strcmp(g,"glPopMatrix")) glPopMatrix();
    else if(!strcmp(g,"glOrthof")) glOrtho(SF(0),SF(1),SF(2),SF(3),SF(4),SF(5));
    else if(!strcmp(g,"glTranslatef")) glTranslatef(SF(0),SF(1),SF(2));
    else if(!strcmp(g,"glScalef")) glScalef(SF(0),SF(1),SF(2));
    else if(!strcmp(g,"glRotatef")) glRotatef(SF(0),SF(1),SF(2),SF(3));
    else if(!strcmp(g,"glGenTextures")){ int c=RI(0); std::vector<GLuint> t(c>0?c:0); if(c>0){glGenTextures(c,t.data()); for(int i=0;i<c;i++) wr32(cpu.r[1]+4*i,t[i]);} }
    else if(!strcmp(g,"glBindTexture")) glBindTexture(RI(0),RI(1));
    else if(!strcmp(g,"glDeleteTextures")){ int c=RI(0); std::vector<GLuint> t; for(int i=0;i<c;i++)t.push_back(rd32(cpu.r[1]+4*i)); if(c>0)glDeleteTextures(c,t.data()); }
    else if(!strcmp(g,"glTexParameteri")) glTexParameteri(RI(0),RI(1),RI(2));
    else if(!strcmp(g,"glTexParameterf")) glTexParameterf(RI(0),RI(1),SF(0));
    else if(!strcmp(g,"glTexImage2D")) glTexImage2D(RI(0),RI(1),RI(2),RI(3),rd32(sp),rd32(sp+4),rd32(sp+8),rd32(sp+12),GPTR(rd32(sp+16)));
    else if(!strcmp(g,"glActiveTexture")) glActiveTexture(RI(0));
    else if(!strcmp(g,"glEnableClientState")) glEnableClientState(RI(0));
    else if(!strcmp(g,"glDisableClientState")) glDisableClientState(RI(0));
    else if(!strcmp(g,"glVertexPointer")) glVertexPointer(RI(0),RI(1),RI(2),GPTR(RI(3)));
    else if(!strcmp(g,"glColorPointer")) glColorPointer(RI(0),RI(1),RI(2),GPTR(RI(3)));
    else if(!strcmp(g,"glTexCoordPointer")) glTexCoordPointer(RI(0),RI(1),RI(2),GPTR(RI(3)));
    else if(!strcmp(g,"glDrawArrays")) glDrawArrays(RI(0),RI(1),RI(2));
    else if(!strcmp(g,"glDrawElements")) glDrawElements(RI(0),RI(1),RI(2),GPTR(RI(3)));
    else if(!strcmp(g,"glGenFramebuffersOES")){ int c=RI(0); std::vector<GLuint> t(c>0?c:0); if(c>0){glGenFramebuffersEXT(c,t.data()); for(int i=0;i<c;i++) wr32(cpu.r[1]+4*i,t[i]);} }
    else if(!strcmp(g,"glBindFramebufferOES")) glBindFramebufferEXT(RI(0),RI(1));
    else if(!strcmp(g,"glGenRenderbuffersOES")){ int c=RI(0); std::vector<GLuint> t(c>0?c:0); if(c>0){glGenRenderbuffersEXT(c,t.data()); for(int i=0;i<c;i++) wr32(cpu.r[1]+4*i,t[i]);} }
    else if(!strcmp(g,"glBindRenderbufferOES")) glBindRenderbufferEXT(RI(0),RI(1));
    else if(!strcmp(g,"glRenderbufferStorageOES")) glRenderbufferStorageEXT(RI(0),RI(1),RI(2),RI(3));
    else if(!strcmp(g,"glFramebufferRenderbufferOES")) glFramebufferRenderbufferEXT(RI(0),RI(1),RI(2),RI(3));
    else if(!strcmp(g,"glCheckFramebufferStatusOES")) cpu.r[0]=glCheckFramebufferStatusEXT(RI(0));
    else { GLN--; /* 未实现的 gl* 先吞掉 */ }
    return true;
}
static const char* skip_type(const char* p, char* outc){     // 跳一个类型(+尾随偏移数字)
    while(*p=='r'||*p=='n'||*p=='N'||*p=='o'||*p=='O'||*p=='R'||*p=='V') p++;
    *outc=*p;
    if(*p=='{'||*p=='('){ int dep=0; do{ if(*p=='{'||*p=='(')dep++; else if(*p=='}'||*p==')')dep--; p++; }while(dep>0&&*p); }
    else if(*p=='^'){ p++; char d; p=skip_type(p,&d); }
    else if(*p=='b'){ p++; while(*p>='0'&&*p<='9')p++; }
    else if(*p) p++;
    while(*p>='0'&&*p<='9') p++;
    return p;
}
static ffi_type* ret_ffi(char c){
    switch(c){ case '@':case '#':case ':':case '*':case '^': return &ffi_type_pointer;
        case 'v': return &ffi_type_void; case 'f': return &ffi_type_float; case 'd': return &ffi_type_double;
        case 'i':case 'I':case 's':case 'S': return &ffi_type_sint32; case 'c':case 'C':case 'B': return &ffi_type_sint8;
        case 'l':case 'L':case 'q':case 'Q': return &ffi_type_sint64; default: return (ffi_type*)0; }
}
// 按签名编组 guest 参数(g[0]=arg0...),libffi 调原生 objc_msgSend;成功则设 cpu.r[0]/s[0] 并返回 true
static bool native_forward(id so, SEL op, const char* enc, const uint32_t* g){
    if(!enc) return false;
    char ret; const char* p=skip_type(enc,&ret);
    std::vector<char> a; while(*p){ char c; p=skip_type(p,&c); a.push_back(c); }   // a[0]=self,a[1]=_cmd,a[2..]=真参
    int nreal=(int)a.size()-2; if(nreal<0||nreal>10) return false;
    if(!ret_ffi(ret)) return false;
    id selfv=so; SEL opv=op; ffi_type* at[16]; void* av[16];
    at[0]=&ffi_type_pointer; av[0]=&selfv; at[1]=&ffi_type_pointer; av[1]=&opv;
    union Slot{ void* p; int32_t i; int64_t q; float f; } slot[10];
    for(int k=0;k<nreal;k++){ char ty=a[2+k]; uint32_t gv=g[k]; if(!ret_ffi(ty)) return false;
        if(ty=='@'||ty=='#'||ty==':'){ id o=(id)0; if(gv&0x80000000u)o=rsH(gv); else if(gv>=CFSTR_LO&&gv<CFSTR_HI)o=guest_cfstr_to_ns(gv); slot[k].p=(void*)o; at[2+k]=&ffi_type_pointer; av[2+k]=&slot[k].p; }
        else if(ty=='*'||ty=='^'){ slot[k].p=(void*)(gv<G.size()?(G.data()+gv):0); at[2+k]=&ffi_type_pointer; av[2+k]=&slot[k].p; }
        else if(ty=='f'){ slot[k].f=as_f(gv); at[2+k]=&ffi_type_float; av[2+k]=&slot[k].f; }
        else if(ty=='l'||ty=='L'||ty=='q'||ty=='Q'){ slot[k].q=(int64_t)(int32_t)gv; at[2+k]=&ffi_type_sint64; av[2+k]=&slot[k].q; }
        else { slot[k].i=(int32_t)gv; at[2+k]=&ffi_type_sint32; av[2+k]=&slot[k].i; }
    }
    ffi_cif cif; if(ffi_prep_cif(&cif,FFI_DEFAULT_ABI,2+nreal,ret_ffi(ret),at)!=FFI_OK) return false;
    union { void* p; int64_t q; float f; double d; ffi_arg raw; } r; r.q=0;
    @try { ffi_call(&cif, FFI_FN(objc_msgSend), &r, av); }      // 框架调用可能抛异常(如 setObject:nil)
    @catch (NSException* ex) { return false; }                  // 接住→返回失败→上层置 r0=0,绝不 abort 整个进程
    @catch (...) { return false; }
    if(ret=='@'||ret=='#'||ret==':'||ret=='*'||ret=='^') cpu.r[0]=mkH((id)r.p,"id");
    else if(ret=='v') cpu.r[0]=0;
    else if(ret=='f'){ cpu.s[0]=as_b(r.f); cpu.r[0]=as_b(r.f); }
    else if(ret=='d'){ cpu.s[0]=as_b((float)r.d); cpu.r[0]=as_b((float)r.d); }
    else cpu.r[0]=(uint32_t)r.q;
    return true;
}

// 沿 class_t 超类链(superclass @ +4)解析实例方法 IMP;0=未找到(走框架继承兜底)
static uint32_t resolve_imp(uint32_t classaddr, const std::string& sel){
    uint32_t cur=classaddr; int g=0;
    while(cur && CLSADDR.count(cur) && g++<24){
        auto it=MMINUS.find(CLSADDR[cur]+"\t"+sel); if(it!=MMINUS.end()) return it->second;
        cur = rd32(cur+4) & ~1u;     // 超类
    }
    return 0;
}
static csh GH=0; static bool VERBOSE=false;
void interp_boot(const char* bin,const char* stub,const char* classref,const char* method,const char* classaddr,const char* databind){
    VERBOSE = (getenv("MOLE_VERBOSE")!=nullptr);
    load_macho(bin);
    if(stub) load_stub_map(stub);
    if(classref){ load_classref_map(classref); for(auto& kv:CLSREF){ id c=(id)objc_getClass(kv.second.c_str()); wr32(kv.first, mkH(c,kv.second)); } }
    if(method) load_method_map(method);
    if(classaddr) load_class_addr_map(classaddr);
    if(databind){ load_data_bind_map(databind);                       // dyld 式:绑定外部数据/函数符号槽
        std::map<std::string,uint32_t> name2stub; for(auto&kv:STUB) name2stub[kv.second]=kv.first;
        int fn=0,dt=0; for(auto& kv:DBIND){ auto fi=name2stub.find(kv.second);
            if(fi!=name2stub.end()){ wr32(kv.first, fi->second); fn++; }                                  // 函数符号→桩(blx 经它仍被拦截)
            else if(kv.second.find("CGAffineTransformIdentity")!=std::string::npos){ uint32_t b=guest_malloc(24); float idm[6]={1,0,0,1,0,0}; for(int i=0;i<6;i++) wr32(b+4*i,as_b(idm[i])); wr32(kv.first,b); dt++; }
            else { wr32(kv.first, guest_malloc(64)); dt++; } }                                            // 其他外部数据→零缓冲(防读0脱轨)
        fprintf(stderr,"[boot] 外部符号绑定:函数%d 数据%d\n",fn,dt);
    }
    gl_init();
    cs_open(CS_ARCH_ARM,CS_MODE_THUMB,&GH); cs_option(GH,CS_OPT_DETAIL,CS_OPT_ON);
    memset(&cpu,0,sizeof(cpu));
    cpu.r[13]=(uint32_t)G.size()-0x10000; cpu.r[14]=0xFFFFFFFF; cpu.r[0]=0x11110000;
    fprintf(stderr,"[boot] 桩=%zu classref=%zu IMP=%zu 类=%zu GL=%s\n",STUB.size(),CLSREF.size(),MPLUS.size()+MMINUS.size(),CLSADDR.size(),GLCTX?"OK":"无");
}
void interp_run(uint32_t start,int steps){
    uint32_t pc=start; int msgcount=0, fwd=0, gameobj=0; std::vector<std::string> itq; uint32_t minsp=cpu.r[13];
    for(int s=0;s<steps;s++){
        if(cpu.r[13]<minsp) minsp=cpu.r[13];
        if(pc>=G.size()){ printf("[ret] pc=0x%x 越界(疑似返回到调用者/哨兵),停止\n",pc); break; }
        cs_insn* in; size_t n=cs_disasm(GH,G.data()+pc,8,pc,1,&in);
        if(!n){ printf("  0x%08x: <decode fail>\n",pc); break; }
        cs_arm* d=&in->detail->arm;
        uint32_t cur=pc, nxt=pc+in->size, pcval=cur+4, branch=0xFFFFFFFF; std::string mn=in->mnemonic, note;
        if(in->id==ARM_INS_IT){   // IT 条件块:门控后续 1~4 条
            std::string base=in->op_str, mask=mn.size()>2?mn.substr(2):""; itq.clear(); itq.push_back(base);
            for(char m:mask) itq.push_back(m=='t'?base:invert_cond(base));
            printf("  0x%08x:  %-4s %-6s | IT块→门控后续 %zu 条\n",cur,in->mnemonic,in->op_str,itq.size());
            pc=nxt; cs_free(in,n); continue;
        }
        if(!itq.empty()){ std::string c=itq.front(); itq.erase(itq.begin());
            if(!eval_cond(c)){ printf("  0x%08x:  %-8s %-16s | (IT门控 %s 假→跳过)\n",cur,in->mnemonic,in->op_str,c.c_str()); pc=nxt; cs_free(in,n); continue; } }
        switch(in->id){
        case ARM_INS_PUSH:{ std::vector<int> rs; for(int i=0;i<d->op_count;i++) if(d->operands[i].type==ARM_OP_REG) rs.push_back(regidx(d->operands[i].reg));
            std::sort(rs.begin(),rs.end()); cpu.r[13]-=4*rs.size(); for(size_t k=0;k<rs.size();k++) wr32(cpu.r[13]+4*k,cpu.r[rs[k]]);
            char b[40]; snprintf(b,40,"SP=0x%x",cpu.r[13]); note=b; } break;
        case ARM_INS_POP:{ std::vector<int> rs; for(int i=0;i<d->op_count;i++) if(d->operands[i].type==ARM_OP_REG) rs.push_back(regidx(d->operands[i].reg));
            std::sort(rs.begin(),rs.end()); for(size_t k=0;k<rs.size();k++){ int ri=rs[k]; uint32_t v=rd32(cpu.r[13]+4*k); if(ri==15)branch=v&~1u; else cpu.r[ri]=v; }
            cpu.r[13]+=4*rs.size(); note=(branch!=0xFFFFFFFF)?"RET":"pop"; } break;
        case ARM_INS_MOV: case ARM_INS_MOVS:{ int rd=regidx(d->operands[0].reg); uint32_t v=opval(d->operands[1],pcval); if(rd>=0)cpu.r[rd]=v; char b[40]; snprintf(b,40,"r%d=0x%x",rd,v); note=b; } break;
        case ARM_INS_MOVW:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=(uint32_t)d->operands[1].imm&0xffff; } break;
        case ARM_INS_MOVT:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=(cpu.r[rd]&0xffff)|(((uint32_t)d->operands[1].imm&0xffff)<<16); char b[40]; snprintf(b,40,"r%d=0x%x",rd,cpu.r[rd]); note=b; } break;
        case ARM_INS_ADD:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=(rd==15?pcval:cpu.r[rd]);b2=opval(d->operands[1],pcval);} cpu.r[rd]=a+b2; char b[40]; snprintf(b,40,"r%d=0x%x",rd,cpu.r[rd]); note=b; } break;
        case ARM_INS_SUB:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a-b2; char b[40]; snprintf(b,40,"r%d=0x%x",rd,cpu.r[rd]); note=b; } break;
        case ARM_INS_BIC:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a&~b2; } break;
        case ARM_INS_CMP:{ uint32_t a=opval(d->operands[0],pcval),b2=opval(d->operands[1],pcval),r=a-b2; cpu.Z=(r==0);cpu.N=(r>>31)&1;cpu.C=(a>=b2);cpu.V=(((a^b2)&(a^r))>>31)&1; char b[36]; snprintf(b,36,"Z=%d",cpu.Z); note=b; } break;
        case ARM_INS_LDR:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; uint32_t addr;
            if(m.type==ARM_OP_MEM){ int bi=regidx(m.mem.base); addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; } else addr=opval(m,pcval);
            cpu.r[rt]=rd32(addr); char b[64]; snprintf(b,64,"r%d=[0x%x]=0x%x",rt,addr,cpu.r[rt]); note=b; } break;
        case ARM_INS_STR:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; wr32(addr,cpu.r[rt]); } break;
        case ARM_INS_STRB:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; wr8(addr,cpu.r[rt]&0xff); char b[48]; snprintf(b,48,"[0x%x]=0x%02x '%c'",addr,cpu.r[rt]&0xff,(cpu.r[rt]&0xff)>=32&&(cpu.r[rt]&0xff)<127?cpu.r[rt]&0xff:'.'); note=b; } break;
        case ARM_INS_STRD:{ int rt=regidx(d->operands[0].reg),rt2=regidx(d->operands[1].reg); const cs_arm_op&m=d->operands[2]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; wr32(addr,cpu.r[rt]); wr32(addr+4,cpu.r[rt2]); } break;
        case ARM_INS_LDRD:{ int rt=regidx(d->operands[0].reg),rt2=regidx(d->operands[1].reg); const cs_arm_op&m=d->operands[2]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; cpu.r[rt]=rd32(addr); cpu.r[rt2]=rd32(addr+4); } break;
        case ARM_INS_LDRB:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; cpu.r[rt]=rd8(addr); } break;
        case ARM_INS_STRH:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; wr16(addr,cpu.r[rt]&0xffff); } break;
        case ARM_INS_LDRH:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; cpu.r[rt]=rd16(addr); } break;
        case ARM_INS_ORR:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a|b2; } break;
        case ARM_INS_AND:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a&b2; } break;
        case ARM_INS_EOR:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a^b2; } break;
        case ARM_INS_MVN:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=~opval(d->operands[1],pcval); } break;
        case ARM_INS_LSL:{ int rd=regidx(d->operands[0].reg); uint32_t a,sh; if(d->op_count>=3){a=opval(d->operands[1],pcval);sh=opval(d->operands[2],pcval)&31;}else{a=cpu.r[rd];sh=opval(d->operands[1],pcval)&31;} cpu.r[rd]=a<<sh; } break;
        case ARM_INS_LSR:{ int rd=regidx(d->operands[0].reg); uint32_t a,sh; if(d->op_count>=3){a=opval(d->operands[1],pcval);sh=opval(d->operands[2],pcval)&31;}else{a=cpu.r[rd];sh=opval(d->operands[1],pcval)&31;} cpu.r[rd]=a>>sh; } break;
        case ARM_INS_MUL:{ int rd=regidx(d->operands[0].reg); uint32_t a=opval(d->operands[d->op_count>=3?1:0],pcval),b2=opval(d->operands[d->op_count>=3?2:1],pcval); cpu.r[rd]=a*b2; } break;
        case ARM_INS_CBZ:{ uint32_t v=cpu.r[regidx(d->operands[0].reg)],tgt=(uint32_t)d->operands[1].imm; if(v==0)branch=tgt; note=v==0?"CBZ→跳":"CBZ不跳"; } break;
        case ARM_INS_CBNZ:{ uint32_t v=cpu.r[regidx(d->operands[0].reg)],tgt=(uint32_t)d->operands[1].imm; if(v!=0)branch=tgt; note=v!=0?"CBNZ→跳":"CBNZ不跳"; } break;
        case ARM_INS_TST:{ uint32_t r=opval(d->operands[0],pcval)&opval(d->operands[1],pcval); cpu.Z=(r==0);cpu.N=(r>>31)&1; } break;
        case ARM_INS_TEQ:{ uint32_t r=opval(d->operands[0],pcval)^opval(d->operands[1],pcval); cpu.Z=(r==0);cpu.N=(r>>31)&1; } break;
        case ARM_INS_CMN:{ uint32_t a=opval(d->operands[0],pcval),b2=opval(d->operands[1],pcval),r=a+b2; cpu.Z=(r==0);cpu.N=(r>>31)&1;cpu.C=(r<a); } break;
        case ARM_INS_RSB:{ int rd=regidx(d->operands[0].reg); uint32_t a=opval(d->operands[1],pcval),b2=d->op_count>=3?opval(d->operands[2],pcval):0; cpu.r[rd]=b2-a; } break;
        case ARM_INS_ADC:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a+b2+(cpu.C?1:0); } break;
        case ARM_INS_SBC:{ int rd=regidx(d->operands[0].reg); uint32_t a,b2; if(d->op_count>=3){a=opval(d->operands[1],pcval);b2=opval(d->operands[2],pcval);}else{a=cpu.r[rd];b2=opval(d->operands[1],pcval);} cpu.r[rd]=a-b2-(cpu.C?0:1); } break;
        case ARM_INS_ASR:{ int rd=regidx(d->operands[0].reg); int32_t a; uint32_t sh; if(d->op_count>=3){a=(int32_t)opval(d->operands[1],pcval);sh=opval(d->operands[2],pcval)&31;}else{a=(int32_t)cpu.r[rd];sh=opval(d->operands[1],pcval)&31;} cpu.r[rd]=(uint32_t)(a>>sh); } break;
        case ARM_INS_ROR:{ int rd=regidx(d->operands[0].reg); uint32_t a,sh; if(d->op_count>=3){a=opval(d->operands[1],pcval);sh=opval(d->operands[2],pcval)&31;}else{a=cpu.r[rd];sh=opval(d->operands[1],pcval)&31;} cpu.r[rd]=sh?((a>>sh)|(a<<(32-sh))):a; } break;
        case ARM_INS_UXTB:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=opval(d->operands[1],pcval)&0xff; } break;
        case ARM_INS_UXTH:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=opval(d->operands[1],pcval)&0xffff; } break;
        case ARM_INS_SXTB:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=(uint32_t)(int32_t)(int8_t)(opval(d->operands[1],pcval)&0xff); } break;
        case ARM_INS_SXTH:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=(uint32_t)(int32_t)(int16_t)(opval(d->operands[1],pcval)&0xffff); } break;
        case ARM_INS_CLZ:{ int rd=regidx(d->operands[0].reg); uint32_t v=opval(d->operands[1],pcval); cpu.r[rd]=v?__builtin_clz(v):32; } break;
        case ARM_INS_MLA:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=opval(d->operands[1],pcval)*opval(d->operands[2],pcval)+opval(d->operands[3],pcval); } break;
        case ARM_INS_MLS:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=opval(d->operands[3],pcval)-opval(d->operands[1],pcval)*opval(d->operands[2],pcval); } break;
        case ARM_INS_ADR:{ int rd=regidx(d->operands[0].reg); cpu.r[rd]=(pcval&~3u)+(uint32_t)d->operands[1].imm; } break;
        case ARM_INS_LDRSB:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=cpu.r[bi]+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; cpu.r[rt]=(uint32_t)(int32_t)(int8_t)rd8(addr); } break;
        case ARM_INS_LDRSH:{ int rt=regidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base); uint32_t addr=cpu.r[bi]+m.mem.disp; if(m.mem.index!=ARM_REG_INVALID)addr+=cpu.r[regidx(m.mem.index)]; cpu.r[rt]=(uint32_t)(int32_t)(int16_t)rd16(addr); } break;
        case ARM_INS_TBB:{ const cs_arm_op&m=d->operands[0]; int bi=regidx(m.mem.base); uint32_t base=(bi==15)?cur+4:cpu.r[bi]; uint32_t idx=m.mem.index!=ARM_REG_INVALID?cpu.r[regidx(m.mem.index)]:0; branch=(cur+4)+2*rd8(base+idx); note="TBB跳表"; } break;
        case ARM_INS_TBH:{ const cs_arm_op&m=d->operands[0]; int bi=regidx(m.mem.base); uint32_t base=(bi==15)?cur+4:cpu.r[bi]; uint32_t idx=m.mem.index!=ARM_REG_INVALID?cpu.r[regidx(m.mem.index)]:0; branch=(cur+4)+2*rd16(base+2*idx); note="TBH跳表"; } break;
        case ARM_INS_BX:{ uint32_t t=opval(d->operands[0],pcval); branch=t&~1u; note="BX"; } break;
        case ARM_INS_STM: case ARM_INS_STMIB: case ARM_INS_STMDA: case ARM_INS_STMDB:{
            int bi=regidx(d->operands[0].reg); std::vector<int> rs; for(int i=1;i<d->op_count;i++) if(d->operands[i].type==ARM_OP_REG) rs.push_back(regidx(d->operands[i].reg));
            std::sort(rs.begin(),rs.end()); int cnt=(int)rs.size(); bool db=(in->id==ARM_INS_STMDB);
            uint32_t addr=db?cpu.r[bi]-4*cnt:cpu.r[bi]; for(int k=0;k<cnt;k++) wr32(addr+4*k,cpu.r[rs[k]]);
            if(d->writeback) cpu.r[bi]= db?cpu.r[bi]-4*cnt:cpu.r[bi]+4*cnt; } break;
        case ARM_INS_LDM: case ARM_INS_LDMIB: case ARM_INS_LDMDA: case ARM_INS_LDMDB:{
            int bi=regidx(d->operands[0].reg); std::vector<int> rs; for(int i=1;i<d->op_count;i++) if(d->operands[i].type==ARM_OP_REG) rs.push_back(regidx(d->operands[i].reg));
            std::sort(rs.begin(),rs.end()); int cnt=(int)rs.size(); bool db=(in->id==ARM_INS_LDMDB);
            uint32_t addr=db?cpu.r[bi]-4*cnt:cpu.r[bi]; for(int k=0;k<cnt;k++){ int ri=rs[k]; uint32_t v=rd32(addr+4*k); if(ri==15)branch=v&~1u; else cpu.r[ri]=v; }
            if(d->writeback) cpu.r[bi]= db?cpu.r[bi]-4*cnt:cpu.r[bi]+4*cnt; } break;
        // NEON/VFP:本解释器暂不建模浮点寄存器,序言里的 vst1/vpush 对整型栈帧无影响,跳过
        case ARM_INS_VST1: case ARM_INS_VLD1: case ARM_INS_VPUSH: case ARM_INS_VPOP: case ARM_INS_VLDMIA: case ARM_INS_VSTMIA:
        case ARM_INS_VORR: case ARM_INS_VEOR: case ARM_INS_VAND: case ARM_INS_VBIC: case ARM_INS_VDUP: case ARM_INS_VMVN:
        case ARM_INS_VTBL: case ARM_INS_VTBX: case ARM_INS_VZIP: case ARM_INS_VUZP: case ARM_INS_VTRN: case ARM_INS_VSWP:
        case ARM_INS_VREV16: case ARM_INS_VREV32: case ARM_INS_VREV64: case ARM_INS_VEXT: case ARM_INS_VBSL: case ARM_INS_VBIT: case ARM_INS_VBIF:
        case ARM_INS_VST2: case ARM_INS_VLD2: case ARM_INS_VST3: case ARM_INS_VLD3: case ARM_INS_VST4: case ARM_INS_VLD4:
            note="(neon skip)"; break;
        case ARM_INS_VLDR:{ int si=sidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base);
            uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(si>=0)cpu.s[si]=rd32(addr);
            char b[48]; if(si>=0)snprintf(b,48,"s%d=%g",si,as_f(cpu.s[si])); note=b; } break;
        case ARM_INS_VSTR:{ int si=sidx(d->operands[0].reg); const cs_arm_op&m=d->operands[1]; int bi=regidx(m.mem.base);
            uint32_t addr=((bi==15)?(pcval&~3u):cpu.r[bi])+m.mem.disp; if(si>=0)wr32(addr,cpu.s[si]); } break;
        case ARM_INS_VMOV:{ int o0s=sidx(d->operands[0].reg), o0d=didx(d->operands[0].reg);
            if(o0d>=0 && d->op_count>=3){ cpu.s[2*o0d]=opval(d->operands[1],pcval); cpu.s[2*o0d+1]=opval(d->operands[2],pcval); }      // vmov dN,rL,rH
            else if(o0s>=0 && d->operands[1].type==ARM_OP_REG && sidx(d->operands[1].reg)>=0) cpu.s[o0s]=cpu.s[sidx(d->operands[1].reg)]; // vmov sN,sM
            else if(o0s>=0 && d->operands[1].type==ARM_OP_REG) cpu.s[o0s]=cpu.r[regidx(d->operands[1].reg)];                              // vmov sN,rM
            else if(o0s<0 && o0d<0 && sidx(d->operands[1].reg)>=0) cpu.r[regidx(d->operands[0].reg)]=cpu.s[sidx(d->operands[1].reg)]; }    // vmov rN,sM
            break;
        case ARM_INS_VCMP: case ARM_INS_VCMPE:{ int ai=sidx(d->operands[0].reg); float fa=ai>=0?as_f(cpu.s[ai]):0,fb=0;
            if(d->operands[1].type==ARM_OP_REG){ int bi=sidx(d->operands[1].reg); fb=bi>=0?as_f(cpu.s[bi]):0; }
            if(fa==fb){cpu.fZ=1;cpu.fC=1;cpu.fN=0;cpu.fV=0;} else if(fa<fb){cpu.fN=1;cpu.fC=0;cpu.fZ=0;cpu.fV=0;} else {cpu.fN=0;cpu.fC=1;cpu.fZ=0;cpu.fV=0;}
            char b[48]; snprintf(b,48,"%g vs %g",fa,fb); note=b; } break;
        case ARM_INS_VMRS: case ARM_INS_FMSTAT:{ cpu.N=cpu.fN;cpu.Z=cpu.fZ;cpu.C=cpu.fC;cpu.V=cpu.fV; note="fpscr→apsr"; } break;
        case ARM_INS_VADD:{ int rd=sidx(d->operands[0].reg); if(rd>=0)cpu.s[rd]=as_b(as_f(cpu.s[sidx(d->operands[1].reg)])+as_f(cpu.s[sidx(d->operands[2].reg)])); } break;
        case ARM_INS_VSUB:{ int rd=sidx(d->operands[0].reg); if(rd>=0)cpu.s[rd]=as_b(as_f(cpu.s[sidx(d->operands[1].reg)])-as_f(cpu.s[sidx(d->operands[2].reg)])); } break;
        case ARM_INS_VMUL:{ int rd=sidx(d->operands[0].reg); if(rd>=0)cpu.s[rd]=as_b(as_f(cpu.s[sidx(d->operands[1].reg)])*as_f(cpu.s[sidx(d->operands[2].reg)])); } break;
        case ARM_INS_VDIV:{ int rd=sidx(d->operands[0].reg); if(rd>=0){ float dv=as_f(cpu.s[sidx(d->operands[2].reg)]); cpu.s[rd]=as_b(dv!=0?as_f(cpu.s[sidx(d->operands[1].reg)])/dv:0);} } break;
        case ARM_INS_VCVT:{ int rd=sidx(d->operands[0].reg); if(rd>=0)cpu.s[rd]=0; note="(vcvt 占位)"; } break;
        case ARM_INS_B:{ uint32_t tgt=(uint32_t)d->operands[0].imm; bool take=true;
            if(mn!="b"&&mn!="b.w"){ char c0=mn.size()>1?mn[1]:0,c1=mn.size()>2?mn[2]:0;
                if(c0=='e'&&c1=='q')take=cpu.Z; else if(c0=='n'&&c1=='e')take=!cpu.Z; else if(c0=='c'&&c1=='s')take=cpu.C; else if(c0=='c'&&c1=='c')take=!cpu.C;
                else if(c0=='m'&&c1=='i')take=cpu.N; else if(c0=='p'&&c1=='l')take=!cpu.N; else if(c0=='g'&&c1=='e')take=(cpu.N==cpu.V); else if(c0=='l'&&c1=='t')take=(cpu.N!=cpu.V);
                else if(c0=='g'&&c1=='t')take=(!cpu.Z&&cpu.N==cpu.V); else if(c0=='l'&&c1=='e')take=(cpu.Z||cpu.N!=cpu.V); }
            note=take?"TAKEN":"skip"; if(take)branch=tgt; } break;
        case ARM_INS_BL: case ARM_INS_BLX:{
            uint32_t tgt=d->operands[0].type==ARM_OP_IMM?(uint32_t)d->operands[0].imm:opval(d->operands[0],pcval);
            cpu.r[14]=nxt|1;
            auto it=STUB.find(tgt);
            if(it!=STUB.end()){
                const std::string& nm=it->second;
                if(nm=="_objc_msgSend"){ msgcount++;
                    uint32_t selfh=cpu.r[0]; std::string sel=rd_cstr(cpu.r[1]);
                    auto ci=CLSADDR.find(selfh);                 // self 是游戏类(class_t 地址)→ 类方法派发
                    if(ci!=CLSADDR.end()){
                        // limp 策略:非必要 SDK(广告/统计/账号/设备ID,含死循环/阻塞/断言)→nil 放行到渲染路径
                        if(SKIPCLS.count(ci->second)){ cpu.r[0]=0; cs_free(in,n); pc=nxt; continue; }
                        if(sel=="alloc"||sel=="allocWithZone:"||sel=="new"){ uint32_t o=guest_alloc_instance(selfh); cpu.r[0]=o;
                            printf("  0x%08x:  bl objc_msgSend | ✉#%d ◆+[%s %s]→分配游戏实例 0x%x\n",cur,msgcount,ci->second.c_str(),sel.c_str(),o);
                        } else { auto mi=MPLUS.find(ci->second+"\t"+sel);
                            if(mi!=MPLUS.end()){ gameobj++;
                                printf("  0x%08x:  bl objc_msgSend | ✉#%d ◆跟进 +[%s %s]@0x%x\n",cur,msgcount,ci->second.c_str(),sel.c_str(),mi->second);
                                pc=mi->second; cs_free(in,n); continue; }
                            if(sel=="class"||sel=="self"||sel=="superclass") cpu.r[0]=selfh;
                            else if(sel=="isEqual:") cpu.r[0]=(selfh==cpu.r[2])?1:0;                 // 指针相等(类对象比较靠它)
                            else if(sel=="isKindOfClass:"||sel=="isMemberOfClass:"||sel=="respondsToSelector:"||sel=="instancesRespondToSelector:"||sel=="conformsToProtocol:") cpu.r[0]=1;
                            else cpu.r[0]=0;
                            printf("  0x%08x:  bl objc_msgSend | ✉#%d +[%s %s]继承→0x%x\n",cur,msgcount,ci->second.c_str(),sel.c_str(),cpu.r[0]); }
                    } else if(!(selfh&0x80000000u) && selfh && (selfh&3)==0 && selfh<G.size() && CLSADDR.count(rd32(selfh))){  // self 是游戏实例(isa∈类表)→ 实例方法
                        std::string nm=CLSADDR[rd32(selfh)];
                        if(SKIPCLS.count(nm)){ cpu.r[0]=0; cs_free(in,n); pc=nxt; continue; }   // 非必要SDK实例方法→nil
                        uint32_t imp=resolve_imp(rd32(selfh), sel);                              // 沿超类链找(继承的方法)
                        if(imp){ gameobj++;
                            printf("  0x%08x:  bl objc_msgSend | ✉#%d ◆跟进 -[%s %s]@0x%x(实例0x%x)\n",cur,msgcount,nm.c_str(),sel.c_str(),imp,selfh);
                            pc=imp; cs_free(in,n); continue; }
                        if(sel=="init"||sel.rfind("init",0)==0||sel=="retain"||sel=="self"||sel=="autorelease") cpu.r[0]=selfh;   // 继承 NSObject:返回自身
                        else if(sel=="class") cpu.r[0]=rd32(selfh);                                                              // 实例的 class = isa
                        else if(sel=="isEqual:") cpu.r[0]=(selfh==cpu.r[2])?1:0;
                        else if(sel=="isKindOfClass:"||sel=="isMemberOfClass:"||sel=="respondsToSelector:"||sel=="conformsToProtocol:") cpu.r[0]=1;
                        else if(sel=="retainCount"||sel=="hash") cpu.r[0]=1; else cpu.r[0]=0;
                        printf("  0x%08x:  bl objc_msgSend | ✉#%d -[%s %s]继承NSObject→0x%x\n",cur,msgcount,nm.c_str(),sel.c_str(),cpu.r[0]);
                    } else if((selfh&0x80000000u) && (selfh&0x7fffffffu)<NH.size()){
                        std::string who=FW[selfh&0x7fffffffu]; id so=rsH(selfh); SEL op=sel_registerName(sel.c_str()); fwd++;
                        Method m = so? class_getInstanceMethod(object_getClass(so),op):(Method)0;   // 含继承;nil=原生不响应(疑游戏category)
                        uint32_t sp0=cpu.r[13]; uint32_t g[10]={cpu.r[2],cpu.r[3],rd32(sp0),rd32(sp0+4),rd32(sp0+8),rd32(sp0+12),rd32(sp0+16),0,0,0};
                        if(m && native_forward(so,op,method_getTypeEncoding(m),g)){
                            std::string val; id rr=rsH(cpu.r[0]);
                            if(rr && class_respondsToSelector(object_getClass(rr),sel_registerName("UTF8String"))){ const char* u=((const char*(*)(id,SEL))objc_msgSend)(rr,sel_registerName("UTF8String")); if(u){val="=\"";val+=u;val+="\"";} }
                            printf("  0x%08x:  bl objc_msgSend | ✉#%d ➜真转发 [%s %s] r0=0x%x%s\n",cur,msgcount,who.c_str(),sel.c_str(),cpu.r[0],val.c_str());
                        } else { printf("  0x%08x:  bl objc_msgSend | ✉#%d ➜%s [%s %s]→nil\n",cur,msgcount, m?"复杂签名(结构体等)放弃":"原生不响应(疑游戏category)", who.c_str(),sel.c_str()); cpu.r[0]=0; }
                    } else { gameobj++;
                        printf("  0x%08x:  bl objc_msgSend | ✉#%d ◆游戏实例 [self=0x%08x \"%s\"] (需isa,暂nil)\n", cur, msgcount, selfh, sel.c_str());
                        cpu.r[0]=0;
                    }
                } else if(nm=="_objc_msgSendSuper2"){ msgcount++;             // [super sel]:从 current_class 超类解析
                    uint32_t sp_=cpu.r[0]; std::string sel=rd_cstr(cpu.r[1]);
                    uint32_t recv=rd32(sp_), curcls=rd32(sp_+4)&~1u, startcls=rd32(curcls+4)&~1u;
                    uint32_t imp=resolve_imp(startcls, sel);
                    cpu.r[0]=recv;                                              // self=receiver(super 调用默认返回 self)
                    if(imp){ printf("  0x%08x:  bl Super2 | ✉#%d ◆[super %s]→@0x%x(self0x%x)\n",cur,msgcount,sel.c_str(),imp,recv); pc=imp; cs_free(in,n); continue; }
                    if(sel=="dealloc"||sel=="release") cpu.r[0]=0;
                    printf("  0x%08x:  bl Super2 | ✉#%d [super %s]继承→0x%x\n",cur,msgcount,sel.c_str(),cpu.r[0]);
                } else { uint32_t rv=0;
                    if(gl_shim(nm)){}                                               // GL 转发 → 真桌面 GL 上下文
                    else if(c_shim(nm,cpu.r[0],cpu.r[1],cpu.r[2],rv)){ cpu.r[0]=rv; } // guest 感知 C 函数
                    else { cpu.r[0]=0; }                                            // 其余 C 导入
                }
            } else if(tgt>=0x4000 && tgt<0x9c8000){   // 游戏体内部函数:递归跟进解释
                printf("  0x%08x:  bl sub_%x | →跟进解释执行(递归)\n", cur, tgt);
                pc=tgt; cs_free(in,n); continue;
            } else { printf("  0x%08x:  bl 0x%x | 目标越界,跳过\n", cur, tgt); cpu.r[0]=0; }
            pc=nxt; cs_free(in,n); continue;
        } break;
        case ARM_INS_NOP: note="nop"; break;
        default:
            printf("  0x%08x:  %-8s %-22s | ⚠ UNIMPL(id=%u) 下一步补\n",cur,in->mnemonic,in->op_str,in->id);
            cs_free(in,n); goto done;
        }
        if(VERBOSE) printf("  0x%08x:  %-8s %-22s | %s\n",cur,in->mnemonic,in->op_str,note.c_str());  // 逐指令打印极慢,默认关
        pc=(branch!=0xFFFFFFFF)?branch:nxt; cs_free(in,n);
    }
done:
    printf("[done] objc_msgSend 共 %d 条:➜原生转发 %d  ◆游戏对象内部解释 %d\n", msgcount, fwd, gameobj);
    printf("[GL] 转发 GL 调用 %d 次\n", GLN);
    if(GLCTX){ std::vector<uint8_t> px(1024*768*4,0); glReadPixels(0,0,1024,768,GL_RGBA,GL_UNSIGNED_BYTE,px.data());
        FILE* fp=fopen("/tmp/mole_frame.ppm","wb"); if(fp){ fprintf(fp,"P6\n1024 768\n255\n"); for(int y=767;y>=0;y--) for(int x=0;x<1024;x++) fwrite(&px[(y*1024+x)*4],1,3,fp); fclose(fp);}
        long nz=0; for(size_t i=0;i<px.size();i+=4) if(px[i]|px[i+1]|px[i+2]) nz++;
        printf("[GL] 帧已读出 → /tmp/mole_frame.ppm,非黑像素 %ld / %d\n", nz, 1024*768); }
    /* 不关 GH:供 App 反复 interp_run */
}

// 给 App 壳用:读取离屏 FBO 像素到 RGBA buffer(供显示)
void interp_read_frame(unsigned char* rgba,int w,int h){ if(GLCTX){ CGLSetCurrentContext(GLCTX); glReadPixels(0,0,w,h,GL_RGBA,GL_UNSIGNED_BYTE,rgba); } }

#ifndef APP_SHELL
int main(int argc,char**argv){
    if(argc<2){ fprintf(stderr,"usage: %s <macho> [start_hex] [steps] [stub classref method classaddr]\n",argv[0]); return 1; }
    uint32_t start=(argc>=3)?(uint32_t)strtoul(argv[2],0,16):0xef40;
    int steps=(argc>=4)?atoi(argv[3]):80;
    interp_boot(argv[1], argc>=5?argv[4]:0, argc>=6?argv[5]:0, argc>=7?argv[6]:0, argc>=8?argv[7]:0, argc>=9?argv[8]:0);
    fprintf(stderr,"[interp] start=0x%x steps=%d\n", start, steps);
    interp_run(start, steps);
    return 0;
}
#endif

static uint32_t load_macho(const char* path){
    FILE* f=fopen(path,"rb"); if(!f){perror("open");exit(1);}
    fseek(f,0,SEEK_END); long n=ftell(f); fseek(f,0,SEEK_SET);
    std::vector<uint8_t> buf(n); if(fread(buf.data(),1,n,f)!=(size_t)n){perror("read");exit(1);} fclose(f);
    auto* mh=(struct mach_header*)buf.data(); if(mh->magic!=MH_MAGIC){fprintf(stderr,"not 32bit macho\n");exit(1);}
    uint64_t top=0; uint8_t* p=buf.data()+sizeof(struct mach_header);
    for(uint32_t i=0;i<mh->ncmds;i++){auto*lc=(struct load_command*)p; if(lc->cmd==LC_SEGMENT){auto*sc=(struct segment_command*)p; if((uint64_t)sc->vmaddr+sc->vmsize>top)top=(uint64_t)sc->vmaddr+sc->vmsize;} p+=lc->cmdsize;}
    uint32_t heap=0xC00000; G.assign((size_t)top+heap+0x20000,0); HEAP_PTR=(uint32_t)top; HEAP_END=(uint32_t)top+heap;  // 游戏实例堆 [top, top+12MB)
    p=buf.data()+sizeof(struct mach_header);
    for(uint32_t i=0;i<mh->ncmds;i++){auto*lc=(struct load_command*)p; if(lc->cmd==LC_SEGMENT){auto*sc=(struct segment_command*)p;
        if((uint64_t)sc->fileoff+sc->filesize<=buf.size()&&(uint64_t)sc->vmaddr+sc->filesize<=G.size()) memcpy(G.data()+sc->vmaddr,buf.data()+sc->fileoff,sc->filesize);
        struct section* sec=(struct section*)((uint8_t*)sc+sizeof(struct segment_command));
        for(uint32_t s2=0;s2<sc->nsects;s2++) if(strncmp(sec[s2].sectname,"__cfstring",16)==0){ CFSTR_LO=sec[s2].addr; CFSTR_HI=sec[s2].addr+sec[s2].size; }
        } p+=lc->cmdsize;}
    return (uint32_t)top;
}
static void load_stub_map(const char* path){
    FILE* f=fopen(path,"r"); if(!f){fprintf(stderr,"warn: stub map not found: %s\n",path); return;}
    char line[256];
    while(fgets(line,256,f)){ uint32_t a; char nm[200]; if(sscanf(line,"%x\t%199s",&a,nm)==2) STUB[a]=nm; }
    fclose(f);
}
static void load_classref_map(const char* path){
    FILE* f=fopen(path,"r"); if(!f){fprintf(stderr,"warn: classref map not found: %s\n",path); return;}
    char line[256];
    while(fgets(line,256,f)){ uint32_t a; char nm[200]; if(sscanf(line,"%x\t%199s",&a,nm)==2) CLSREF[a]=nm; }
    fclose(f);
}
static void load_method_map(const char* path){   // 类\t选择子\tIMP\t+/-
    FILE* f=fopen(path,"r"); if(!f){fprintf(stderr,"warn: method map not found\n"); return;}
    char line[512];
    while(fgets(line,512,f)){ char cls[200],sel[200],ty[4]; uint32_t imp;
        if(sscanf(line,"%199[^\t]\t%199[^\t]\t%x\t%3s",cls,sel,&imp,ty)==4){
            std::string k=std::string(cls)+"\t"+sel; if(ty[0]=='+') MPLUS[k]=imp; else MMINUS[k]=imp; } }
    fclose(f);
}
static void load_class_addr_map(const char* path){   // 0xADDR\t类名
    FILE* f=fopen(path,"r"); if(!f){fprintf(stderr,"warn: class addr map not found\n"); return;}
    char line[256];
    while(fgets(line,256,f)){ uint32_t a; char nm[200]; if(sscanf(line,"%x\t%199s",&a,nm)==2) CLSADDR[a]=nm; }
    fclose(f);
}
static void load_data_bind_map(const char* path){    // 0xADDR\t符号名
    FILE* f=fopen(path,"r"); if(!f){fprintf(stderr,"warn: data bind map not found\n"); return;}
    char line[256];
    while(fgets(line,256,f)){ uint32_t a; char nm[200]; if(sscanf(line,"%x\t%199s",&a,nm)==2) DBIND[a]=nm; }
    fclose(f);
}
