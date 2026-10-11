package org.example.examples.shopping.presentation.component.pairing

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import io.github.alexzhirkevich.qrose.rememberQrCodePainter

/** A QR code of [text], dark on white so any camera reads it. */
@Composable
fun QrCode(text: String, modifier: Modifier = Modifier) {
    Image(rememberQrCodePainter(text), "Pairing code", modifier.clip(RoundedCornerShape(12.dp)).background(Color.White).padding(12.dp))
}

/** A pairing code with its QR representation, short preview, and copy action. */
@Composable
fun InvitationCode(code: String, detail: String, modifier: Modifier = Modifier) {
    @Suppress("DEPRECATION") // LocalClipboard needs a platform ClipEntry.
    val clipboard = LocalClipboardManager.current
    Column(modifier) {
        QrCode(code, Modifier.size(280.dp).align(Alignment.CenterHorizontally))
        Text(detail, style = MaterialTheme.typography.bodySmall)
        SelectionContainer {
            Text(code.take(48) + "…", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
        }
        TextButton({ clipboard.setText(AnnotatedString(code)) }) { Text("Copy code") }
    }
}

/** Whether this platform scans codes with a camera; elsewhere a code is pasted. */
expect val canScanCodes: Boolean

/** A camera view that reports the first code it reads. */
@Composable
expect fun CodeScanner(onCode: (String) -> Unit, modifier: Modifier = Modifier)

/** User-initiated plain-text paste; iOS uses its system paste control. */
@Composable
expect fun PasteCode(label: String, onText: (String) -> Unit, modifier: Modifier = Modifier)
