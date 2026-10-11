package org.example.examples

import android.app.Application
import org.example.examples.shopping.di.PlatformContext
import org.example.examples.shopping.di.ShoppingGraph

/** Creates the app's graph once; the shared module binds the Android platform. */
class ShoppingApplication : Application() {
    lateinit var appGraph: ShoppingGraph
        private set

    override fun onCreate() {
        super.onCreate()
        appGraph = ShoppingGraph(PlatformContext(this))
    }
}
