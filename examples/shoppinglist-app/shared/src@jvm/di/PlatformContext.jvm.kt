package org.example.examples.shopping.di

import java.io.File
import java.net.InetAddress

/** Desktop: the household lives in `ROCKY_SHOPPING_DATA`, else `local/.rocky-shopping`, and is named after the host. */
actual class PlatformContext(
    val directory: File = File(System.getenv("ROCKY_SHOPPING_DATA") ?: File("local/.rocky-shopping").path),
    val deviceName: String = runCatching { InetAddress.getLocalHost().hostName.substringBefore('.') }.getOrDefault("Desktop"),
    val diagnostic: (String) -> Unit = {},
)
