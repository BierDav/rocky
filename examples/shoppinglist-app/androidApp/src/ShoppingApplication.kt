package org.example.examples

import android.app.Application
import org.example.examples.shopping.di.PlatformContext
import org.example.examples.shopping.di.ShoppingGraph
import rocky.android.RockyLifecycle

/** Creates the app's graph once; the shared module binds the Android platform. */
class ShoppingApplication : Application() {
    lateinit var appGraph: ShoppingGraph
        private set

    override fun onCreate() {
        super.onCreate()
        val lifecycle = RockyLifecycle.register(this)
        appGraph = ShoppingGraph(PlatformContext(this, lifecycle))
    }
}
