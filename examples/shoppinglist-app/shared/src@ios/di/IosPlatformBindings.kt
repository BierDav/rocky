package org.example.examples.shopping.di

import androidx.room.Room
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.BindingContainer
import dev.zacsweers.metro.ContributesTo
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.flow.MutableStateFlow
import org.example.examples.shopping.data.coordination.publicCoordination
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.session.ShoppingPlatform
import platform.Foundation.NSApplicationSupportDirectory
import platform.Foundation.NSSearchPathForDirectoriesInDomains
import platform.Foundation.NSUUID
import platform.Foundation.NSUserDefaults
import platform.Foundation.NSUserDomainMask
import platform.UIKit.UIDevice
import rocky.PeerTransport
import rocky.ios.KeychainSecretStore

/** iOS: keys in the Keychain, sync following the scene lifecycle. */
@BindingContainer
@ContributesTo(AppScope::class)
object IosPlatformBindings {
    @Provides
    @SingleIn(AppScope::class)
    fun platform(context: PlatformContext): ShoppingPlatform {
        val lifecycle = context.lifecycle
        val visible = MutableStateFlow(lifecycle.isForeground)
        val defaults = NSUserDefaults.standardUserDefaults
        return ShoppingPlatform(
            directory = (NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory, NSUserDomainMask, true).first() as String) + "/shopping",
            deviceName = UIDevice.currentDevice.name,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(name = path) },
            // Reinstalling removes defaults, so a new installation cannot inherit an old owner's keys.
            secrets = {
                val installation = defaults.stringForKey(INSTALLATION) ?: NSUUID().UUIDString.also { defaults.setObject(it, INSTALLATION) }
                KeychainSecretStore("org.example.rockyshopping.$installation")
            },
            transport = { PeerTransport.Iroh(local = false) },
            commands = ::publicCoordination,
            opened = { database -> lifecycle.follow(database, onApplied = { visible.value = it }, onError = { println("Shopping lifecycle: ${it.message}") }) },
            closed = lifecycle::unfollow,
            // A new Keychain namespace gives the next installation fresh keys.
            erase = { defaults.setObject(NSUUID().UUIDString, INSTALLATION) },
            foreground = visible,
        )
    }

    private const val INSTALLATION = "keychain-installation"
}
