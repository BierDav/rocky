package org.example.examples.shopping.presentation.navigation

import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.rememberNavBackStack

/**
 * The app's navigation state: a root stack for screens outside a household, and one stack per tab while it is open.
 * Each tab keeps its screens, saved state and ViewModels while another shows; [tabs]' first is the start tab, which
 * the others return to on Back. Leaving the tabs empties their stacks, which releases their ViewModels.
 */
@Stable
class Navigator internal constructor(
    val tabs: List<NavKey>,
    internal val root: NavBackStack<NavKey>,
    internal val tabStacks: List<NavBackStack<NavKey>>,
    selected: MutableIntState,
) {
    private var selected by selected

    /** Whether the tabs show instead of the root stack. */
    val inTabs: Boolean get() = root.isEmpty()

    /** The tab on screen while [inTabs]. */
    var tab: NavKey
        get() = tabs[selected]
        set(value) {
            selected = tabs.indexOf(value).coerceAtLeast(0)
        }

    /** Shows [destination] alone, or the tabs at their start for null. */
    fun showRoot(destination: NavKey?) {
        if (destination == null) {
            if (inTabs) return
            root.clear()
            tabStacks.forEachIndexed { index, stack -> if (stack.isEmpty()) stack.add(tabs[index]) }
            selected = 0
        } else {
            if (root.toList() == listOf(destination)) return
            root.clear()
            root.add(destination)
            tabStacks.forEach { it.clear() }
        }
    }

    /** Opens [destination] above the current screen, replacing an earlier one of its kind. */
    fun navigate(destination: NavKey) {
        val stack = current()
        stack.removeAll { it::class == destination::class }
        stack.add(destination)
    }

    fun remove(destination: NavKey) {
        (listOf(root) + tabStacks).forEach { it.remove(destination) }
    }

    /** Pops the current stack, or returns to the start tab from another tab's root. */
    fun back() {
        val stack = current()
        when {
            stack.size > 1 -> stack.removeAt(stack.lastIndex)
            inTabs && selected != 0 -> selected = 0
        }
    }

    private fun current(): NavBackStack<NavKey> = if (inTabs) tabStacks[selected] else root
}

@Composable
fun rememberNavigator(tabs: List<NavKey>, initial: NavKey?): Navigator {
    // Only the first composition's destination seeds the stacks; later ones arrive through showRoot.
    val start = remember { initial }
    val root = rememberNavBackStack(navigationSavedStateConfig, *listOfNotNull(start).toTypedArray())
    val tabStacks = tabs.map { tab -> rememberNavBackStack(navigationSavedStateConfig, *(if (start == null) arrayOf(tab) else emptyArray())) }
    val selected = rememberSaveable { mutableIntStateOf(0) }
    return remember(tabs, root, tabStacks, selected) { Navigator(tabs, root, tabStacks, selected) }
}

/** The app's navigator, for screens and components that move between destinations. */
val LocalNavigator = staticCompositionLocalOf<Navigator> { error("No Navigator provided") }
