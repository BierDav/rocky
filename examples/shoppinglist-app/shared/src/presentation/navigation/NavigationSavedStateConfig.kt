package org.example.examples.shopping.presentation.navigation

import androidx.navigation3.runtime.NavKey
import androidx.savedstate.serialization.SavedStateConfiguration
import kotlinx.serialization.modules.SerializersModule
import kotlinx.serialization.modules.polymorphic
import org.example.examples.shopping.presentation.screen.approvals.ApprovalsKey
import org.example.examples.shopping.presentation.screen.devices.DevicesKey
import org.example.examples.shopping.presentation.screen.history.HistoryKey
import org.example.examples.shopping.presentation.screen.lists.ListsKey
import org.example.examples.shopping.presentation.screen.opening.OpeningKey
import org.example.examples.shopping.presentation.screen.pairing.PairingOfferKey
import org.example.examples.shopping.presentation.screen.resetting.ResettingKey
import org.example.examples.shopping.presentation.screen.revoked.RevokedKey
import org.example.examples.shopping.presentation.screen.setup.SetupKey
import org.example.examples.shopping.presentation.screen.sync.SyncKey

/** Every destination, so iOS and desktop can save the back stack without reflection. */
val navigationSavedStateConfig = SavedStateConfiguration {
    serializersModule = SerializersModule {
        polymorphic(NavKey::class) {
            subclass(OpeningKey::class, OpeningKey.serializer())
            subclass(SetupKey::class, SetupKey.serializer())
            subclass(RevokedKey::class, RevokedKey.serializer())
            subclass(ResettingKey::class, ResettingKey.serializer())
            subclass(PairingOfferKey::class, PairingOfferKey.serializer())
            subclass(ListsKey::class, ListsKey.serializer())
            subclass(ApprovalsKey::class, ApprovalsKey.serializer())
            subclass(DevicesKey::class, DevicesKey.serializer())
            subclass(SyncKey::class, SyncKey.serializer())
            subclass(HistoryKey::class, HistoryKey.serializer())
        }
    }
}
