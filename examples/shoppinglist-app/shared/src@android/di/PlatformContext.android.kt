package org.example.examples.shopping.di

import android.app.Application
import rocky.android.RockyLifecycle

actual class PlatformContext(val application: Application, val lifecycle: RockyLifecycle)
