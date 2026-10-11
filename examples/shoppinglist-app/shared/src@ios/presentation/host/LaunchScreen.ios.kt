package org.example.examples.shopping.presentation.host

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.*
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.UIKitInteropInteractionMode
import androidx.compose.ui.viewinterop.UIKitInteropProperties
import androidx.compose.ui.viewinterop.UIKitView
import platform.UIKit.UIStoryboard
import platform.UIKit.UIView

// The system shows `LaunchScreen.storyboard` until Compose draws; the same view stays on top until it fades out.
@OptIn(ExperimentalComposeUiApi::class)
@Composable
actual fun LaunchScreen(visible: Boolean) {
    var rendered by remember { mutableStateOf(true) }
    val view = remember { UIStoryboard.storyboardWithName("LaunchScreen", null).instantiateInitialViewController()?.view }
    if (!rendered || view == null) return
    UIKitView(
        factory = { view },
        modifier = Modifier.fillMaxSize(),
        properties = UIKitInteropProperties(interactionMode = UIKitInteropInteractionMode.NonCooperative, placedAsOverlay = true),
        update = { launch ->
            UIView.animateWithDuration(
                duration = 0.3,
                animations = { launch.alpha = if (visible) 1.0 else 0.0 },
                completion = { if (!visible) rendered = false },
            )
        },
    )
}
