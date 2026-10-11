package org.example.examples.shopping.ios

import androidx.compose.ui.window.ComposeUIViewController
import org.example.examples.shopping.di.PlatformContext
import org.example.examples.shopping.di.ShoppingGraph
import org.example.examples.shopping.presentation.host.ShoppingApp
import rocky.ios.RockyLifecycle

private val lifecycle = RockyLifecycle.register()
private val graph: ShoppingGraph by lazy { ShoppingGraph(PlatformContext(lifecycle)) }

fun sceneVisibility(foreground: Boolean) {
    lifecycle.sceneVisibility(foreground)
}

/** Hands a URL the system opened in this app to its deep links. */
fun openLink(url: String) = graph.deepLinks.open(url)

fun viewController() = ComposeUIViewController { ShoppingApp(graph) }
