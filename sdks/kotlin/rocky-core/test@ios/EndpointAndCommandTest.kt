@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky
import kotlinx.atomicfu.atomic
import kotlinx.coroutines.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import kotlin.test.*

class EndpointAndCommandTest {
    @Test fun applicationProtocolSurvivesDatabaseCloseAndReopen() = runBlocking {
        withTimeout(30_000) {
            val alpn = "rocky.phase17.echo.v1"
            val endpoint = RockyEndpoint.bind(
                ByteArray(32) { 17 },
                relay = "disabled",
                discovery = false,
                bind = "127.0.0.1:0",
                protocols = mapOf(
                    alpn to { _, stream ->
                        val bytes = stream.read(128)
                        stream.write(bytes)
                        stream.finish()
                    },
                ),
            )
            val caller = RockyEndpoint.bind(ByteArray(32) { 18 }, relay = "disabled", discovery = false, bind = "127.0.0.1:0")
            val root = NSTemporaryDirectory() + "rocky-endpoint-" + NSUUID().UUIDString
            val definition = schema("endpoint", 1) {
                table("items") {
                    text("key")
                    primaryKey("key")
                }
            }
            suspend fun echo() {
                val stream = caller.connect(endpoint.address(), alpn)
                try {
                    stream.write("hello".encodeToByteArray())
                    stream.finish()
                    assertEquals("hello", stream.read(128).decodeToString())
                } finally {
                    stream.close()
                }
            }
            try {
                val database = RockyDatabase.create(root, definition, PeerTransport.Endpoint(endpoint), automaticSync = false)
                echo()
                database.close()
                echo()
                val reopened = RockyDatabase.open(root, definition, PeerTransport.Endpoint(endpoint))
                try {
                    echo()
                } finally {
                    reopened.close()
                }
                echo()
            } finally {
                caller.shutdown()
                endpoint.shutdown()
                NSFileManager.defaultManager.removeItemAtPath(root, null)
            }
        }
    }

    @Test fun kotlinCommandHandlerIsCalledByRust() = runBlocking {
        val calls = atomic(0)
        val probe = Command("Probe", handler = "phase17.probe.v1", arguments = Shape.none, accepted = Shape.none, rejected = Shape.none, invocation = "items", result = "items", resources = listOf("items"))
        val definition = schema("commandcallback", 1) {
            table("items") {
                text("key")
                primaryKey("key")
            }
            commands = CommandSet(listOf(probe))
        }
        val coordinator = DevelopmentCoordinator()
        val root = NSTemporaryDirectory() + "rocky-command-" + NSUUID().UUIDString
        val database = RockyDatabase.local(
            root,
            definition,
            commands = CommandRuntime(
                coordinator,
                listOf(
                    probe handledBy { _, _ ->
                        calls.incrementAndGet()
                        Decision.accept(Unit)
                    },
                ),
            ),
        )
        try {
            coordinator.register(database.commandIdentity())
            val handle = RockyDatabase.newHandle()
            database.submit(probe, Unit, handle)
            assertEquals(TicketState.Applied, database.run(probe, handle).state)
            assertTrue(calls.value > 0)
        } finally {
            database.close()
            coordinator.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}
