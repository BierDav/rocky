package org.example.examples.shopping.presentation.component

import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.staticCompositionLocalOf
import kotlinx.coroutines.flow.Flow

/** The snackbar of the surrounding screen, such as home's, where a nested screen reports its messages. */
val LocalSnackbarHostState = staticCompositionLocalOf { SnackbarHostState() }

/** Shows each of [messages] in [LocalSnackbarHostState] while this screen is visible. */
@Composable
fun ShowMessages(messages: Flow<String>) {
    val snackbar = LocalSnackbarHostState.current
    LaunchedEffect(messages, snackbar) { messages.collect { snackbar.showSnackbar(it) } }
}
