package org.example.examples.shopping.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/** A desktop shows codes; it is joined by pasting one. */
actual val canScanCodes: Boolean = false

@Composable
actual fun CodeScanner(onCode: (String) -> Unit, modifier: Modifier) {}
