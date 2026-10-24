package org.example.examples.shopping.di

import android.os.Build
import android.util.Log
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
import java.io.File

/** Android: keys in KSafe, sync paused while no activity is visible. */
@BindingContainer
@ContributesTo(AppScope::class)
object AndroidPlatformBindings {
    @Provides
    @SingleIn(AppScope::class)
    fun platform(context: PlatformContext): ShoppingPlatform {
        val application = context.application
        val lifecycle = context.lifecycle
        val visible = MutableStateFlow(lifecycle.isForeground)
        val secrets = SafeSecretStore(KSafe(application, fileName = SafeSecretStore.FILE_NAME))
        return ShoppingPlatform(
            directory = File(application.filesDir, "shopping").absolutePath,
            deviceName = Build.MODEL,
            diagnostic = context.diagnostic,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(application, path) },
            secrets = { secrets },
            coordinator = { publicCoordination(File(application.filesDir, "shopping").absolutePath) },
            opened = { database ->
                lifecycle.follow(database) { foreground ->
                    visible.value = foreground
                    Log.i(TAG, if (foreground) "sync foreground" else "sync background")
                }
            },
            closed = lifecycle::unfollow,
            foreground = visible,
            erase = secrets::clear,
        )
    }

    private const val TAG = "RockyShopping"
}
