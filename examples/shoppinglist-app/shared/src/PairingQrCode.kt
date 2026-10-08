package org.example.examples.shopping

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import qrgenerator.qrkitpainter.rememberQrKitPainter
import rocky.PairingInvitation

/** QRKit encodes the exact URI accepted by RockyDatabase.join. */
@Composable
fun PairingQrCode(invitation: PairingInvitation, modifier: Modifier = Modifier) {
    Image(rememberQrKitPainter(invitation.uri), "Pair with this shopping-list domain",
        modifier.background(Color.White).padding(16.dp))
}
