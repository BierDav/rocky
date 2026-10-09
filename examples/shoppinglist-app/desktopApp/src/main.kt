package org.example.examples

import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import androidx.room.Room
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.example.examples.shopping.NodeKind
import org.example.examples.shopping.SessionState
import org.example.examples.shopping.ShoppingDatabase
import org.example.examples.shopping.ShoppingGraph
import org.example.examples.shopping.ShoppingPlatform
import org.example.examples.shopping.publicCoordination
import org.example.examples.shopping.ui.ShoppingApp
import java.io.File
import java.net.InetAddress

/** The household lives in `ROCKY_SHOPPING_DATA`, else `~/.rocky-shopping`. */
fun main(args: Array<String>) {
    require(
        args.isEmpty() ||
            (args.size == 2 && args[0] == "--join-file") ||
            (args.size == 3 && args[0] == "--invite-file"),
    ) {
        "Usage: Shopping [--join-file PATH | --invite-file PATH DEVICE_NAME]"
    }
    val directory = File(System.getenv("ROCKY_SHOPPING_DATA") ?: File(System.getProperty("user.home"), ".rocky-shopping").path)
    val name = runCatching { InetAddress.getLocalHost().hostName.substringBefore('.') }.getOrDefault("Desktop")
    val graph = ShoppingGraph(
        ShoppingPlatform(
            directory,
            name,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
            commands = ::publicCoordination,
        ),
    )
    application {
        LaunchedEffect(graph) {
            if (args.firstOrNull() == "--join-file" && graph.session.state.value == SessionState.Setup) {
                graph.session.join(File(args[1]).readText().trim())
            }
            if (args.firstOrNull() == "--invite-file") {
                val state = graph.session.state.first { it is SessionState.Open || it is SessionState.Failed }
                val node = (state as? SessionState.Open)?.node ?: throw (state as SessionState.Failed).error
                File(args[1]).writeText(node.household.invite(NodeKind.Full, args[2]).toString())
            }
        }
        Window(
            onCloseRequest = {
                runBlocking { graph.session.close() }
                exitApplication()
            },
            title = "Shopping",
            state = rememberWindowState(width = 520.dp, height = 860.dp),
        ) { ShoppingApp(graph) }
    }
}
