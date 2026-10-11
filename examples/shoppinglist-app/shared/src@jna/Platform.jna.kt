package org.example.examples.shopping

import java.security.MessageDigest
import java.security.SecureRandom
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

internal actual fun randomBytes(size: Int): ByteArray = ByteArray(size).also(SecureRandom()::nextBytes)
internal actual fun formatDate(epochSeconds: Long): String = DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)
    .withZone(ZoneId.systemDefault()).format(Instant.ofEpochSecond(epochSeconds))
internal actual fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)
