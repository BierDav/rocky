package org.example.examples.shopping.presentation.component.pairing

import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import org.publicvalue.multiplatform.qrcode.CameraPosition
import org.publicvalue.multiplatform.qrcode.CodeType
import org.publicvalue.multiplatform.qrcode.ScannerWithPermissions

@Composable
actual fun CodeScanner(onCode: (String) -> Unit, modifier: Modifier) {
    var reported by remember { mutableStateOf(false) }
    val receive by rememberUpdatedState(onCode)
    val scope = rememberCoroutineScope()
    ScannerWithPermissions(
        modifier = modifier.clipToBounds(),
        onScanned = { code ->
            scope.launch(Dispatchers.Main.immediate) {
                if (!reported) {
                    reported = true
                    receive(code)
                }
            }
            true
        },
        types = listOf(CodeType.QR),
        cameraPosition = CameraPosition.BACK,
        enableTorch = false,
        permissionDeniedContent = { Text("Camera access denied. Paste the pairing code instead.", modifier) },
    )
}
