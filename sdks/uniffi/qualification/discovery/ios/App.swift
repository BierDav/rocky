import UIKit

@main
class App: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool { true }
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Discovery", sessionRole: session.role)
        configuration.delegateClass = Scene.self
        return configuration
    }
}

class Scene: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    var probe: Probe?
    var display = UITextView()
    var timer: Timer?
    var failure = "starting"

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let controller = UIViewController()
        controller.view.backgroundColor = .systemBackground
        display.isEditable = false
        display.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        display.accessibilityIdentifier = "discovery-result"
        display.frame = CGRect(x: 12, y: 70, width: 350, height: 650)
        controller.view.addSubview(display)
        window = UIWindow(windowScene: scene)
        window?.rootViewController = controller
        window?.makeKeyAndVisible()
        do { probe = try Probe() } catch { failure = "failed: \(error)" }
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.refresh() }
        refresh()
    }
    func refresh() {
        let status = probe?.status() ?? failure
        display.text = status
        let file = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("result.json")
        try? status.write(to: file, atomically: true, encoding: .utf8)
    }
    func sceneDidEnterBackground(_ scene: UIScene) { try? probe?.foreground(foreground: false); refresh() }
    func sceneWillEnterForeground(_ scene: UIScene) { try? probe?.foreground(foreground: true); refresh() }
    func sceneDidDisconnect(_ scene: UIScene) { timer?.invalidate(); try? probe?.shutdown() }
}
