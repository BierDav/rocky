// Room's generated DAOs on Rocky's driver, against recorded statements and exported schemas.
plugins {
    id("rocky.multiplatform")
    id("rocky.room")
}

kotlin {
    sourceSets {
        commonMain.dependencies {
            implementation(project(":rocky-room"))
            implementation(libs.atomicfu)
        }
        commonTest.dependencies {
            implementation(kotlin("test"))
            implementation(libs.kotlinx.coroutines.test)
            implementation(project(":qualification:platform-test"))
            // The reference engine Room runs on in these tests.
            implementation(libs.sqlite.bundled)
        }
    }
}
