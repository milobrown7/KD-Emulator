plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.kdemulator"
    compileSdk = 35
    // NDK r28+: 16 KB page-aligned toolchain defaults.
    ndkVersion = "28.0.13004108"

    defaultConfig {
        applicationId = "com.kdemulator"
        minSdk = 31
        targetSdk = 35
        versionCode = 2
        versionName = "0.2.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    // Release signing comes from the environment (CI secrets or a local
    // keystore); without KD_KEYSTORE the release APK is left unsigned.
    val keystore = System.getenv("KD_KEYSTORE")?.let(::file)?.takeIf { it.exists() }
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = keystore
                storePassword = System.getenv("KD_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("KD_KEY_ALIAS")
                keyPassword = System.getenv("KD_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            if (keystore != null) signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures { compose = true }

    // Store libkd_emulator.so uncompressed so the installer maps it straight
    // from the APK at a 16 KB-aligned offset (required on 16 KB-page kernels).
    packaging { jniLibs { useLegacyPackaging = false } }

    testOptions { unitTests { isIncludeAndroidResources = true } }
}

// Build the Rust native library (crates/android-jni) with cargo-ndk.
// The 16 KB ELF alignment flags come from ../.cargo/config.toml.
val cargoBuild by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds libkd_emulator.so for arm64-v8a"
    workingDir = rootProject.projectDir.parentFile
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "--platform", "31",
        "-o", "android/app/src/main/jniLibs",
        "build", "--release", "-p", "android-jni",
    )
}
tasks.named("preBuild") { dependsOn(cargoBuild) }

// JVM UI tests (Robolectric) load a host build of the same native library.
val cargoBuildHost by tasks.registering(Exec::class) {
    group = "verification"
    description = "Builds libkd_emulator.so for the host so Robolectric tests run real emulation"
    workingDir = rootProject.projectDir.parentFile
    commandLine("cargo", "build", "-p", "android-jni")
}
tasks.withType<Test>().configureEach {
    dependsOn(cargoBuildHost)
    val repo = rootProject.projectDir.parentFile
    systemProperty("java.library.path", repo.resolve("target/debug").absolutePath)
    systemProperty("kd.screenshots", layout.buildDirectory.dir("screenshots").get().asFile.absolutePath)
    maxHeapSize = "3g"
    // Optional mirror for Robolectric's android-all jars (Maven Central rate-limits some networks).
    System.getenv("ROBOLECTRIC_REPO")?.let { systemProperty("robolectric.dependency.repo.url", it) }
    testLogging { events("passed", "failed", "skipped"); showStandardStreams = true; exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("androidx.documentfile:documentfile:1.0.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
    testImplementation("androidx.test:core:1.6.1")
    testImplementation("androidx.test.ext:junit:1.2.1")
    testImplementation("androidx.compose.ui:ui-test-junit4")
}
