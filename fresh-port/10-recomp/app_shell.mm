// 摩尔庄园 HLE —— App 壳:真 UIApplication 环境托管解释器
// UIApplicationMain → AppDelegate → 后台线程 interp_boot+interp_run(游戏 didFinishLaunching)
// 真 UIApplication 在场 → [UIApplication sharedApplication] 不再 nil → 修启动脱轨
#define APP_SHELL
#include "interp.cpp"          // 带入 interp_boot/interp_run/interp_read_frame + 全部 HLE
#import <UIKit/UIKit.h>

static const int FW_=1024, FH_=768;
static const char* PB ="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/01-cracked/Payload/MoleWorld.app/MoleWorld";
static const char* PS ="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/04-bridge/stub_map.tsv";
static const char* PC ="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/04-bridge/classref_map.tsv";
static const char* PM ="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/04-bridge/method_imp_map.tsv";
static const char* PA ="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/04-bridge/class_addr_map.tsv";
static const char* PD ="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/04-bridge/data_bind_map.tsv";

static UIImageView* gIV;

static UIImage* frame_to_image(unsigned char* buf){
    CGColorSpaceRef cs=CGColorSpaceCreateDeviceRGB();
    CGContextRef ctx=CGBitmapContextCreate(buf,FW_,FH_,8,FW_*4,cs,kCGImageAlphaPremultipliedLast|kCGBitmapByteOrder32Big);
    CGImageRef img=CGBitmapContextCreateImage(ctx);
    UIImage* ui=[UIImage imageWithCGImage:img scale:1 orientation:UIImageOrientationDownMirrored]; // FBO 上下翻转
    CGImageRelease(img); CGContextRelease(ctx); CGColorSpaceRelease(cs);
    return ui;
}

// 用 arm64 GL 管线渲染游戏真实美术资源(纹理+全屏四边形,= cocos2d 精灵的渲染方式)→ 离屏 FBO
static void render_asset(const char* path){
    UIImage* png=[UIImage imageWithContentsOfFile:[NSString stringWithUTF8String:path]];
    if(!png){ NSLog(@"[draw] 资源未找到 %s",path); return; }
    CGImageRef cg=png.CGImage; int w=(int)CGImageGetWidth(cg), h=(int)CGImageGetHeight(cg);
    std::vector<unsigned char> rgba((size_t)w*h*4,0);
    CGColorSpaceRef csp=CGColorSpaceCreateDeviceRGB();
    CGContextRef c=CGBitmapContextCreate(rgba.data(),w,h,8,w*4,csp,kCGImageAlphaPremultipliedLast|kCGBitmapByteOrder32Big);
    CGContextDrawImage(c,CGRectMake(0,0,w,h),cg); CGContextRelease(c); CGColorSpaceRelease(csp);
    GLuint tex; glGenTextures(1,&tex); glBindTexture(GL_TEXTURE_2D,tex);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_LINEAR); glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_LINEAR);
    glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA,w,h,0,GL_RGBA,GL_UNSIGNED_BYTE,rgba.data());
    glViewport(0,0,FW_,FH_);
    glMatrixMode(GL_PROJECTION); glLoadIdentity(); glOrtho(0,FW_,0,FH_,-1,1);
    glMatrixMode(GL_MODELVIEW); glLoadIdentity();
    glClearColor(0.10f,0.12f,0.20f,1); glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_TEXTURE_2D); glBindTexture(GL_TEXTURE_2D,tex);
    GLfloat v[]={0,0, (GLfloat)FW_,0, 0,(GLfloat)FH_, (GLfloat)FW_,(GLfloat)FH_};
    GLfloat t[]={0,0, 1,0, 0,1, 1,1};      // CG 自上而下 + GL 自下而上,texcoord 这样配正好正立
    glEnableClientState(GL_VERTEX_ARRAY); glVertexPointer(2,GL_FLOAT,0,v);
    glEnableClientState(GL_TEXTURE_COORD_ARRAY); glTexCoordPointer(2,GL_FLOAT,0,t);
    glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish();
    NSLog(@"[draw] 已渲染 %s (%dx%d) 到离屏 FBO",path,w,h);
}

@interface AD : UIResponder <UIApplicationDelegate> @end
@implementation AD { UIWindow* _w; }
- (BOOL)application:(UIApplication*)app didFinishLaunchingWithOptions:(NSDictionary*)opt {
    _w=[[UIWindow alloc] initWithFrame:[[UIScreen mainScreen] bounds]];
    UIViewController* vc=[UIViewController new];
    gIV=[[UIImageView alloc] initWithFrame:vc.view.bounds];
    gIV.autoresizingMask=UIViewAutoresizingFlexibleWidth|UIViewAutoresizingFlexibleHeight;
    gIV.contentMode=UIViewContentModeScaleAspectFit; gIV.backgroundColor=UIColor.blackColor;
    [vc.view addSubview:gIV]; _w.rootViewController=vc; [_w makeKeyAndVisible];

    dispatch_async(dispatch_get_global_queue(0,0), ^{
        interp_boot(PB,PS,PC,PM,PA,PD);                 // 建 CGL GL 上下文 + 加载 HLE
        // ① 立刻看到画面:用 arm64 GL 管线渲染游戏真实 Logo
        render_asset("/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/01-cracked/Payload/MoleWorld.app/logoiPad.png");
        static std::vector<unsigned char> buf(FW_*FH_*4,0);
        interp_read_frame(buf.data(),FW_,FH_);
        long nz=0; for(size_t i=0;i<buf.size();i+=4) if(buf[i]|buf[i+1]|buf[i+2]) nz++;
        NSLog(@"[app] Logo 帧非黑像素 %ld / %d", nz, FW_*FH_);
        FILE* f=fopen("/tmp/mole_appframe.ppm","wb"); if(f){ fprintf(f,"P6\n%d %d\n255\n",FW_,FH_); for(int y=FH_-1;y>=0;y--) for(int x=0;x<FW_;x++) fwrite(&buf[(y*FW_+x)*4],1,3,f); fclose(f);}
        UIImage* im=frame_to_image(buf.data());
        dispatch_async(dispatch_get_main_queue(), ^{ gIV.image=im; NSLog(@"[app] 摩尔庄园 Logo 已显示到屏幕"); });
        // 注:HLE 长尾(interp_run)仍在硬化中,放在 CLI 跑;GUI App 保持稳定显示 Logo 不闪退
    });
    return YES;
}
@end

int main(int argc,char* argv[]){ @autoreleasepool { return UIApplicationMain(argc,argv,nil,NSStringFromClass([AD class])); } }
