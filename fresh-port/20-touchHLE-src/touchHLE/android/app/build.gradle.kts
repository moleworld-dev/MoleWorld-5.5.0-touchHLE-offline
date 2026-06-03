/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Parts of this file are derived from SDL 2's Android project template, which
 * has a different license. Please see vendor/SDL/LICENSE.txt for details.
 */
import org.gradle.nativeplatform.platform.internal.DefaultNativePlatform

plugins {
    id("com.android.application") version("8.10.1")
    id("com.github.willir.rust.cargo-ndk-android") version("0.3.4")
    id("org.jetbrains.kotlin.android") version("2.0.21")
}

fun runTouchHLEVersionTool(wantBranding: Boolean): String {
    val output = providers.exec {
        commandLine("cargo", "run", "--package", "touchHLE_version")
        if (wantBranding) {
            args("--", "--branding")
        }
    }.standardOutput.asText.get().trim()

    return output
}

fun getTouchHLEBranding(): String {
    return runTouchHLEVersionTool(/* wantBranding: */ true)
}

fun getTouchHLEVersionName(): String {
    return runTouchHLEVersionTool(/* wantBranding: */ false)
}

fun join(prefix: String, separator: String, branding: String): String {
    return if (branding.isEmpty()) prefix else prefix + separator + branding
}

android {
    ndkVersion = "25.2.9519653"
    compileSdk = 31
    buildFeatures {
        buildConfig = true
    }
    androidResources {
        // 内置的 MoleWorld.ipa 已是 store 模式(里面全是 mp3/png/ccz 等已压缩资源),
        // 让 AGP 不要再压缩这个 ~110MB 的 asset:① 避免超大压缩 asset 在设备上读取/解压
        // 的内存与兼容问题;② 加快构建;③ 首启复制是一次顺序大读取。
        noCompress += "ipa"
    }
    defaultConfig {
        val branding = getTouchHLEBranding()
        applicationId = "org.touchhle.android"
        if (!branding.isEmpty()) {
            applicationIdSuffix = branding.lowercase()
        }
        // [MoleWorld] 固定 launcher 名称为"摩尔庄园HD"、图标固定用游戏高清图标
        // (@drawable/icon = res/drawable-nodpi/icon.png,已替换成游戏 iTunesArtwork)。
        // 不再随 touchHLE branding 变化(原本带 branding 时会解析成 icon_unofficial,
        // 且名字会带"touchHLE UNOFFICIAL …")。
        resValue("string", "app_name", "摩尔庄园HD")
        buildConfigField("String", "APP_NAME", "\"摩尔庄园HD\"")
        manifestPlaceholders["icon"] = "@drawable/icon"
        buildConfigField("int", "APP_ICON", "R.drawable.icon")
        // [MoleWorld] 版本号大一统:与 src/mole_sysinfo.rs 的 USER_VERSION 对齐,展示
        // 「v0.0.4 beta (短hash)」,不再用 git-describe 的 UNOFFICIAL 迷惑串。CI 注入
        // MOLE_USER_VERSION/MOLE_BUILD_HASH/MOLE_VERSION_CODE(build-release.yml),本地回退。
        // versionCode 由 v0.0.X 的 X 派生(=4)、随发版递增(覆盖更新需只增不减)。
        // 注意:applicationId 仍保留 .unofficial 后缀不动 —— 改包名会让老用户装成另一个 App、
        // 永远无法更新覆盖。
        val moleUserVersion = System.getenv("MOLE_USER_VERSION") ?: "v0.0.4 beta"
        val moleBuildHash = System.getenv("MOLE_BUILD_HASH")?.takeIf { it.isNotEmpty() }?.take(7) ?: "local"
        versionName = "$moleUserVersion ($moleBuildHash)"
        versionCode = (System.getenv("MOLE_VERSION_CODE") ?: "4").toInt()

        minSdk = 21 // first version with AArch64
        targetSdk = 31
        externalNativeBuild {
            ndkBuild {
                arguments("APP_PLATFORM=android-21")
                // abiFilters 'armeabi-v7a', 'arm64-v8a', 'x86', 'x86_64'
                // Only 'arm64-v8a' and 'x86_64' are supported by dynarmic
                // and hence touchHLE. The 'x86_64' build works, but the main
                // use for that would be the emulator in Android Studio, and
                // its OpenGL ES implementations don't seem to work properly
                // with touchHLE, so we disable it to reduce build time and
                // avoid shipping stuff we haven't meaningfully tested.
                // Make sure this matches the cargoNdk targets below.
                abiFilters("arm64-v8a")
            }
        }
    }
    // The target JVM version must be the same for Java and Kotlin.
    compileOptions {
        sourceCompatibility(JavaVersion.VERSION_11)
        targetCompatibility(JavaVersion.VERSION_11)
    }
    kotlinOptions {
        jvmTarget = "11"
    }
    // [MoleWorld] 固定 release 签名:用提交进仓库的 moleworld-release.p12(PKCS12,alias
    // moleworld)。固定签名 → 安卓更新可直接覆盖安装、不再每版签名冲突。(私钥+弱口令明文
    // 进公开仓库,纯爱好/非上架项目的取舍;要更安全可改用 GitHub secret 注入同一把 key。)
    signingConfigs {
        create("release") {
            storeFile = file("moleworld-release.p12")
            storePassword = "moleworld2025"
            keyAlias = "moleworld"
            keyPassword = "moleworld2025"
            storeType = "PKCS12"
        }
    }
    buildTypes {
        release {
            signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = false
            isDebuggable = true // allow use of ADB to manage files, etc
        }
        debug {
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("**/*.so")
            }
            isDebuggable = true
            isJniDebuggable = true
        }
    }

    applicationVariants.all {
        val variantName = name.replaceFirstChar { char ->
            if (char.isLowerCase()) char.titlecase() else char.toString()
        }
        tasks.named("merge${variantName}Assets").configure {
            dependsOn("externalNativeBuild${variantName}")
        }
    }

    sourceSets {
        getByName("main") {
            java.srcDir("${rootDir.parentFile}/vendor/SDL/android-project/app/src/main/java")
        }
    }

    if (!project.hasProperty("EXCLUDE_NATIVE_LIBS")) {
        sourceSets {
            getByName("main") {
                jniLibs.srcDir("${projectDir}/jniLibs")
            }
        }
        externalNativeBuild {
            ndkBuild {
                path("jni/Android.mk")
            }
        }
    }

    lint {
        abortOnError = false
    }
    namespace = "org.touchhle.android"
}

cargoNdk {
    // Make sure this matches the android abiFilters above.
    targets = arrayListOf("arm64")
    module = ".."
    librariesNames = arrayListOf("libtouchHLE.so", "libSDL2.so", "libc++_shared.so")
    extraCargoEnv = mapOf(
        "ANDROID_NDK" to android.ndkDirectory.toString(),
        "ANDROID_NDK_HOME" to android.ndkDirectory.toString(),
    )

    if (DefaultNativePlatform.host().operatingSystem.isWindows) {
        val binPath =
            android.ndkDirectory.toPath().resolve("toolchains/llvm/prebuilt/windows-x86_64/bin")
        val clangPath = binPath.resolve("clang.exe")
        val clangXXPath = binPath.resolve("clang++.exe")

        if (!clangPath.toFile().exists()) {
            throw GradleException("NDK clang compiler not found at expected location: $clangPath")
        }
        if (!clangXXPath.toFile().exists()) {
            throw GradleException("NDK clang++ compiler not found at expected location: $clangXXPath")
        }

        extraCargoEnv.putAll(
            mapOf(
                "CC" to clangPath.toString(),
                "CXX" to clangXXPath.toString(),
                // The default generator on Windows (Visual Studio) does not respect
                // the CC and CXX environment variables. Using Ninja ensures that
                // the specified compilers are used
                "CMAKE_GENERATOR" to "Ninja",
            )
        )
    }
    // The default feature, "static", makes us use static linking for SDL2 and OpenAL Soft.
    // For Android, we need dynamic linking for SDL2, but static linking for OpenAL Soft.
    extraCargoBuildArguments = arrayListOf(
        "--lib",
        "--no-default-features",
        "--features",
        "touchHLE_openal_soft_wrapper/static,sdl2/bundled"
    )
}

dependencies {
    implementation(fileTree("libs") {
        include("*.jar")
    })
}
