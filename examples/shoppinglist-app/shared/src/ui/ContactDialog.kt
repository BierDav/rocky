package org.example.examples.shopping.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import org.example.examples.pairing.CodeScanner
import org.example.examples.pairing.PasteCode
import org.example.examples.pairing.canScanCodes

/** A contact is scanned on phones or pasted; its optional name stays local. */
@Composable
internal fun ContactDialog(title: String, onDismiss: () -> Unit, onContact: (String, String) -> Unit) {
    var code by remember { mutableStateOf("") }
    var label by remember { mutableStateOf("") }
    AlertDialog(
        onDismiss,
        confirmButton = { TextButton({ onContact(code, label) }, enabled = code.startsWith("rocky://peer/")) { Text("Use contact") } },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text(title) },
        text = {
            Column {
                if (canScanCodes) CodeScanner({ code = it }, Modifier.height(220.dp).fillMaxWidth())
                OutlinedTextField(code, { code = it }, label = { Text("Contact code") })
                PasteCode("Paste contact", { code = it })
                OutlinedTextField(label, { label = it }, label = { Text("Local label") })
            }
        },
    )
}
