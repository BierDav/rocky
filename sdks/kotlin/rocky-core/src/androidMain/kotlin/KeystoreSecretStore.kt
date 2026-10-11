package rocky.android

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import rocky.SecretStore
import java.io.File
import java.io.FileOutputStream
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Installation secrets sealed with AES-256-GCM under a non-exportable Android Keystore key. Only ciphertext
 * bound to its name is written; a secret is plaintext only in process memory while the runtime uses it.
 * See docs/runtime/android.md for the threat model.
 *
 * Never invents a secret: an absent name loads as null, and a stored secret that cannot be opened (its
 * Keystore key is gone, or the file was altered) fails instead of reading as absent.
 */
class KeystoreSecretStore(
    private val directory: File,
    private val alias: String = DEFAULT_ALIAS,
    private val strongBox: Boolean = false,
) : SecretStore {
    /** Ciphertext in the app's no-backup directory: the Keystore key is never in a backup either. */
    constructor(context: Context, alias: String = DEFAULT_ALIAS, strongBox: Boolean = false) :
        this(File(context.noBackupFilesDir, "rocky-secrets"), alias, strongBox)

    private val lock = Any()

    override fun load(name: String): ByteArray? = synchronized(lock) {
        val file = path(name)
        if (!file.exists()) return null
        val sealed = file.readBytes()
        require(
            sealed.size > HEADER.size + 1 + IV_BYTES &&
                sealed.copyOfRange(0, HEADER.size).contentEquals(HEADER) &&
                sealed[HEADER.size] == FORMAT,
        ) {
            "secret $name is not a sealed Rocky secret"
        }
        val key = key() ?: throw IllegalStateException("Keystore key $alias is missing, so secret $name cannot be opened; it is never regenerated")
        val iv = sealed.copyOfRange(HEADER.size + 1, HEADER.size + 1 + IV_BYTES)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, iv))
        cipher.updateAAD(aad(name))
        try {
            cipher.doFinal(sealed, HEADER.size + 1 + IV_BYTES, sealed.size - HEADER.size - 1 - IV_BYTES)
        } catch (error: javax.crypto.AEADBadTagException) {
            throw IllegalStateException("secret $name failed authentication", error)
        }
    }

    override fun store(name: String, secret: ByteArray): Unit = synchronized(lock) {
        val file = path(name)
        val key = key() ?: run {
            // A new key would orphan what the lost one sealed; refuse instead of hiding the loss.
            check(sealedNames().isEmpty()) { "Keystore key $alias is missing while sealed secrets exist" }
            generate()
        }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        cipher.updateAAD(aad(name))
        val sealed = cipher.doFinal(secret)
        val iv = cipher.iv
        check(iv.size == IV_BYTES) { "unexpected GCM nonce length" }
        directory.mkdirs()
        val partial = File.createTempFile(".rocky-", ".tmp", directory)
        try {
            FileOutputStream(partial).use { out ->
                out.write(HEADER)
                out.write(FORMAT.toInt())
                out.write(iv)
                out.write(sealed)
                out.fd.sync()
            }
            check(partial.renameTo(file)) { "cannot replace secret $name" }
        } finally {
            partial.delete()
        }
    }

    override fun delete(name: String): Unit = synchronized(lock) {
        val file = path(name)
        check(!file.exists() || file.delete()) { "cannot delete secret $name" }
    }

    private fun sealedNames(): List<String> = directory.listFiles()?.map { it.name }?.filter { !it.startsWith(".") }.orEmpty()

    private fun path(name: String): File {
        require(
            name.isNotEmpty() &&
                name.length <= 255 &&
                !name.startsWith(".") &&
                name.all { it in 'a'..'z' || it in '0'..'9' || it in "._-" },
        ) { "invalid secret name" }
        return File(directory, name)
    }

    private fun aad(name: String) = "rocky.secret.v1:$name".toByteArray(Charsets.UTF_8)

    private fun key(): SecretKey? {
        val keyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        return (keyStore.getEntry(alias, null) as? KeyStore.SecretKeyEntry)?.secretKey
    }

    private fun generate(): SecretKey {
        val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setRandomizedEncryptionRequired(true)
        if (strongBox) {
            check(Build.VERSION.SDK_INT >= 28) { "StrongBox needs Android 9" }
            spec.setIsStrongBoxBacked(true)
        }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE).run {
            init(spec.build())
            generateKey()
        }
    }

    companion object {
        const val DEFAULT_ALIAS = "rocky.secret-store.v1"
        private const val KEYSTORE = "AndroidKeyStore"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val TAG_BITS = 128
        private const val IV_BYTES = 12
        private val HEADER = "RKSS".toByteArray(Charsets.US_ASCII)
        private const val FORMAT: Byte = 1
    }
}
