package org.example.examples.shopping.di

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
import org.example.examples.shopping.data.session.ShoppingPlatform
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.navigation.DeepLinks
import org.example.examples.shopping.util.splash.SplashController
import kotlin.reflect.KClass

/** The app's object graph: the host's [PlatformContext] comes in, the session and every ViewModel come out. */
@DependencyGraph(AppScope::class)
interface ShoppingGraph : ViewModelGraph {
    val session: ShoppingSession

    /** Where each platform hands its incoming app links. */
    val deepLinks: DeepLinks

    /** Whether the launch splash still shows, for a platform that holds its own. */
    val splash: SplashController

    @DependencyGraph.Factory
    fun interface Factory {
        fun create(@Provides context: PlatformContext): ShoppingGraph
    }
}

/** Creates the graph once per app process; each platform contributes its [ShoppingPlatform] binding. */
fun ShoppingGraph(context: PlatformContext): ShoppingGraph = createGraphFactory<ShoppingGraph.Factory>().create(context)

/** Creates the ViewModels contributed with `@ViewModelKey`. */
@Inject
@SingleIn(AppScope::class)
@ContributesBinding(AppScope::class)
class ShoppingViewModelFactory(
    override val viewModelProviders: Map<KClass<out ViewModel>, () -> ViewModel>,
) : MetroViewModelFactory()
