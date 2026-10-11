// Room's DAOs on Rocky's driver, with observers refreshed after sync.
plugins {
    id("rocky.library")
}

kotlin {
    sourceSets {
        commonMain.dependencies {
            api(project(":rocky-core"))
            api(project(":rocky-sqlite"))
            api(libs.room.runtime)
        }
    }
}
