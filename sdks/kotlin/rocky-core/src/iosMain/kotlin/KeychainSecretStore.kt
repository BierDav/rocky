@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class, kotlinx.cinterop.BetaInteropApi::class)

package rocky.ios

import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.usePinned
import kotlinx.cinterop.value
import platform.CoreFoundation.CFDictionaryCreateMutable
import platform.CoreFoundation.CFDictionaryRef
import platform.CoreFoundation.CFDictionarySetValue
import platform.CoreFoundation.CFRelease
import platform.CoreFoundation.CFTypeRefVar
import platform.CoreFoundation.kCFBooleanFalse
import platform.CoreFoundation.kCFBooleanTrue
import platform.CoreFoundation.kCFTypeDictionaryKeyCallBacks
import platform.CoreFoundation.kCFTypeDictionaryValueCallBacks
import platform.Foundation.CFBridgingRelease
import platform.Foundation.CFBridgingRetain
import platform.Foundation.NSData
import platform.Foundation.create
import platform.Foundation.getBytes
import platform.Security.SecItemAdd
import platform.Security.SecItemCopyMatching
import platform.Security.SecItemDelete
import platform.Security.SecItemUpdate
import platform.Security.errSecItemNotFound
import platform.Security.errSecSuccess
import platform.Security.kSecAttrAccessible
import platform.Security.kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
import platform.Security.kSecAttrAccount
import platform.Security.kSecAttrService
import platform.Security.kSecAttrSynchronizable
import platform.Security.kSecClass
import platform.Security.kSecClassGenericPassword
import platform.Security.kSecMatchLimit
import platform.Security.kSecMatchLimitOne
import platform.Security.kSecReturnData
import platform.Security.kSecValueData
import rocky.SecretStore

/** Device-only generic passwords, available after the first unlock. Use a service per installation. */
class KeychainSecretStore(private val service: String) : SecretStore {
    private val lock = ReentrantLock()

    init {
        require(service.isNotBlank()) { "Keychain service must not be blank" }
    }

    private fun query(name: String): Map<Any?, Any?> {
        require(name.isNotEmpty() && name.all { it in 'a'..'z' || it in '0'..'9' || it in "._-" }) { "invalid secret name" }
        return mapOf(kSecClass to kSecClassGenericPassword, kSecAttrService to service, kSecAttrAccount to name, kSecAttrSynchronizable to false)
    }

    override fun load(name: String): ByteArray? = lock.withLock {
        dictionary(query(name) + mapOf(kSecReturnData to true, kSecMatchLimit to kSecMatchLimitOne)) { query ->
            memScoped {
                val result = alloc<CFTypeRefVar>()
                result.value = null
                val status = SecItemCopyMatching(query, result.ptr)
                if (status == errSecItemNotFound) return@memScoped null
                checked(status, "load")
                val data = CFBridgingRelease(result.value) as? NSData ?: error("Keychain returned no data")
                ByteArray(data.length.toInt()).also { bytes ->
                    if (bytes.isNotEmpty()) bytes.usePinned { data.getBytes(it.addressOf(0), data.length) }
                }
            }
        }
    }

    override fun store(name: String, secret: ByteArray) = lock.withLock {
        val data = if (secret.isEmpty()) NSData() else secret.usePinned { NSData.create(bytes = it.addressOf(0), length = secret.size.toULong()) }
        val attributes = mapOf<Any?, Any?>(kSecValueData to data, kSecAttrAccessible to kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly)
        dictionary(query(name)) { query ->
            dictionary(attributes) { updates ->
                val status = SecItemUpdate(query, updates)
                if (status == errSecItemNotFound) {
                    dictionary(this.query(name) + attributes) { checked(SecItemAdd(it, null), "store") }
                } else {
                    checked(status, "store")
                }
            }
        }
    }

    override fun delete(name: String) = lock.withLock {
        dictionary(query(name)) {
            val status = SecItemDelete(it)
            if (status != errSecItemNotFound) checked(status, "delete")
        }
    }

    private fun checked(status: Int, operation: String) {
        check(status == errSecSuccess) { "Keychain $operation failed: OSStatus $status" }
    }

    private fun <T> dictionary(values: Map<Any?, Any?>, call: (CFDictionaryRef) -> T): T {
        val query = CFDictionaryCreateMutable(null, 0, kCFTypeDictionaryKeyCallBacks.ptr, kCFTypeDictionaryValueCallBacks.ptr)
            ?: error("cannot create Keychain query")
        return try {
            for ((key, value) in values) {
                val cfKey = key as? CPointer<*> ?: error("invalid Keychain attribute")
                val retained = value !is CPointer<*> && value !is Boolean
                val cfValue = when (value) {
                    is Boolean -> if (value) kCFBooleanTrue else kCFBooleanFalse
                    is CPointer<*> -> value
                    else -> CFBridgingRetain(value)
                } ?: error("invalid Keychain value")
                try {
                    CFDictionarySetValue(query, cfKey, cfValue)
                } finally {
                    if (retained) CFRelease(cfValue)
                }
            }
            call(query)
        } finally {
            CFRelease(query)
        }
    }
}
