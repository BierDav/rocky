package org.example.examples.shopping.presentation.navigation

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.scene.Scene
import androidx.navigation3.scene.SceneDecoratorStrategy
import androidx.navigation3.scene.SceneDecoratorStrategyScope

/** One top-level destination in the navigation bar or rail. */
data class NavigationItem(val key: NavKey, val label: String, val icon: ImageVector)

/**
 * Puts scenes of entries marked [navigationSuite] beside the [navigator]'s tabs: a bar below them, or a rail on the
 * left once the window is at least [railWidth] wide. Every decorated scene shares one key, so the bar stays put while
 * the destination inside it changes; other scenes show without it.
 */
class NavigationSuiteSceneDecoratorStrategy<T : Any>(
    private val items: List<NavigationItem>,
    private val navigator: Navigator,
    private val railWidth: Dp = 600.dp,
) : SceneDecoratorStrategy<T> {
    override fun SceneDecoratorStrategyScope<T>.decorateScene(scene: Scene<T>): Scene<T> = if (scene.metadata[NAVIGATION_SUITE] == true) NavigationSuiteScene(scene, this@NavigationSuiteSceneDecoratorStrategy) else scene

    @Composable
    internal fun Chrome(content: @Composable () -> Unit) {
        BoxWithConstraints(Modifier.fillMaxSize()) {
            if (maxWidth >= railWidth) {
                Row(Modifier.fillMaxSize()) {
                    NavigationRail(Modifier.fillMaxHeight()) {
                        Spacer(Modifier.weight(1f))
                        items.forEach { item ->
                            NavigationRailItem(navigator.tab == item.key, { navigator.tab = item.key }, icon = { Icon(item.icon, null) }, label = { Text(item.label) })
                        }
                        Spacer(Modifier.weight(1f))
                    }
                    Box(Modifier.weight(1f).fillMaxHeight()) { content() }
                }
            } else {
                Column(Modifier.fillMaxSize()) {
                    Box(Modifier.weight(1f).fillMaxWidth()) { content() }
                    NavigationBar {
                        items.forEach { item ->
                            NavigationBarItem(navigator.tab == item.key, { navigator.tab = item.key }, icon = { Icon(item.icon, null) }, label = { Text(item.label) })
                        }
                    }
                }
            }
        }
    }

    companion object {
        private const val NAVIGATION_SUITE = "navigationSuite"

        /** Entry metadata that shows the entry inside the navigation bar or rail. */
        fun navigationSuite(): Map<String, Any> = mapOf(NAVIGATION_SUITE to true)
    }
}

private class NavigationSuiteScene<T : Any>(
    private val scene: Scene<T>,
    private val strategy: NavigationSuiteSceneDecoratorStrategy<T>,
) : Scene<T> {
    override val key: Any get() = NavigationSuiteScene::class
    override val entries: List<NavEntry<T>> get() = scene.entries
    override val previousEntries: List<NavEntry<T>> get() = scene.previousEntries
    override val metadata: Map<String, Any> get() = scene.metadata
    override val content: @Composable () -> Unit = { strategy.Chrome(scene.content) }
}
