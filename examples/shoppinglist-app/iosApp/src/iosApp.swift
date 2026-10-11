import KotlinModules
import SwiftUI

struct ComposeView: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> some UIViewController {
        ViewControllerKt.viewController()
    }
    func updateUIViewController(_ uiViewController: UIViewControllerType, context: Context) {}
}

struct ContentView: View {
    @Environment(\.scenePhase) private var scenePhase
    var body: some View {
        ComposeView().ignoresSafeArea()
            .onChange(of: scenePhase) { _, phase in
                switch phase {
                case .active: ViewControllerKt.sceneVisibility(foreground: true)
                case .background: ViewControllerKt.sceneVisibility(foreground: false)
                default: break
                }
            }
            .onOpenURL { url in
                ViewControllerKt.openLink(url: url.absoluteString)
            }
    }
}

@main
struct ShoppingApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
