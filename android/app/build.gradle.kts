plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// 发布签名：CI 从 Secrets 注入（RELEASE_KEYSTORE_FILE 指向解码后的 keystore）；
// 本地默认 android/release.keystore；文件不存在时回退 debug 签名（仅自测用）。
val releaseKeystoreFile = file(System.getenv("RELEASE_KEYSTORE_FILE") ?: "../release.keystore")

android {
    namespace = "com.ainovel.engine"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.ainovel.engine"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    // Rust 引擎交叉编译产物（cargo ndk -o app/src/main/jniLibs）
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")

    signingConfigs {
        create("release") {
            if (releaseKeystoreFile.exists()) {
                storeFile = releaseKeystoreFile
                storePassword = System.getenv("RELEASE_STORE_PASSWORD")
                keyAlias = System.getenv("RELEASE_KEY_ALIAS") ?: "ainovel"
                keyPassword = System.getenv("RELEASE_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (releaseKeystoreFile.exists()) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }
}

dependencies {
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.core:core-ktx:1.13.1")
}
