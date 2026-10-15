package org.example.examples.shopping.data.coordination

import org.example.examples.shopping.util.readText
import rocky.ServiceCoordinator

/** A deployment's current service bundle; an absent file leaves decisions unavailable. */
suspend fun publicCoordination(directory: String): ServiceCoordinator? = readText("$directory/coordinator.json")?.let(::ServiceCoordinator)
