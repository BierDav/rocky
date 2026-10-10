package org.example.examples.market

import androidx.room.RoomDatabase
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import rocky.*
import rocky.room.RockyRoom
import rocky.room.buildOnRocky

/** How a device takes part when it opens. */
sealed interface Opening {
    /** A new market with this device as its owner, first issuer and executor. */
    data object Create : Opening

    /** Joins with an invitation from an issuer. */
    data class Join(val invitation: String, val directAddress: String? = null) : Opening

    /** Opens the existing installation. */
    data object Resume : Opening
}

/**
 * What differs per platform: where files and secrets live, how Rocky reaches peers and how Room is built. Paths stay
 * with the platform, so this module needs no platform file API.
 */
class MarketPlatform(
    /** The coordination service's serialized trust bundle. */
    val trustBundle: String,
    /** Opens or creates Rocky's database for [Opening] with the given command runtime. */
    val open: suspend (Opening, CommandRuntime) -> RockyDatabase,
    /** Room on the opened database's file. */
    val room: () -> RoomDatabase.Builder<MarketDatabase>,
    /** Keeps the owner's tenant key and pending controls. */
    val secrets: SecretStore,
    /** Bytes from a cryptographically secure source, for the tenant key. */
    val secureRandom: (Int) -> ByteArray,
)

/** One open installation: Rocky and Room on its file. Close it to release both. */
class MarketNode private constructor(private val room: RockyRoom<MarketDatabase>, val market: Market) {
    val rocky: RockyDatabase get() = room.rocky

    suspend fun close() = withContext(NonCancellable) {
        try {
            room.close()
        } finally {
            room.rocky.close()
        }
    }

    companion object {
        /**
         * Opens a device. Every device composes the handlers; only those the owner names as executors and whose
         * credentials hold `Execute` decide commands.
         */
        suspend fun open(platform: MarketPlatform, opening: Opening): MarketNode {
            val routed = if (opening is Opening.Join && !opening.directAddress.isNullOrBlank()) {
                Opening.Join(directInvitation(opening.invitation, opening.directAddress))
            } else {
                opening
            }
            val rocky = platform.open(routed, CommandRuntime(ServiceCoordinator(platform.trustBundle), MarketHandlers))
            return try {
                val room = platform.room().buildOnRocky(rocky)
                MarketNode(room, Market(rocky, room.room, platform.secrets, platform.secureRandom))
            } catch (error: Throwable) {
                rocky.close()
                throw error
            }
        }
    }
}
