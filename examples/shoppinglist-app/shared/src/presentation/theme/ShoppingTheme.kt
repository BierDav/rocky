package org.example.examples.shopping.presentation.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import com.materialkolor.rememberDynamicColorScheme

/** Material 3 colours generated from [BrandColor], light or dark with the system. */
@Composable
fun ShoppingTheme(content: @Composable () -> Unit) {
    val colors = rememberDynamicColorScheme(seedColor = BrandColor, isDark = isSystemInDarkTheme())
    MaterialTheme(colorScheme = colors, content = content)
}
