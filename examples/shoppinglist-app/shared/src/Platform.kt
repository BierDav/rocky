package org.example.examples

interface Platform {
    val name: String
}

expect fun getPlatform(): Platform
