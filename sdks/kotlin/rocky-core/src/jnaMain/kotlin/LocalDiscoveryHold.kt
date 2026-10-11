package rocky

/**
 * Holds a platform resource exactly while local-network discovery runs, such as Android's Wi-Fi multicast
 * lock, without which the radio filters the announcements of other devices.
 */
class LocalDiscoveryHold(private val acquire: () -> Unit, private val release: () -> Unit) {
    private var held = false

    /** Whether the resource is held now. */
    val isHeld: Boolean @Synchronized get() = held

    /** Acquires while the [status]'s local path is on and releases otherwise; `null` releases. Safe to repeat. */
    @Synchronized
    fun apply(status: DiscoveryStatus?) {
        val wanted = status?.local?.state == DiscoveryPathState.On
        if (wanted == held) return
        if (wanted) acquire() else release()
        held = wanted
    }
}
