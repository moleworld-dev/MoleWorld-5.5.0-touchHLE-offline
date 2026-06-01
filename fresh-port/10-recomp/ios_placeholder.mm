// 摩尔庄园 iOS26 占位 App:原生 arm64,显示游戏真实 Logo(可装真机,先看到画面)
#import <UIKit/UIKit.h>
@interface PAD : UIResponder <UIApplicationDelegate> @end
@implementation PAD { UIWindow* _w; }
- (BOOL)application:(UIApplication*)a didFinishLaunchingWithOptions:(NSDictionary*)o {
    _w=[[UIWindow alloc] initWithFrame:[[UIScreen mainScreen] bounds]];
    UIViewController* vc=[UIViewController new]; vc.view.backgroundColor=UIColor.blackColor;
    UIImageView* iv=[[UIImageView alloc] initWithFrame:vc.view.bounds];
    iv.autoresizingMask=UIViewAutoresizingFlexibleWidth|UIViewAutoresizingFlexibleHeight;
    iv.contentMode=UIViewContentModeScaleAspectFit;
    NSString* p=[[NSBundle mainBundle] pathForResource:@"logoiPad" ofType:@"png"];
    iv.image=[UIImage imageWithContentsOfFile:p];
    [vc.view addSubview:iv]; _w.rootViewController=vc; [_w makeKeyAndVisible];
    return YES;
}
@end
int main(int c,char*v[]){ @autoreleasepool { return UIApplicationMain(c,v,nil,NSStringFromClass([PAD class])); } }
