package org.example.examples.shopping.qualification

import kotlinx.serialization.json.*

/*
 * Phase 20's seal scenarios: compaction and kept seals, a dev branch, a schema tried and published offline, and
 * concurrent publication with a rollback. See docs/features_09/20-seal-qualification.md.
 */

private val JsonElement.text: String get() = jsonPrimitive.content
private fun JsonObject.text(name: String): String? = get(name)?.jsonPrimitive?.contentOrNull
private fun expect(condition: Boolean, message: () -> String) {
    if (!condition) throw AssertionError(message())
}

/** Milliseconds [block] took, with its result. */
private inline fun <T> timed(block: () -> T): Pair<T, Long> {
    val started = System.nanoTime()
    val result = block()
    return result to (System.nanoTime() - started) / 1_000_000
}

internal fun Device.tips(): JsonObject = call("tips").jsonObject
internal fun Device.bookmark(): String? = tips().text("bookmark")
internal fun Device.tip(): String = tips().text("tip")!!
internal fun Device.storage(): JsonObject = call("storage").jsonObject
internal fun Device.maintain(sealAfter: Long? = null): JsonObject = call("maintain", "sealAfter" to sealAfter).jsonObject

/** Syncs [rounds] times, tolerating passes that reach no peer, as an offline or switching device does. */
internal fun Device.settle(rounds: Int = 2) = repeat(rounds) { runCatching { call("sync") } }

/** Retries [block] with sync passes on [devices] in between; fails with its last error. */
internal fun <T> retrying(what: String, vararg devices: Device, attempts: Int = 60, block: () -> T): T {
    var last: Throwable? = null
    repeat(attempts) {
        try {
            return block()
        } catch (error: Exception) {
            last = error
        }
        devices.forEach { it.settle(1) }
        Thread.sleep(250)
    }
    throw AssertionError("gave up on $what", last)
}

/** Polls [condition] with sync passes on [devices] in between. */
internal fun eventually(what: String, vararg devices: Device, attempts: Int = 60, condition: () -> Boolean) {
    repeat(attempts) {
        if (condition()) return
        devices.forEach { it.settle(1) }
        Thread.sleep(250)
    }
    expect(condition()) { "timed out waiting for $what" }
}

/**
 * A trusted desktop that founds the household and full members that join it, each list shared with them, as the
 * app's Devices tab shares every list with full devices.
 */
internal fun Run.household(vararg members: String, coordinator: String? = null): List<Device> {
    val trusted = device("trusted").open("create", "coordinator" to coordinator)
    val joined = members.map { name ->
        // As the app does: only trusted devices compose the coordination service.
        device(name).open("join", "invitation" to trusted.call("pairing", "profile" to "writer").text)
    }
    joined.forEach { it.sync() }
    return listOf(trusted) + joined
}

internal fun Run.shareAll(trusted: Device, list: Long, vararg members: Device) {
    trusted.share(list, *members)
    members.forEach { it.settle() }
}

/**
 * Slice 1: the app's seal policy on the trusted desktop, compaction on every device, and kept seals. The phone is
 * offline across one seal and returns; the trusted desktop shows the list as it was two seals ago.
 */
internal fun Run.seals() {
    val (trusted, member, phone) = household("member", "phone")
    val weekly = trusted.call("createList", "title" to "Weekly").jsonPrimitive.long
    for (title in listOf("Milk", "Eggs", "Flour")) trusted.call("add", "list" to weekly, "title" to title)
    shareAll(trusted, weekly, member, phone)
    eventually("both members to hold Weekly", member, phone) { member.items(weekly).size == 3 && phone.items(weekly).size == 3 }

    val (first, sealing) = timed { trusted.call("seal").text }
    val eggs = trusted.call("items", "list" to weekly).jsonArray.single { it.jsonObject.getValue("title").text == "Eggs" }.jsonObject.getValue("id").jsonPrimitive.long
    trusted.call("delete", "list" to weekly, "id" to eggs)
    trusted.call("add", "list" to weekly, "title" to "Bread")
    // The policy, with a threshold this scenario reaches: a round seals once enough writes gathered.
    val second = trusted.maintain(sealAfter = 2).text("sealed") ?: throw AssertionError("the policy did not seal")
    expect(trusted.maintain(sealAfter = 2).text("sealed") == null) { "the policy sealed without new writes" }
    eventually("both members to adopt the second seal", member, phone) { member.tip() == trusted.tip() && phone.tip() == trusted.tip() }
    phone.online(false)
    trusted.call("add", "list" to weekly, "title" to "Jam")
    member.call("check", "list" to weekly, "id" to trusted.call("items", "list" to weekly).jsonArray.single { it.jsonObject.getValue("title").text == "Milk" }.jsonObject.getValue("id").jsonPrimitive.long, "completed" to true)
    member.settle()
    trusted.settle()
    val third = trusted.call("seal").text
    val (_, adoption) = timed { eventually("the member to adopt the third seal", member) { member.tip() == trusted.tip() } }
    notes += "seals: first $sealing ms to seal; the member adopted a seal in $adoption ms"

    // Compaction keeps one seal uncompacted: at the third seal, every device compacts at the second.
    notes += "member before compacting: ${member.storage()}"
    val (memberRound, memberCompaction) = timed { member.maintain() }
    notes += "member compaction round: $memberRound"
    notes += "member items retention: ${member.call("retention", "collection" to "items", "maintain" to true)}"
    expect(memberRound.text("compacted") == second) { "member compacted at ${memberRound.text("compacted")}, not $second" }
    expect(memberRound.getValue("pruned").jsonPrimitive.long > 0) { "the member released nothing: $memberRound" }
    phone.online(true)
    eventually("the phone to return to the third seal", phone, trusted) { phone.tip() == trusted.tip() }
    notes += "phone before compacting: ${phone.storage()}"
    val (phoneRound, phoneCompaction) = timed { phone.maintain() }
    expect(phoneRound.text("compacted") == second) { "phone compacted at ${phoneRound.text("compacted")}, not $second" }
    expect(phoneRound.getValue("pruned").jsonPrimitive.long > 0) { "the phone released nothing: $phoneRound" }
    val trustedRound = trusted.maintain()
    notes += "compaction: member $memberCompaction ms, phone $phoneCompaction ms; trusted $trustedRound"

    val expected = listOf("Bread", "Flour", "Jam", "Milk [x]")
    for (device in listOf(trusted, member, phone)) {
        eventually("${device.name} to read the sealed lists", device, trusted) { device.items(weekly) == expected }
    }
    for (device in listOf(member, phone)) {
        val storage = device.storage()
        expect(storage.getValue("tombstones").jsonPrimitive.long == 0L) { "${device.name} keeps tombstones after compacting: $storage" }
        notes += "${device.name} after compaction: $storage"
    }
    notes += "trusted keeps every seal: ${trusted.storage()}"

    // The trusted desktop keeps every seal and reads earlier ones through read-only views.
    val held = trusted.tips().getValue("path").jsonArray.associate { it.jsonObject.text("id")!! to it.jsonObject.text("held") }
    expect(listOf(first, second, third).all { held[it] == "KEPT" }) { "trusted holds $held" }
    fun view(seal: String) = timed { trusted.call("snapshot", "seal" to seal).jsonArray.single().jsonObject.getValue("items").jsonArray.map { it.text }.sorted() }
    val (twoAgo, viewing) = view(second)
    expect(twoAgo == listOf("Bread", "Flour", "Milk")) { "two seals ago read $twoAgo" }
    val (threeAgo, _) = view(first)
    expect(threeAgo == listOf("Eggs", "Flour", "Milk")) { "three seals ago read $threeAgo" }
    notes += "kept seals: two seals ago read $twoAgo through a view in $viewing ms; three seals ago $threeAgo"
}

/**
 * Slice 2: the trusted desktop opens `dev` on main's latest seal, the member follows it, both write, and a
 * coordinated write runs through dev's own log. The phone on `main` receives nothing of dev; the member returns and
 * drops it.
 */
internal fun Run.branch() {
    val (trusted, member, phone) = household("member", "phone", coordinator = voters().bundle.path)
    trusted.call("register")
    val weekly = trusted.call("createList", "title" to "Weekly").jsonPrimitive.long
    trusted.call("add", "list" to weekly, "title" to "Milk")
    shareAll(trusted, weekly, member, phone)
    // Sealed first, so dev opens on main's latest seal while main's tip and its log stay open.
    trusted.call("seal")
    // A coordinated write on main, so main's log has a position to keep.
    val onMain = member.call("ask", "title" to "Paint").text
    retrying("a decision on main", trusted, member) { trusted.call("decide", "key" to onMain) }
    val mainPosition = trusted.call("coordinated").jsonPrimitive.long
    eventually("both members to hold Weekly and main's decision", member, phone, trusted) {
        listOf(member, phone).all { it.items(weekly) == listOf("Milk") && it.call("decisions").jsonArray.map { key -> key.text } == listOf(onMain) }
    }

    val (dev, opening) = timed { trusted.call("branch", "name" to "dev").text }
    expect(trusted.bookmark() == "dev") { "trusted follows ${trusted.bookmark()}" }
    eventually("the member to learn dev", member, trusted) { member.tips().getValue("bookmarks").jsonObject.containsKey("dev") }
    val (_, switching) = timed {
        member.call("follow", "name" to "dev")
        eventually("the member to switch to dev", member, trusted) {
            member.maintain()
            member.bookmark() == "dev"
        }
    }
    notes += "branch: opened dev in $opening ms; the member switched in $switching ms"
    trusted.call("add", "list" to weekly, "title" to "Trusted on dev")
    member.call("add", "list" to weekly, "title" to "Member on dev")
    eventually("both dev writes on both devices", trusted, member) {
        trusted.items(weekly) == listOf("Member on dev", "Milk", "Trusted on dev") && member.items(weekly) == trusted.items(weekly)
    }
    val onDev = member.call("ask", "title" to "Ladder").text
    retrying("a decision on dev", trusted, member) { trusted.call("decide", "key" to onDev) }
    val devPosition = trusted.call("coordinated").jsonPrimitive.long
    notes += "coordinated: main's log at $mainPosition; dev's log at $devPosition after its decision"

    phone.call("add", "list" to weekly, "title" to "Phone on main")
    phone.settle(3)
    expect(phone.items(weekly) == listOf("Milk", "Phone on main")) { "the phone read ${phone.items(weekly)}" }
    val phoneTips = phone.storage().getValue("tips").jsonArray.map { it.text }
    expect(dev !in phoneTips) { "the phone holds frames of dev: $phoneTips" }
    expect(phone.call("decisions").jsonArray.map { it.text } == listOf(onMain)) { "the phone read decisions ${phone.call("decisions")}" }
    notes += "the phone on main holds frames of ${phoneTips.size} tips, none of dev, and only main's decision"

    member.settle()
    member.call("leave", "name" to "dev")
    trusted.call("leave", "name" to "dev", "delete" to true)
    expect(trusted.call("coordinated").jsonPrimitive.long == mainPosition) { "main's log moved to ${trusted.call("coordinated")} from $mainPosition" }
    for (device in listOf(trusted, member, phone)) {
        eventually("${device.name} to read main", device, trusted, phone) { device.items(weekly) == listOf("Milk", "Phone on main") }
        expect(device.call("decisions").jsonArray.map { it.text } == listOf(onMain)) { "${device.name} on main read decisions ${device.call("decisions")}" }
    }
    expect(dev !in member.storage().getValue("tips").jsonArray.map { it.text }) { "the member kept dev's frames after dropping it" }
    eventually("dev's deletion to reach the member", member, trusted) { !member.tips().getValue("bookmarks").jsonObject.containsKey("dev") }
    notes += "main's log stayed at $mainPosition; the member dropped dev and every device reads main"
}
