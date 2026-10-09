package org.example.examples.shopping

import rocky.AuthorPseudonym
import rocky.RockyDatabase

/** The tables a shopping device inserts into. */
val ShoppingTables = listOf("lists", "categories", "items")

/**
 * Installs this device's authors for [ShoppingTables], assigned by [issuer]: itself when it issues. Rocky signs the
 * ids a device inserts under them; reopening restores them.
 */
suspend fun RockyDatabase.installShoppingAuthor(issuer: RockyDatabase = this): List<AuthorPseudonym> =
    ShoppingTables.map { credentials.installPseudonym(issuer.credentials.assignPseudonym(credentials.pseudonymRequest(it))) }
