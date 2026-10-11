// Publishes as rocky:<project name>: to Maven local for development, or to the
// staging repository the release uploads; see docs/releasing.md.
plugins {
    `maven-publish`
}

group = "rocky"
// Releases pass -ProckyVersion; development uses the version the examples depend on.
version = providers.gradleProperty("rockyVersion").getOrElse("0.1.0")

publishing {
    repositories {
        maven {
            name = "staging"
            url = uri(rootProject.layout.buildDirectory.dir("release-maven"))
        }
    }
}
