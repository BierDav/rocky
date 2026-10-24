package org.example.examples

import android.app.Application
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import org.example.examples.shopping.di.PlatformContext
import org.example.examples.shopping.di.ShoppingGraph
import org.example.examples.shopping.diagnostics.DIAGNOSTIC_BUILD
import org.example.examples.shopping.diagnostics.DiagnosticLog
import org.example.examples.shopping.diagnostics.observeDiagnostics
import rocky.android.RockyLifecycle
import java.io.File

/** Creates the app's graph once; the shared module binds the Android platform. */
class ShoppingApplication : Application() {
    lateinit var diagnosticLog: DiagnosticLog
        private set

    lateinit var appGraph: ShoppingGraph
        private set

    override fun onCreate() {
        super.onCreate()
        diagnosticLog = DiagnosticLog(File(filesDir, "diagnostics"), DIAGNOSTIC_BUILD, "android")
        val lifecycle = RockyLifecycle.register(this)
        appGraph = ShoppingGraph(PlatformContext(this, lifecycle) { diagnosticLog.write(it) })
        observeDiagnostics(appGraph.session, CoroutineScope(SupervisorJob() + Dispatchers.IO), diagnosticLog::write)
    }
}
