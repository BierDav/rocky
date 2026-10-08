package org.example.examples.shopping

import rocky.*

/** Must not compile: a `Reserve` handler rejects with a `Shortfall`, not a `Remaining`. */
val mismatchedHandler = Reserve handledBy { _, arguments -> Decision.reject(Remaining(arguments.quantity)) }
