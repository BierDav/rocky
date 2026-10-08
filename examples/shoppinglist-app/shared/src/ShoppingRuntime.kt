package org.example.examples.shopping

import rocky.AuthorPseudonym
import rocky.RockyDatabase

/**
 * Installs this device's author for `shopping_items`, assigned by [issuer]: itself when it issues. Rocky signs the
 * ids a device inserts under it; reopening restores it.
 */
suspend fun RockyDatabase.installShoppingAuthor(issuer: RockyDatabase = this): AuthorPseudonym =
    credentials.installPseudonym(issuer.credentials.assignPseudonym(credentials.pseudonymRequest("shopping_items")))
