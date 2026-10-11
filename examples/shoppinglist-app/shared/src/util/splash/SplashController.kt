package org.example.examples.shopping.util.splash

import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Whether the launch splash still covers the app; each platform keeps its own splash up while [visible]. */
@SingleIn(AppScope::class)
@Inject
class SplashController {
    private val mutableVisible = MutableStateFlow(true)
    val visible: StateFlow<Boolean> = mutableVisible.asStateFlow()

    /** Called once the first screen worth showing is drawn. */
    fun hide() {
        mutableVisible.value = false
    }
}
