package org.example.examples.shopping.presentation.screen.setup

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AddHome
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.serialization.Serializable
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.presentation.component.JoinHouseholdCard
import org.example.examples.shopping.presentation.component.ResetShoppingButton

@Serializable
data object SetupKey : NavKey

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
        if (failed?.existing != true) {
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
        ResetShoppingButton(model::reset)
    }
}
