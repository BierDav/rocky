package org.example.examples.shopping.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import qrscanner.CameraLens
import qrscanner.QrScanner
import java.util.concurrent.atomic.AtomicBoolean

actual val canScanCodes: Boolean = true

@Composable
actual fun CodeScanner(onCode: (String) -> Unit, modifier: Modifier) {
    // The camera keeps reporting the code it sees; only the first one counts.
    val reported = remember { AtomicBoolean(false) }
    QrScanner(
        modifier = modifier,
        flashlightOn = false,
        cameraLens = CameraLens.Back,
        openImagePicker = false,
        onCompletion = { if (reported.compareAndSet(false, true)) onCode(it) },
        imagePickerHandler = {},
        onFailure = {},
    )
}
