package org.example.examples.market.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import org.example.examples.market.Market
import org.example.examples.market.MarketNode
import org.example.examples.market.Opening
import org.example.examples.pairing.CodeScanner
import org.example.examples.pairing.PasteCode
import org.example.examples.pairing.QrCode
import org.example.examples.pairing.canScanCodes

/** Setup and pairing shared by native consumers; files, keys and opening stay with the platform. */
@Composable
fun MarketApp(node: MarketNode?, message: String, foreground: Boolean, busy: Boolean = false, reset: (() -> Unit)? = null, open: (String, Opening) -> Unit) {
    var bundle by remember { mutableStateOf("") }
    var invitation by remember { mutableStateOf("") }
    var directAddress by remember { mutableStateOf("") }
    var scanning by remember { mutableStateOf(false) }
    var pairing by remember { mutableStateOf<String?>(null) }
    var resetting by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        if (node == null) {
            Column(Modifier.weight(1f).padding(16.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("Market", style = MaterialTheme.typography.headlineSmall)
                OutlinedTextField(bundle, { bundle = it }, label = { Text("Coordination bundle") }, maxLines = 3)
                PasteCode("Paste bundle", { bundle = it })
                OutlinedTextField(invitation, { invitation = it }, label = { Text("Pairing invitation") }, maxLines = 3)
                PasteCode("Paste invitation", { invitation = it })
                OutlinedTextField(directAddress, { directAddress = it }, label = { Text("Direct pairing address (optional)") }, singleLine = true)
                if (canScanCodes) {
                    if (scanning) {
                        CodeScanner({
                            invitation = it
                            scanning = false
                        }, Modifier.fillMaxWidth().aspectRatio(1f))
                    } else {
                        TextButton({ scanning = true }, enabled = !busy) { Text("Scan QR code") }
                    }
                }
                Row {
                    Button({ open(bundle, Opening.Create) }, enabled = !busy && bundle.isNotBlank()) { Text("Create market") }
                    Button({ open(bundle, Opening.Join(invitation, directAddress)) }, enabled = !busy && bundle.isNotBlank() && invitation.isNotBlank()) { Text("Join market") }
                }
            }
        } else {
            if (node.rocky.isIssuer) TextButton({ scope.launch { pairing = node.rocky.pairing(Market.Buyer).uri } }) { Text("Pair buyer") }
            Box(Modifier.weight(1f)) { MarketScreen(node.market, foreground) }
        }
        if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (message.isNotBlank()) Text(message, Modifier.padding(16.dp).testTag("market-status"))
        reset?.let { TextButton({ resetting = true }, enabled = !busy) { Text("Reset market") } }
    }
    if (resetting) {
        AlertDialog(
            onDismissRequest = { resetting = false },
            title = { Text("Reset this device's market?") },
            text = { Text("Remove local market data and return to setup to create or join a market. Other devices keep their data.") },
            confirmButton = {
                TextButton({
                    resetting = false
                    pairing = null
                    invitation = ""
                    directAddress = ""
                    scanning = false
                    reset?.invoke()
                }) { Text("Reset") }
            },
            dismissButton = { TextButton({ resetting = false }) { Text("Cancel") } },
        )
    }
    pairing?.let { code ->
        AlertDialog(
            onDismissRequest = { pairing = null },
            title = { Text("Pair a buyer") },
            text = {
                Column {
                    QrCode(code, Modifier.fillMaxWidth().aspectRatio(1f))
                    Text(code, Modifier.testTag("pairing-invitation"), maxLines = 2)
                }
            },
            confirmButton = { TextButton({ pairing = null }) { Text("Done") } },
        )
    }
}
