package rocky

/** One installation's configuration, reused unchanged for explicit lifecycle operations. */
object Rocky {
    fun configure(directory: String, schema: Schema, configure: RockyConfiguration.Builder.() -> Unit = {}): RockyConfiguration = RockyConfiguration.Builder(directory, schema).apply(configure).build()
}

class RockyConfiguration internal constructor(
    val directory: String,
    val schema: Schema,
    private val secrets: SecretStore?,
    private val identity: IdentityProvider?,
    private val coordinator: ServiceCoordinator?,
    private val transport: PeerTransport?,
    private val automaticSync: Boolean,
) {
    fun exists(): Boolean = RockyDatabase.exists(directory)
    suspend fun open(confirmation: String? = null): RockyDatabase = RockyDatabase.open(directory, schema, transport, secrets, automaticSync, coordinator, confirmation, identity)
    suspend fun create(): RockyDatabase = if (transport == null) {
        RockyDatabase.local(directory, schema, secrets, coordinator, identity)
    } else {
        RockyDatabase.create(directory, schema, transport, secrets, automaticSync, coordinator, identity)
    }
    suspend fun join(invitation: String): RockyDatabase = RockyDatabase.join(directory, schema, invitation, requireNotNull(transport) { "Joining needs networking enabled" }, secrets, automaticSync, coordinator, identity)

    class Builder internal constructor(private val directory: String, private val schema: Schema) {
        private var secrets: SecretStore? = null
        private var identity: IdentityProvider? = null
        private var coordinator: ServiceCoordinator? = null
        private val network = Networking()
        fun secrets(store: SecretStore?) {
            secrets = store
        }
        fun identity(provider: IdentityProvider) {
            identity = provider
        }
        fun coordinator(service: ServiceCoordinator?) {
            coordinator = service
        }
        fun networking(configure: Networking.() -> Unit) {
            network.apply(configure)
        }
        internal fun build() = RockyConfiguration(directory, schema, secrets, identity, coordinator, network.transport.takeIf { network.enabled }, network.enabled && network.automaticSync)
    }

    class Networking {
        var transport: PeerTransport = PeerTransport.Iroh()
        var enabled: Boolean = true
        var automaticSync: Boolean = true
    }
}
