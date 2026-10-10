package rocky.room.qualification

import rocky.AuthorPseudonym
import rocky.RockyDatabase

/** Installs the table pseudonyms this device inserts under, assigned by [issuer]: itself on the creator. */
suspend fun RockyDatabase.installAuthors(issuer: RockyDatabase = this, tables: List<String> = listOf("lists", "items")): List<AuthorPseudonym> = tables.map { credentials.installPseudonym(issuer.credentials.assignPseudonym(credentials.pseudonymRequest(it))) }

/** The native installer visits table names in order: items gets 1 and lists gets 2. */
class Ids {
    private var lists = 0L
    private var items = 0L
    fun list(): Long = (2L shl 32) or ++lists
    fun item(): Long = (1L shl 32) or ++items
}
