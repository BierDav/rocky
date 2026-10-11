package org.example.examples.shopping.presentation.host

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.presentation.theme.BrandColor
import org.example.examples.shopping.presentation.theme.BrandLogo

// The desktop has no system launch screen, so the window starts with the same one the phones show.
@Composable
actual fun LaunchScreen(visible: Boolean) {
    AnimatedVisibility(visible, exit = fadeOut()) {
        Box(Modifier.fillMaxSize().background(BrandColor), contentAlignment = Alignment.Center) {
            Image(BrandLogo, null, Modifier.size(96.dp))
        }
    }
}
