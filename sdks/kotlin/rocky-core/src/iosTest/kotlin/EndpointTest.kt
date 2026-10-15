@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky
import kotlinx.coroutines.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import kotlin.test.*

class EndpointTest {
    @Test fun applicationProtocolSurvivesDatabaseCloseAndReopen() = runBlocking {
        withTimeout(30_000) {
            val alpn = "rocky.phase17.echo.v1"
            val endpoint = RockyEndpoint.bind(
                ByteArray(32) { 17 },
                relay = "disabled",
                lookup = "disabled",
                bind = "127.0.0.1:0",
                protocols = mapOf(
                    alpn to { _, stream ->
                        val bytes = stream.read(128)
                        stream.write(bytes)
                        stream.finish()
                    },
                ),
            )
            val caller = RockyEndpoint.bind(ByteArray(32) { 18 }, relay = "disabled", lookup = "disabled", bind = "127.0.0.1:0")
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
}
