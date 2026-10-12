package org.example.examples.shopping.di

import android.content.Context
import android.os.Build
import android.util.Log
import androidx.room.Room
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.BindingContainer
import dev.zacsweers.metro.ContributesTo
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.withContext
import org.example.examples.shopping.data.coordination.publicCoordination
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.session.ShoppingPlatform
import rocky.android.KeystoreSecretStore
import java.io.File
import java.util.UUID

/** Android: keys in the Keystore, sync paused while no activity is visible. */
@BindingContainer
@ContributesTo(AppScope::class)
object AndroidPlatformBindings {
    @Provides
    @SingleIn(AppScope::class)
    fun platform(context: PlatformContext): ShoppingPlatform {
        val application = context.application
        val lifecycle = context.lifecycle
        val visible = MutableStateFlow(lifecycle.isForeground)
        val preferences = application.getSharedPreferences("shopping", Context.MODE_PRIVATE)
        return ShoppingPlatform(
            directory = File(application.filesDir, "shopping").absolutePath,
            deviceName = Build.MODEL,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(application, path) },
            secrets = { KeystoreSecretStore(application, preferences.getString(SECRETS_ALIAS, null) ?: KeystoreSecretStore.DEFAULT_ALIAS) },
            commands = ::publicCoordination,
            opened = { database ->
                lifecycle.follow(database) { foreground ->
                    visible.value = foreground
                    Log.i(TAG, if (foreground) "sync foreground" else "sync background")
                }
            },
            closed = lifecycle::unfollow,
            foreground = visible,
            // Fresh credentials: the old secrets go, and the next installation uses a new Keystore alias.
            erase = {
                withContext(Dispatchers.IO) {
                    val secrets = File(application.noBackupFilesDir, "rocky-secrets")
                    check(!secrets.exists() || secrets.deleteRecursively()) { "Could not remove local Shopping secrets" }
                    check(preferences.edit().putString(SECRETS_ALIAS, "rocky.shopping.${UUID.randomUUID()}").commit()) { "Could not save the new installation" }
                }
            },
        )
    }

    private const val TAG = "RockyShopping"
    private const val SECRETS_ALIAS = "secrets-alias"
}
