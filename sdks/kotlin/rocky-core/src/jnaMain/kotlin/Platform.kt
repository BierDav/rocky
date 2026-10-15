package rocky
import kotlinx.serialization.json.JsonObject
import java.io.File
import java.security.SecureRandom
internal actual fun secureBytes(size: Int): ByteArray = ByteArray(size).also { SecureRandom().nextBytes(it) }
internal actual fun fileExists(path: String): Boolean = File(path).exists()
actual open class PlatformOpeners {
    fun exists(directory: File) = RockyDatabase.exists(directory.absolutePath)
    suspend fun local(directory: File, schema: Schema, secrets: SecretStore? = null, coordinator: ServiceCoordinator? = null, identity: IdentityProvider? = null) = RockyDatabase.local(directory.absolutePath, schema, secrets, coordinator, identity)
    suspend fun install(
        directory: File,
        schema: Schema,
        supplied: JsonObject,
        secrets: SecretStore? = null,
        transport: PeerTransport? = null,
        coordinator: ServiceCoordinator? = null,
        identity: IdentityProvider? = null,
    ) = RockyDatabase.install(directory.absolutePath, schema, supplied, secrets, transport, coordinator, identity)
    suspend fun create(directory: File, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, coordinator: ServiceCoordinator? = null, identity: IdentityProvider? = null) = RockyDatabase.create(directory.absolutePath, schema, transport, secrets, automaticSync, coordinator, identity)
    suspend fun join(directory: File, schema: Schema, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, coordinator: ServiceCoordinator? = null, identity: IdentityProvider? = null) = RockyDatabase.join(directory.absolutePath, schema, invitation, transport, secrets, automaticSync, coordinator, identity)
    suspend fun enrollIdentity(directory: File, schema: Schema, contact: String, identity: IdentityProvider, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, coordinator: ServiceCoordinator? = null) = RockyDatabase.enrollIdentity(directory.absolutePath, schema, contact, identity, transport, secrets, automaticSync, coordinator)
    suspend fun openOrCreate(directory: File, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, coordinator: ServiceCoordinator? = null, confirmation: String? = null, identity: IdentityProvider? = null) = RockyDatabase.openOrCreate(directory.absolutePath, schema, transport, secrets, automaticSync, coordinator, confirmation, identity)
    suspend fun open(directory: File, schema: Schema, transport: PeerTransport? = null, secrets: SecretStore? = null, automaticSync: Boolean = false, coordinator: ServiceCoordinator? = null, confirmation: String? = null, identity: IdentityProvider? = null) = RockyDatabase.open(directory.absolutePath, schema, transport, secrets, automaticSync, coordinator, confirmation, identity)
    suspend fun resume(directory: File, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, coordinator: ServiceCoordinator? = null, confirmation: String? = null, identity: IdentityProvider? = null) = RockyDatabase.resume(directory.absolutePath, schema, transport, secrets, automaticSync, coordinator, confirmation, identity)
    suspend fun restore(directory: File, schema: Schema, archive: File, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, coordinator: ServiceCoordinator? = null) = RockyDatabase.restore(directory.absolutePath, schema, archive.absolutePath, invitation, transport, secrets, coordinator)
}
actual open class PlatformCredentialOpeners {
    suspend fun prepare(directory: File, secrets: SecretStore? = null, identity: IdentityProvider? = null) = Credentials.prepare(directory.absolutePath, secrets, identity)
}
suspend fun RockyDatabase.backup(archive: File) = backup(archive.absolutePath)
