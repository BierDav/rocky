package org.example.examples.shopping.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AddHome
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import org.example.examples.pairing.CodeScanner
import org.example.examples.pairing.PasteCode
import org.example.examples.pairing.canScanCodes
import org.example.examples.shopping.SessionState
import org.example.examples.shopping.ShoppingSession

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class)
class SetupViewModel(private val session: ShoppingSession) : ViewModel() {
    val state = session.state
    fun create() = session.create()
    fun join(code: String) = session.join(code)
    fun startOver() = session.startOver()
    fun retry() = session.retry()
    fun rejoin(code: String) = session.rejoin(code)
}

/** Start a household on this device, or join one by scanning a trusted device's code. */
@Composable
fun SetupScreen(model: SetupViewModel = metroViewModel()) {
    val failed = model.state.collectAsState().value as? SessionState.Failed
    Column(
        Modifier.fillMaxSize().safeContentPadding().verticalScroll(rememberScrollState()).padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Spacer(Modifier.height(24.dp))
        Text("Shopping", style = MaterialTheme.typography.displaySmall, color = MaterialTheme.colorScheme.primary)
        Text(
            "Lists that sync directly between your devices, peer to peer. No server holds them.",
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
        )
        failed?.let { failure ->
            Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer)) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(if (failure.existing) "Could not open the household" else "Setting up failed", style = MaterialTheme.typography.titleMedium)
                    Text(failure.error.message ?: failure.error.toString(), style = MaterialTheme.typography.bodySmall)
                    Row {
                        TextButton(onClick = model::retry) { Text("Retry") }
                        if (!failure.existing) TextButton(onClick = model::startOver) { Text("Start over") }
                    }
                }
            }
        }
        if (failed?.existing == true) return@Column
        ElevatedCard(Modifier.fillMaxWidth().widthIn(max = 520.dp)) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Filled.AddHome, null, tint = MaterialTheme.colorScheme.primary)
                Text("Start a household", style = MaterialTheme.typography.titleLarge)
                Text("This device becomes trusted: it creates lists and adds your other devices.", style = MaterialTheme.typography.bodyMedium)
                Button(onClick = model::create, enabled = failed == null) { Text("Start") }
            }
        }
        JoinHouseholdCard(onJoin = model::join, enabled = failed == null)
    }
}

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
