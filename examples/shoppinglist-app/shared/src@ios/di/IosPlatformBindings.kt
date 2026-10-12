package org.example.examples.shopping.di

import androidx.room.Room
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.BindingContainer
import dev.zacsweers.metro.ContributesTo
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import eu.anifantakis.lib.ksafe.KSafe
import kotlinx.coroutines.flow.MutableStateFlow
import org.example.examples.shopping.data.coordination.publicCoordination
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.session.SafeSecretStore
import org.example.examples.shopping.data.session.ShoppingPlatform
import platform.Foundation.NSApplicationSupportDirectory
import platform.Foundation.NSSearchPathForDirectoriesInDomains
import platform.Foundation.NSUserDomainMask
import platform.UIKit.UIDevice
import rocky.PeerTransport

/** iOS: keys in KSafe, sync following the scene lifecycle. */
@BindingContainer
@ContributesTo(AppScope::class)
object IosPlatformBindings {
    @Provides
    @SingleIn(AppScope::class)
    fun platform(context: PlatformContext): ShoppingPlatform {
        val lifecycle = context.lifecycle
        val visible = MutableStateFlow(lifecycle.isForeground)
        val secrets = SafeSecretStore(KSafe(fileName = SafeSecretStore.FILE_NAME))
        return ShoppingPlatform(
            directory = (NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory, NSUserDomainMask, true).first() as String) + "/shopping",
            deviceName = UIDevice.currentDevice.name,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(name = path) },
            secrets = { secrets },
            transport = { PeerTransport.Iroh(local = false) },
            commands = ::publicCoordination,
            opened = { database -> lifecycle.follow(database, onApplied = { visible.value = it }, onError = { println("Shopping lifecycle: ${it.message}") }) },
            closed = lifecycle::unfollow,
            erase = secrets::clear,
            foreground = visible,
        )
    }
}
