package org.example.examples.shopping.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import org.example.examples.shopping.ShoppingNode
import org.example.examples.shopping.ShoppingSession

/** A screen's ViewModel over the open installation: actions run once it is open, and failures become [messages]. */
abstract class NodeViewModel(session: ShoppingSession) : ViewModel() {
    protected val node: Flow<ShoppingNode> = session.node
    private val failures = Channel<String>(Channel.BUFFERED)
    /** One message per failed or refused action, for a snackbar. */
    val messages: Flow<String> = failures.receiveAsFlow()

    /** Shows [message] like a failure, for an outcome that changed nothing. */
    protected suspend fun tell(message: String) = failures.send(message)

    protected fun write(action: suspend ShoppingNode.() -> Unit) {
        viewModelScope.launch {
            try { node.first().action() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failures.send(error.message ?: error.toString()) }
        }
    }
}
