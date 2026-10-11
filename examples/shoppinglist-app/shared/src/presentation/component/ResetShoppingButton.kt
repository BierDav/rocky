package org.example.examples.shopping.presentation.component

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*

/** Removes this device's household after confirmation; other devices keep their copies. */
@Composable
fun ResetShoppingButton(onReset: () -> Unit, enabled: Boolean = true) {
    var confirming by remember { mutableStateOf(false) }
    TextButton({ confirming = true }, enabled = enabled) { Text("Reset Shopping") }
    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text("Reset Shopping on this device?") },
            text = { Text("Remove local lists and household setup. Unsynced changes will be lost. Other devices keep their copies.") },
            confirmButton = {
                TextButton({
                    confirming = false
                    onReset()
                }) { Text("Reset") }
            },
            dismissButton = { TextButton({ confirming = false }) { Text("Cancel") } },
        )
    }
}
