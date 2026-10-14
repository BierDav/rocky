package rocky
import kotlinx.serialization.json.JsonObject
import java.io.File
import java.security.SecureRandom
internal actual fun secureBytes(size: Int): ByteArray = ByteArray(size).also { SecureRandom().nextBytes(it) }
internal actual fun fileExists(path: String): Boolean = File(path).exists()
actual open class PlatformOpeners {
    fun exists(directory: File) = RockyDatabase.exists(directory.absolutePath)
    suspend fun local(directory: File, schema: Schema, secrets: SecretStore? = null, commands: CommandRuntime? = null) = RockyDatabase.local(directory.absolutePath, schema, secrets, commands)
    suspend fun install(
        directory: File,
        schema: Schema,
        supplied: JsonObject,
        secrets: SecretStore? = null,
        transport: PeerTransport? = null,
        commands: CommandRuntime? = null,
    ) = RockyDatabase.install(directory.absolutePath, schema, supplied, secrets, transport, commands)
    suspend fun create(directory: File, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null) = RockyDatabase.create(directory.absolutePath, schema, transport, secrets, automaticSync, commands)
    suspend fun join(directory: File, schema: Schema, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null) = RockyDatabase.join(directory.absolutePath, schema, invitation, transport, secrets, automaticSync, commands)
    suspend fun openOrCreate(directory: File, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null, confirmation: String? = null) = RockyDatabase.openOrCreate(directory.absolutePath, schema, transport, secrets, automaticSync, commands, confirmation)
    suspend fun open(directory: File, schema: Schema, transport: PeerTransport? = null, secrets: SecretStore? = null, automaticSync: Boolean = false, commands: CommandRuntime? = null, confirmation: String? = null) = RockyDatabase.open(directory.absolutePath, schema, transport, secrets, automaticSync, commands, confirmation)
    suspend fun resume(directory: File, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null, confirmation: String? = null) = RockyDatabase.resume(directory.absolutePath, schema, transport, secrets, automaticSync, commands, confirmation)
    suspend fun restore(directory: File, schema: Schema, archive: File, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, commands: CommandRuntime? = null) = RockyDatabase.restore(directory.absolutePath, schema, archive.absolutePath, invitation, transport, secrets, commands)
}
actual open class PlatformCredentialOpeners {
    suspend fun prepare(directory: File, secrets: SecretStore? = null) = Credentials.prepare(directory.absolutePath, secrets)
}
suspend fun RockyDatabase.backup(archive: File) = backup(archive.absolutePath)
