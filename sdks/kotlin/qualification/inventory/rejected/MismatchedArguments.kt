package rocky.rejected

import rocky.*

/** Must not compile: `Reserve` takes `ReserveArguments`, not another command's. */
suspend fun mismatchedArguments(commands: CommandPort): Ticket<Remaining, Shortfall> = commands.submit(Reserve, RestockArguments("bolt", 1), "00".repeat(32))
