package org.example.examples.shopping.presentation.navigation

import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.receiveAsFlow
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.presentation.screen.pairing.PairingOfferKey

/**
 * The app's deep links: every platform hands incoming `rocky-shopping://` URLs to [open], and the app shows each one's
 * destination. Links opened before the app shows anything wait for it.
 */
@SingleIn(AppScope::class)
@Inject
class DeepLinks {
    private val opened = Channel<String>(Channel.UNLIMITED)

    fun open(url: String) {
        opened.trySend(url)
    }

    /** Each opened link's destination, once; unknown and malformed links have none. */
    val destinations: Flow<NavKey> = opened.receiveAsFlow().mapNotNull(::destinationOf)
}

/** `rocky-shopping://pair?…` asks to join with the code, since any app or page may open one. */
fun destinationOf(url: String): NavKey? = runCatching { PairingCode.parse(url) }.getOrNull()?.let { PairingOfferKey(it.toString()) }
