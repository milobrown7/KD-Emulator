package com.kdemulator.bridge

import android.view.Surface

/**
 * JNI surface of `libkd_emulator.so` (Rust crate `crates/android-jni`).
 *
 * Every function is a thin call into native code; the emulation itself runs
 * on a native thread pinned to the Tensor Prime/Mid clusters.
 */
object NativeEmulatorBridge {
    init {
        System.loadLibrary("kd_emulator")
    }

    /** Button bits — must match `kd_core::input::button` (checked by a Rust test). */
    object Buttons {
        const val A = 1 shl 0
        const val B = 1 shl 1
        const val X = 1 shl 2
        const val Y = 1 shl 3
        const val L = 1 shl 4
        const val R = 1 shl 5
        const val Z = 1 shl 6
        const val START = 1 shl 7
        const val SELECT = 1 shl 8
        const val UP = 1 shl 9
        const val DOWN = 1 shl 10
        const val LEFT = 1 shl 11
        const val RIGHT = 1 shl 12
        const val ONE = 1 shl 13
        const val TWO = 1 shl 14
        const val PLUS = 1 shl 15
        const val MINUS = 1 shl 16
        const val HOME = 1 shl 17
        const val C = 1 shl 18
        const val LID_CLOSED = 1 shl 19
        const val DEBUG = 1 shl 20
    }

    /** `kd_core::System` discriminants. */
    enum class Console(val id: Int, val label: String, val screenWidth: Int, val screenHeight: Int) {
        GBA(1, "Game Boy Advance", 240, 160),
        NDS(2, "Nintendo DS", 256, 384),
        GAMECUBE(3, "GameCube", 640, 480),
        WII(4, "Wii", 640, 480);

        companion object {
            fun fromId(id: Int): Console? = entries.firstOrNull { it.id == id }
        }
    }

    @JvmStatic external fun nativeInit(cacheDir: String): String

    /** Load a game from a Storage Access Framework descriptor. Returns a [Console] id or -1. */
    @JvmStatic external fun nativeLoadRomFd(fd: Int, name: String): Int

    /** "systemId|gameCode|title" from the header only, or "" if unrecognised. */
    @JvmStatic external fun nativePeekTitleFd(fd: Int, name: String): String

    @JvmStatic external fun nativeSetSurface(surface: Surface?)
    @JvmStatic external fun nativeStart()
    @JvmStatic external fun nativePause()
    @JvmStatic external fun nativeStop()

    @JvmStatic external fun nativeSetInput(
        buttons: Int,
        stickX: Float,
        stickY: Float,
        cX: Float,
        cY: Float,
        triggerL: Float,
        triggerR: Float,
        touchX: Int,
        touchY: Int,
        pointerX: Float,
        pointerY: Float,
        pointerValid: Boolean,
        tiltX: Float,
        tiltY: Float,
        light: Int,
    )

    /** `PowerManager.THERMAL_STATUS_*`. */
    @JvmStatic external fun nativeSetThermalStatus(status: Int)

    @JvmStatic external fun nativeSaveState(): ByteArray
    @JvmStatic external fun nativeLoadState(data: ByteArray): Boolean
    @JvmStatic external fun nativeGetBackup(): ByteArray
    @JvmStatic external fun nativeLoadBackup(data: ByteArray)

    /** Animal Crossing time travel: move the guest clock by [seconds]. */
    @JvmStatic external fun nativeTimeTravel(seconds: Long)
    @JvmStatic external fun nativeSetClockOffset(seconds: Long)
    @JvmStatic external fun nativeGetClockOffset(): Long

    /** `key=value` lines describing the Animal Crossing profile, or "". */
    @JvmStatic external fun nativeGetAnimalCrossingInfo(): String

    /** `key=value` lines for [com.kdemulator.ui.debugger.MobileDebuggerSheet]. */
    @JvmStatic external fun nativeGetDebugInfo(): String

    @JvmStatic external fun nativeGetEmuThreadTid(): Int
    @JvmStatic external fun nativeGetLastFrameNanos(): Long

    /** Parse the native `key=value` format. */
    fun parseInfo(text: String): Map<String, String> =
        text.lineSequence()
            .mapNotNull { line -> line.indexOf('=').takeIf { it > 0 }?.let { line.substring(0, it) to line.substring(it + 1) } }
            .toMap()
}
