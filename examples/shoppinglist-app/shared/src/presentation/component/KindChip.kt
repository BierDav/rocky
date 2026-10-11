package org.example.examples.shopping.presentation.component

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.data.household.NodeKind

@Composable
internal fun KindChip(kind: NodeKind) {
    val color = when (kind) {
        NodeKind.Trusted -> MaterialTheme.colorScheme.primaryContainer
        NodeKind.Full -> MaterialTheme.colorScheme.secondaryContainer
        NodeKind.Partial -> MaterialTheme.colorScheme.tertiaryContainer
    }
    Surface(color = color, shape = MaterialTheme.shapes.small) {
        Text(kind.label, Modifier.padding(horizontal = 8.dp, vertical = 2.dp), style = MaterialTheme.typography.labelMedium)
    }
}
