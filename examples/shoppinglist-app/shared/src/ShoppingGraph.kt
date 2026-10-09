package org.example.examples.shopping

import androidx.lifecycle.ViewModel
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesBinding
import dev.zacsweers.metro.DependencyGraph
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import dev.zacsweers.metro.createGraphFactory
import dev.zacsweers.metrox.viewmodel.MetroViewModelFactory
import dev.zacsweers.metrox.viewmodel.ViewModelGraph
import kotlin.reflect.KClass

/** The app's object graph: the platform comes in, the session and every screen's ViewModel come out. */
@DependencyGraph(AppScope::class)
interface ShoppingGraph : ViewModelGraph {
    val session: ShoppingSession

    @DependencyGraph.Factory
    fun interface Factory {
        fun create(@Provides platform: ShoppingPlatform): ShoppingGraph
    }
}

fun ShoppingGraph(platform: ShoppingPlatform): ShoppingGraph = createGraphFactory<ShoppingGraph.Factory>().create(platform)

/** Creates the ViewModels contributed with `@ViewModelKey`. */
@Inject
@SingleIn(AppScope::class)
@ContributesBinding(AppScope::class)
class ShoppingViewModelFactory(
    override val viewModelProviders: Map<KClass<out ViewModel>, () -> ViewModel>,
) : MetroViewModelFactory()
