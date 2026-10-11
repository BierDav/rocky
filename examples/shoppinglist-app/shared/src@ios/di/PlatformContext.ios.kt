package org.example.examples.shopping.di

import rocky.ios.RockyLifecycle

/** The app's scene lifecycle, which the app also feeds with scene visibility. */
actual class PlatformContext(val lifecycle: RockyLifecycle)
