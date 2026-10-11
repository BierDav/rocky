package org.example.examples.shopping.presentation.component

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.presentation.component.pairing.CodeScanner
import org.example.examples.shopping.presentation.component.pairing.PasteCode
import org.example.examples.shopping.presentation.component.pairing.canScanCodes

@Composable
internal fun JoinHouseholdCard(onJoin: (String) -> Unit, enabled: Boolean = true, title: String = "Join a household", joinLabel: String = "Join") {
    var scanning by remember { mutableStateOf(false) }
    var code by remember { mutableStateOf("") }
    ElevatedCard(Modifier.fillMaxWidth().widthIn(max = 520.dp)) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Icon(Icons.Filled.QrCodeScanner, null, tint = MaterialTheme.colorScheme.primary)
            Text(title, style = MaterialTheme.typography.titleLarge)
            Text(
                "On a trusted device open Devices → Add device, choose what this device should be, then scan its code.",
                style = MaterialTheme.typography.bodyMedium,
            )
            if (canScanCodes) {
                if (scanning) {
                    CodeScanner(onCode = {
                        scanning = false
                        onJoin(it)
                    }, modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(16.dp)))
                    TextButton(onClick = { scanning = false }) { Text("Cancel scanning") }
                } else {
                    FilledTonalButton(onClick = { scanning = true }, enabled = enabled) { Text("Scan QR code") }
                }
            }
            OutlinedTextField(code, { code = it }, Modifier.fillMaxWidth(), label = { Text("…or paste the code") }, singleLine = true)
            PasteCode("Paste code", { code = it })
            Button(onClick = { onJoin(code) }, enabled = enabled && code.isNotBlank()) { Text(joinLabel) }
        }
    }
}
