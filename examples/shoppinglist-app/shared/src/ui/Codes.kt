package org.example.examples.shopping.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import qrgenerator.qrkitpainter.rememberQrKitPainter

/** A QR code of [text], dark on white so any camera reads it. */
@Composable
fun QrCode(text: String, modifier: Modifier = Modifier) {
    Image(rememberQrKitPainter(text), "Pairing code", modifier.clip(RoundedCornerShape(12.dp)).background(Color.White).padding(12.dp))
}

/** Whether this platform scans codes with a camera; elsewhere a code is pasted. */
expect val canScanCodes: Boolean

/** A camera view that reports the first code it reads. */
@Composable
expect fun CodeScanner(onCode: (String) -> Unit, modifier: Modifier = Modifier)
