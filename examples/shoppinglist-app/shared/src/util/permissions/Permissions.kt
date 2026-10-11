package org.example.examples.shopping.util.permissions

import androidx.compose.runtime.Composable

/** Asks once for the runtime permissions direct peer routes need; sync still works through relays without them. */
@Composable
expect fun NetworkPermissions()
