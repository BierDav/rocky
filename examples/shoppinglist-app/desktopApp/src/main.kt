package org.example.examples

import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import kotlinx.coroutines.runBlocking
import org.example.examples.shopping.di.PlatformContext
import org.example.examples.shopping.di.ShoppingGraph
import org.example.examples.shopping.presentation.host.ShoppingApp
import java.awt.Desktop

/**
 * The household lives in `ROCKY_SHOPPING_DATA`, else `~/.rocky-shopping`; see [PlatformContext]. A `rocky-shopping://`
 * link arrives as the only argument, as Windows and Linux pass it, or through the system's open-URI event on macOS.
 */
fun main(args: Array<String>) {
    val graph = ShoppingGraph(PlatformContext())
    if (args.size == 1) graph.deepLinks.open(args[0])
    if (Desktop.isDesktopSupported() && Desktop.getDesktop().isSupported(Desktop.Action.APP_OPEN_URI)) {
        Desktop.getDesktop().setOpenURIHandler { event -> graph.deepLinks.open(event.uri.toString()) }
    }
    application {
        Window(
            onCloseRequest = {
                runBlocking { graph.session.close() }
                exitApplication()
            },
            title = "Shopping",
            state = rememberWindowState(width = 1200.dp, height = 860.dp),
        ) { ShoppingApp(graph) }
    }
}
