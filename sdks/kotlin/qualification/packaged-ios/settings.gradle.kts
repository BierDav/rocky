// A standalone build: the consumer sees only the packaged repository, never the SDK's projects.
pluginManagement {
    repositories {
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    versionCatalogs {
        create("libs") { from(files("../../gradle/libs.versions.toml")) }
    }
    repositories {
        maven {
            url = uri(providers.gradleProperty("rocky.repository").orNull ?: error("pass -Procky.repository=<packaged maven directory>"))
        }
        google()
        mavenCentral()
    }
}

rootProject.name = "packaged-ios"
