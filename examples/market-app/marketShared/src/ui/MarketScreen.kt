package org.example.examples.market.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.example.examples.market.Market
import org.example.examples.market.Remaining
import org.example.examples.market.Shortfall
import rocky.Outcome
import rocky.RockyException
import rocky.Ticket
import rocky.TicketState

/** What the screen says about the latest reservation, including while it is pending or its outcome is unknown. */
fun describe(ticket: Ticket<Remaining, Shortfall>?, failure: String? = null): String = when {
    failure != null -> "Not sent: $failure"

    ticket == null -> "No reservation yet"

    else -> when (val outcome = ticket.outcome) {
        is Outcome.Accepted -> "Accepted: ${outcome.value.remaining} left"

        is Outcome.Rejected -> "Rejected: ${outcome.value.remaining} left, ${outcome.value.requested} requested"

        null -> when (ticket.state) {
            TicketState.Queued, TicketState.Prepared -> "Pending"
            TicketState.OutcomeUnknown -> "Outcome unknown; retrying the same ticket"
            TicketState.Confirmed, TicketState.Applied -> "Confirmed; the result arrives with the next sync"
            TicketState.Cancelled -> "Cancelled"
            TicketState.Expired -> "Expired unconfirmed; it never ran"
            TicketState.Retired -> "Retired; the outcome is no longer available"
        } +
            (ticket.error?.let { " (${it.code})" } ?: "")
    }
}

private val TicketState.unresolved get() = this in setOf(TicketState.Queued, TicketState.Prepared, TicketState.OutcomeUnknown)

/** The market's one screen: listings with their confirmed stock, a reserve action and the latest ticket's state. */
@Composable
fun MarketScreen(market: Market, foreground: Boolean = true) {
    val offers by market.offers().collectAsState(emptyList())
    val holds by market.holds().collectAsState(emptyList())
    val connection by market.rocky.status.collectAsState()
    var ticket by remember { mutableStateOf<Ticket<Remaining, Shortfall>?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var title by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    val owner = market.isOwner

    LaunchedEffect(market, foreground) { if (foreground) market.maintain() }
    // An unresolved ticket runs again until it settles; the same ticket, never a new one.
    LaunchedEffect(ticket?.handle, foreground) {
        if (!foreground) return@LaunchedEffect
        val handle = ticket?.handle ?: return@LaunchedEffect
        while (ticket?.state?.unresolved == true || ticket?.state == TicketState.Confirmed) {
            delay(2_000)
            ticket = runCatching { market.retry(handle) }.getOrElse { market.reservation(handle) ?: ticket }
        }
    }
    fun act(block: suspend () -> Unit) {
        scope.launch {
            failure = try {
                block()
                null
            } catch (error: RockyException) {
                "${error.code}: ${error.message}"
            }
        }
    }

    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Market", style = MaterialTheme.typography.headlineSmall)
        Text(describe(ticket, failure), Modifier.testTag("ticket"))
        connection.error?.let { Text("Peer connection: $it") }
        if (owner) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(title, { title = it }, Modifier.weight(1f), label = { Text("New listing") }, singleLine = true)
                Button({ act { market.addListing(title.trim().lowercase().replace(' ', '-'), title.trim()).also { title = "" } } }, enabled = title.isNotBlank()) {
                    Text("List")
                }
            }
        }
        LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(offers, key = { it.key }) { offer ->
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(offer.title)
                        Text(offer.remaining?.let { "$it in stock" } ?: "No stock confirmed", style = MaterialTheme.typography.bodySmall)
                    }
                    if (owner) TextButton({ act { market.restock(offer.key, 1) } }) { Text("Restock") }
                    Button({ act { ticket = market.reserve(offer.key, 1) } }, Modifier.testTag("reserve-${offer.key}"), enabled = ticket?.state?.unresolved != true) {
                        Text("Reserve")
                    }
                }
            }
        }
        Text("${holds.size} confirmed holds, ${holds.sumOf { it.quantity }} items", style = MaterialTheme.typography.bodySmall)
    }
}
