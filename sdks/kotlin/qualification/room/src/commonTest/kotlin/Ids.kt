package rocky.room.qualification

/** Each table has a creation sequence under the creator node's author number. */
class Ids {
    private var lists = 0L
    private var items = 0L
    fun list(): Long = (1L shl 32) or ++lists
    fun item(): Long = (1L shl 32) or ++items
}
