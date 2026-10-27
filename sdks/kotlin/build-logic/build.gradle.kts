plugins {
    `kotlin-dsl`
}

dependencies {
    testImplementation(kotlin("test"))
    implementation(libs.kotlin.gradle.plugin)
    implementation(libs.android.gradle.plugin)
    implementation(libs.ksp.gradle.plugin)
    implementation(libs.room.gradle.plugin)
}
