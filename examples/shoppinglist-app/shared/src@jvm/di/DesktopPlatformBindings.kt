package org.example.examples.shopping.di

import androidx.room.Room
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.BindingContainer
import dev.zacsweers.metro.ContributesTo
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import org.example.examples.shopping.data.coordination.publicCoordination
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.session.ShoppingPlatform

/** Desktop: everything, keys included, lives in the household directory. */
@BindingContainer
@ContributesTo(AppScope::class)
object DesktopPlatformBindings {
    @Provides
    @SingleIn(AppScope::class)
    fun platform(context: PlatformContext): ShoppingPlatform = ShoppingPlatform(
        context.directory.absolutePath,
        context.deviceName,
        diagnostic = context.diagnostic,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        coordinator = { publicCoordination(context.directory.absolutePath) },
    )
}
