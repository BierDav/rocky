package rocky.rejected

import rocky.*

/** Must not compile: a `Reserve` rejection is a `Shortfall`, not `Unit`. */
suspend fun mismatchedResult(commands: CommandPort): Ticket<Remaining, Unit>? = commands.ticket(Reserve, "00".repeat(32))
