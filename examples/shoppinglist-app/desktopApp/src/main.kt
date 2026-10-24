package org.example.examples

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import org.example.examples.shopping.di.PlatformContext
import org.example.examples.shopping.di.ShoppingGraph
import org.example.examples.shopping.diagnostics.DIAGNOSTIC_BUILD
import org.example.examples.shopping.diagnostics.DiagnosticLog
import org.example.examples.shopping.diagnostics.observeDiagnostics
import org.example.examples.shopping.presentation.host.ShoppingApp
import java.awt.Desktop
import java.io.File

/**
 * The household lives in `ROCKY_SHOPPING_DATA`, else `local/.rocky-shopping`; see [PlatformContext]. A `rocky-shopping://`
 * link arrives as the only argument, as Windows and Linux pass it, or through the system's open-URI event on macOS.
 */
fun main(args: Array<String>) {
    val label = System.getenv("ROCKY_SHOPPING_INSTANCE") ?: "Shopping"
    val context = PlatformContext(deviceName = label)
    val log = DiagnosticLog(File(context.directory, "logs"), DIAGNOSTIC_BUILD, label)
    val graph = ShoppingGraph(PlatformContext(context.directory, label) { log.write(it) })
    val diagnostics = observeDiagnostics(graph.session, CoroutineScope(SupervisorJob() + Dispatchers.IO), log::write)
    if (args.size == 1) graph.deepLinks.open(args[0])
    if (Desktop.isDesktopSupported() && Desktop.getDesktop().isSupported(Desktop.Action.APP_OPEN_URI)) {
        Desktop.getDesktop().setOpenURIHandler { event -> graph.deepLinks.open(event.uri.toString()) }
    }
    application {
        val closingScope = rememberCoroutineScope()
        var closing by remember { mutableStateOf(false) }
        Window(
            onCloseRequest = {
                if (!closing) {
                    closing = true
                    closingScope.launch {
                        diagnostics.cancelAndJoin()
                        graph.session.close()
                        log.write("shutdown")
                        exitApplication()
                    }
                }
            },
            title = label,
            state = rememberWindowState(width = 1200.dp, height = 860.dp),
        ) { ShoppingApp(graph) }
    }
}
