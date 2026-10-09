package org.example.examples.shopping.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Block
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.zacsweers.metrox.viewmodel.metroViewModel
import org.example.examples.shopping.SessionState

/** Revocation ends access; a fresh invitation creates a new identity while keeping the old copy. */
@Composable
fun RevokedScreen(model: SetupViewModel = metroViewModel()) {
    val revoked = model.state.collectAsState().value as? SessionState.Revoked
    Column(
        Modifier.fillMaxSize().safeContentPadding().verticalScroll(rememberScrollState()).padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Icon(Icons.Filled.Block, null, Modifier.size(48.dp), tint = MaterialTheme.colorScheme.error)
        Text("This device was revoked", style = MaterialTheme.typography.headlineMedium)
        Text("A trusted device removed your access to this household. You can no longer sync or receive new list keys.")
        Text(
            "Your old local copy stays saved. Ask a trusted device for a fresh pairing code to join again.",
            style = MaterialTheme.typography.bodyMedium,
        )
        revoked?.error?.let { Text(it.message ?: it.toString(), color = MaterialTheme.colorScheme.error) }
        JoinHouseholdCard(onJoin = model::rejoin, title = "Rejoin the network", joinLabel = "Join network")
    }
}
