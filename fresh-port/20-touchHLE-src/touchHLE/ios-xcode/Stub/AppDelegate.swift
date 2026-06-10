import UIKit

// 最小 iOS app 桩:仅用于让 xcodebuild 自动签名生成描述文件(provisioning profile)。
// 真正的 touchHLE 可执行 + 资源由后续脚本组装并用开发证书+该描述文件签名。
@main
class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        true
    }
}
