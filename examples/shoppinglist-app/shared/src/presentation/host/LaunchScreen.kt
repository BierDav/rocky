package org.example.examples.shopping.presentation.host

import androidx.compose.runtime.Composable

/** Covers the app with the platform's launch screen while [visible], then fades it out; Android's system splash needs none. */
@Composable
expect fun LaunchScreen(visible: Boolean)
