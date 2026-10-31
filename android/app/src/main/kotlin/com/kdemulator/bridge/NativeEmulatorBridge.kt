package com.kdemulator.bridge

import java.nio.ByteBuffer

/**
 * JNI surface of `libkd_emulator.so` (Rust crate `crates/android-jni`).
 *
 * Every function is a thin call into native code; the emulation itself runs
 * on a native thread pinned to the Tensor Prime/Mid clusters, and finished
 * frames are pulled by [com.kdemulator.ui.game.GameView] with [nativeCopyFrame].
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
    enum class Console(val id: Int, val label: String, val shortLabel: String, val screenWidth: Int, val screenHeight: Int) {
        GBA(1, "Game Boy Advance", "GBA", 240, 160),
        NDS(2, "Nintendo DS", "DS", 256, 384),
        GAMECUBE(3, "GameCube", "GC", 640, 480),
        WII(4, "Wii", "Wii", 640, 480);

        companion object {
            fun fromId(id: Int): Console? = entries.firstOrNull { it.id == id }
        }
    }

    @JvmStatic external fun nativeInit(cacheDir: String): String

    /** Load a game from a file descriptor. Returns a [Console] id or -1. */
    @JvmStatic external fun nativeLoadRomFd(fd: Int, name: String): Int

    /**
     * Library metadata as `key=value` lines (`system`, `code`, `title`, `ac`,
     * `icon_w`, `icon_h`); the game's own artwork, if any, is written to
     * [iconOut] as ARGB pixels (needs room for 96x32).
     */
    @JvmStatic external fun nativeGameInfoFd(fd: Int, name: String, iconOut: IntArray): String

    @JvmStatic external fun nativeStart()
    @JvmStatic external fun nativePause()
    @JvmStatic external fun nativeStop()

    /** `width shl 16 or height` of the newest frame (0 before any frame). */
    @JvmStatic external fun nativeGetFrameSize(): Int

    /**
     * Copy the newest RGBA frame into a direct [buffer] if it is newer than
     * [lastSeq]. Returns its sequence number, or -1 if the buffer is too small.
     */
    @JvmStatic external fun nativeCopyFrame(buffer: ByteBuffer, lastSeq: Long): Long

    /** Step [frames] frames synchronously while paused (frame advance). */
    @JvmStatic external fun nativeRunFrames(frames: Int)

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

    /** 1 = normal speed, 2–8 = fast-forward. */
    @JvmStatic external fun nativeSetSpeed(speed: Int)

    /** Output volume 0–256 (256 = unity). */
    @JvmStatic external fun nativeSetVolume(volume: Int)

    /** Rewind snapshot spacing in frames; 0 disables rewind. */
    @JvmStatic external fun nativeSetRewindInterval(frames: Int)
    @JvmStatic external fun nativeSetRewinding(on: Boolean)
    @JvmStatic external fun nativeGetRewindAvailable(): Int

    /** Replace the active cheats; returns error lines ("" when all parsed). */
    @JvmStatic external fun nativeSetCheats(spec: String): String

    /** "" when [code] parses as [format], otherwise the error. */
    @JvmStatic external fun nativeValidateCheat(format: String, code: String): String

    /** DS microphone input (16-bit mono PCM). */
    @JvmStatic external fun nativePushMic(samples: ShortArray, count: Int)

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

    /** Separators for [nativeSetCheats] (ASCII record / unit separators). */
    const val CHEAT_RECORD_SEP = '\u001E'
    const val CHEAT_FIELD_SEP = '\u001F'

    /** Parse the native `key=value` format. */
    fun parseInfo(text: String): Map<String, String> =
        text.lineSequence()
            .mapNotNull { line -> line.indexOf('=').takeIf { it > 0 }?.let { line.substring(0, it) to line.substring(it + 1) } }
            .toMap()
}
