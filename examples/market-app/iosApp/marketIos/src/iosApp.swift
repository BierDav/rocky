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
        ComposeView().ignoresSafeArea(.keyboard)
            .onChange(of: scenePhase) { _, phase in
                switch phase {
                case .active: ViewControllerKt.sceneVisibility(foreground: true)
                case .background: ViewControllerKt.sceneVisibility(foreground: false)
                default: break
                }
            }
    }
}

@main
struct MarketApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
