package org.example.examples.shopping.presentation.component

import androidx.compose.foundation.layout.*
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.zacsweers.metrox.viewmodel.metroViewModel
import org.example.examples.shopping.presentation.navigation.LocalNavigator
import org.example.examples.shopping.presentation.screen.sync.SyncKey

/** A tab's screen under its top bar, whose sync badge opens the Sync tab. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TabTopBar(title: String, badge: SyncBadgeViewModel = metroViewModel(), content: @Composable () -> Unit) {
    val navigator = LocalNavigator.current
    val state by badge.badge.collectAsState()
    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            title = { Text(title) },
            actions = {
                SyncBadge(state, onClick = { navigator.tab = SyncKey })
                Spacer(Modifier.width(8.dp))
            },
        )
        Box(Modifier.weight(1f).fillMaxWidth()) { content() }
    }
}
