plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.multiplatform)
}

kotlin {
    androidTarget()
}

android {
    namespace = "com.example.muse"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.example.muse"
        minSdk = 26
    }
}
