package org.example.examples.shopping.data.session

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flow
import rocky.RockyDatabase

/** Whether other devices sync with this one: a session is open, or served sessions grew since the last look. */
fun RockyDatabase.serving(): Flow<Boolean> = flow {
    var last = -1L
    while (true) {
        val network = network()
        emit(network.inboundActive > 0 || (last >= 0 && network.inboundServed > last))
        last = network.inboundServed
        delay(3_000)
    }
}.distinctUntilChanged()
