@file:OptIn(
    kotlinx.cinterop.ExperimentalForeignApi::class,
    kotlin.experimental.ExperimentalNativeApi::class,
    kotlin.ExperimentalStdlibApi::class,
    kotlin.native.runtime.NativeRuntimeApi::class,
)

package rocky

import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.reinterpret
import kotlinx.cinterop.sizeOf
import kotlinx.cinterop.value
import kotlinx.coroutines.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.darwin.KERN_SUCCESS
import platform.darwin.MACH_TASK_BASIC_INFO
import platform.darwin.integer_tVar
import platform.darwin.mach_msg_type_number_tVar
import platform.darwin.mach_task_basic_info
import platform.darwin.mach_task_self_
import platform.darwin.task_info
import rocky.internal.native.bindingLiveHandles
import rocky.internal.native.bindingPendingContinuations
import rocky_uniffi.cinterop.malloc_statistics_t
import rocky_uniffi.cinterop.malloc_zone_statistics
import kotlin.native.runtime.GC
import kotlin.test.*

class NativeLifetimeTest {
    private val definition = schema("lifetime", 1) {
        table("items") {
            text("key")
            primaryKey("key")
        }
    }
    private fun directory(): String = NSTemporaryDirectory() + "rocky-lifetime-" + NSUUID().UUIDString

    @Test fun aCancelledCallReleasesItsFutureAndObserverRemainsUsable() = runBlocking {
        withTimeout(30_000) {
            val root = directory()
            val database = RockyDatabase.local(root, definition)
            database.installAuthors()
            val events = database.driverTableEvents()
            try {
                val count = bindingLiveHandles()
                val pending = launch { events.next() }
                while (bindingPendingContinuations() == 0) yield()
                pending.cancelAndJoin()
                assertEquals(count, bindingLiveHandles())
                val next = async { events.next() }
                database.write { execute("INSERT INTO items(key,id) VALUES(?,?)", "one", nextId("items")) }
                assertContains(next.await().tables, "items")
            } finally {
                events.destroy()
                database.close()
                NSFileManager.defaultManager.removeItemAtPath(root, null)
            }
        }
    }

    @Test fun oneThousandOpenCloseCyclesReleaseAllHandlesAndBoundResidentMemory() = runBlocking {
        withTimeout(180_000) {
            val root = directory()
            RockyDatabase.local(root, definition).close()
            GC.collect()
            var baseline = 0L
            try {
                repeat(1_000) { index ->
                    val database = RockyDatabase.open(root, definition)
                    database.session()
                    database.close()
                    assertEquals(0, bindingLiveHandles(), "cycle ${index + 1}")
                    // Compare equal allocation windows before each RSS sample.
                    if (index == 99) {
                        baseline = collectedResidentBytes(100)
                    } else if (index < 999 && (index + 1) % 100 == 0) {
                        GC.collect()
                    }
                }
                val final = collectedResidentBytes(1_000)
                println("LIFETIME cycles=1000 handles=${bindingLiveHandles()} rss100=$baseline rss1000=$final")
                assertTrue(final <= baseline * 1.10, "$final > 110% of $baseline")
            } finally {
                NSFileManager.defaultManager.removeItemAtPath(root, null)
            }
        }
    }
}
private fun collectedResidentBytes(cycle: Int): Long {
    GC.collect()
    val nativeBytes = memScoped {
        val statistics = alloc<malloc_statistics_t>()
        malloc_zone_statistics(null, statistics.ptr)
        statistics.size_in_use.toLong()
    }
    val resident = residentBytes()
    val heap = GC.lastGCInfo?.memoryUsageAfter?.values?.sumOf { it.totalObjectsSizeBytes }
    val objects = GC.lastGCInfo?.sweepStatistics?.values?.sumOf { it.keptCount }
    println("LIFETIME sample=$cycle rss=$resident heap=$heap objects=$objects native=$nativeBytes")
    return resident
}

private fun residentBytes(): Long = memScoped {
    val info = alloc<mach_task_basic_info>()
    val count = alloc<mach_msg_type_number_tVar>()
    count.value = (sizeOf<mach_task_basic_info>() / sizeOf<integer_tVar>()).toUInt()
    check(task_info(mach_task_self_, MACH_TASK_BASIC_INFO.toUInt(), info.ptr.reinterpret(), count.ptr) == KERN_SUCCESS)
    info.resident_size.toLong()
}
