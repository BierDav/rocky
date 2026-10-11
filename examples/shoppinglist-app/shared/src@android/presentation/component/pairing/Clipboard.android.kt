package org.example.examples.shopping.presentation.component.pairing

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import klip.Clipboard
import klip.SystemClipboard

@Composable
internal actual fun rememberClipboard(): Clipboard {
    val context = LocalContext.current
    return remember(context) { SystemClipboard(context) }
}
