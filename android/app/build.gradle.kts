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
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
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
    implementation("androidx.compose.material:material-icons-core")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
}
