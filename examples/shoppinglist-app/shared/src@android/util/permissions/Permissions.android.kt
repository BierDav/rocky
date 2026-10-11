package org.example.examples.shopping.util.permissions

import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalContext

/** Android 17 gates LAN peers, such as direct TCP, behind a runtime permission. */
private const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"

@Composable
actual fun NetworkPermissions() {
    val context = LocalContext.current
    // A denial still works: iroh reaches peers through relays, only direct LAN routes are refused.
    val request = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= 37 && context.checkSelfPermission(LOCAL_NETWORK) != PackageManager.PERMISSION_GRANTED) {
            request.launch(LOCAL_NETWORK)
        }
    }
}
