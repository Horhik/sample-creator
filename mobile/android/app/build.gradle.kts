plugins {
    id("com.android.application")
    id("kotlin-android")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

android {
    namespace = "io.github.horhik.sample_creator"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    // Native libraries compressed inside the APK: a much smaller download,
    // they are unpacked once on install.
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = JavaVersion.VERSION_17.toString()
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "io.github.horhik.sample_creator"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        // cpal plays through AAudio, which appeared in Android 8 (API 26).
        minSdk = 26
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    buildTypes {
        release {
            // TODO: Add your own signing config for the release build.
            // Signing with the debug keys for now, so `flutter run --release` works.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
}

flutter {
    source = "../.."
}

// The engine is Rust: build it for the phone (arm64) and the emulator (x86_64)
// before every build. Always optimised: DSP and peak scanning crawl in debug.
val buildRust = tasks.register<Exec>("buildRust") {
    workingDir = file("../../rust")
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a", "-t", "x86_64",
        "-P", "26",
        "-o", file("src/main/jniLibs").absolutePath,
        "build", "--release",
    )
}
tasks.named("preBuild") { dependsOn(buildRust) }
