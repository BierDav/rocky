package org.example.examples.shopping.presentation.screen.pairing

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.serialization.Serializable
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.data.session.SessionState

/** A pairing link opened from outside, such as by a camera app; [link] is the `rocky-shopping://pair?…` code. */
@Serializable
data class PairingOfferKey(val link: String) : NavKey

/** Asks before joining: the user confirms the offered code, or this device already belongs to a household. */
@Composable
fun PairingOfferScreen(key: PairingOfferKey, onDone: () -> Unit, model: PairingOfferViewModel = metroViewModel()) {
    val state by model.state.collectAsState()
    val offer = remember(key) { runCatching { PairingCode.parse(key.link) }.getOrNull() }
    if (offer == null) {
        LaunchedEffect(Unit) { onDone() }
        return
    }
    val joinable = state == SessionState.Setup || state is SessionState.Revoked
    Surface(shape = AlertDialogDefaults.shape, color = AlertDialogDefaults.containerColor, tonalElevation = AlertDialogDefaults.TonalElevation) {
        Column(Modifier.padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text(if (joinable) "Join as ${offer.name.ifBlank { "this device" }}?" else "Already in a household", style = MaterialTheme.typography.headlineSmall)
            Text(
                if (joinable) {
                    "A trusted device invited this one as a ${offer.kind.label.lowercase()} device: ${offer.kind.summary.lowercase()}." +
                        (if (state is SessionState.Revoked) " The revoked copy is kept aside and this device gets a new identity." else "")
                } else {
                    "This device already belongs to a household. A pairing code only sets up a new or revoked device."
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            Row(Modifier.align(Alignment.End), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (joinable) {
                    TextButton(onDone) { Text("Cancel") }
                    Button({
                        model.accept(offer)
                        onDone()
                    }) { Text("Join") }
                } else {
                    TextButton(onDone) { Text("OK") }
                }
            }
        }
    }
}
