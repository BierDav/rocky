package org.example.examples.shopping.presentation.navigation

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.scene.Scene
import androidx.navigation3.scene.SceneDecoratorStrategy
import androidx.navigation3.scene.SceneDecoratorStrategyScope
import org.example.examples.shopping.presentation.component.LocalSnackbarHostState

/** Gives every scene the app's [snackbar]: its screens report through [LocalSnackbarHostState], shown at its bottom. */
class SnackbarSceneDecoratorStrategy<T : Any>(private val snackbar: SnackbarHostState) : SceneDecoratorStrategy<T> {
    override fun SceneDecoratorStrategyScope<T>.decorateScene(scene: Scene<T>): Scene<T> = SnackbarScene(scene, snackbar)
}

private class SnackbarScene<T : Any>(private val scene: Scene<T>, private val snackbar: SnackbarHostState) : Scene<T> {
    override val key: Any get() = scene.key
    override val entries: List<NavEntry<T>> get() = scene.entries
    override val previousEntries: List<NavEntry<T>> get() = scene.previousEntries
    override val metadata: Map<String, Any> get() = scene.metadata
    override val content: @Composable () -> Unit = {
        Box(Modifier.fillMaxSize()) {
            CompositionLocalProvider(LocalSnackbarHostState provides snackbar) { scene.content() }
            SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
        }
    }
}
