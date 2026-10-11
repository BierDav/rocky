package org.example.examples.shopping.presentation.component.pairing

import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager

@Composable
actual fun PasteCode(label: String, onText: (String) -> Unit, modifier: Modifier) {
    @Suppress("DEPRECATION") // LocalClipboard needs a platform ClipEntry.
    val clipboard = LocalClipboardManager.current
    TextButton({ clipboard.getText()?.text?.let(onText) }, modifier) { Text(label) }
}
