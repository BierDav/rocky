package org.example.examples.shopping.presentation.screen.opening

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import kotlinx.serialization.Serializable

@Serializable
data object OpeningKey : NavKey

/** Shown while the session opens an existing household or enrolls a new one. */
@Composable
fun OpeningScreen() {
    Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        CircularProgressIndicator()
        Spacer(Modifier.height(16.dp))
        Text("Opening the household…", style = MaterialTheme.typography.bodyLarge)
        Text("Enrolling, installing authors and starting sync", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
