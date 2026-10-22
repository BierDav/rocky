package org.example.examples.shopping.qualification

import org.example.examples.shopping.data.db.ShoppingSchema
import rocky.Schema

/** The app version this build ships; a device process may also run an older app's schema. */
internal const val CURRENT_APP = 7

internal fun appSchema(app: Int): Schema = when (app) {
    CURRENT_APP -> ShoppingSchema
    else -> error("no app version $app")
}
