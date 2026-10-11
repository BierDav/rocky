package org.example.examples.pairing

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import io.github.alexzhirkevich.qrose.rememberQrCodePainter

/** A QR code of [text], dark on white so any camera reads it. */
@Composable
fun QrCode(text: String, modifier: Modifier = Modifier) {
    Image(rememberQrCodePainter(text), "Pairing code", modifier.clip(RoundedCornerShape(12.dp)).background(Color.White).padding(12.dp))
}

/** Whether this platform scans codes with a camera; elsewhere a code is pasted. */
expect val canScanCodes: Boolean

/** A camera view that reports the first code it reads. */
@Composable
expect fun CodeScanner(onCode: (String) -> Unit, modifier: Modifier = Modifier)

/** User-initiated plain-text paste; iOS uses its system paste control. */
@Composable
expect fun PasteCode(label: String, onText: (String) -> Unit, modifier: Modifier = Modifier)
