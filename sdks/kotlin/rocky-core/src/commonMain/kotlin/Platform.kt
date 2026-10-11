package rocky
internal expect fun secureBytes(size: Int): ByteArray
internal expect fun fileExists(path: String): Boolean
expect open class PlatformOpeners()
expect open class PlatformCredentialOpeners()
