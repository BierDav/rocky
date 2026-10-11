package org.example.examples.shopping.presentation.component.pairing

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import klip.Clipboard
import klip.SystemClipboard

@Composable
internal actual fun rememberClipboard(): Clipboard = remember { SystemClipboard() }
