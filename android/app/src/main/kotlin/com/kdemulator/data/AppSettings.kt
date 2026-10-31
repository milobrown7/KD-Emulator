package com.kdemulator.data

import android.content.Context
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Snapshot of the player's settings. */
data class Settings(
    val skinOpacity: Float = 0.7f,
    val buttonHaptics: Boolean = true,
    val volume: Float = 1f,
    val rewindEnabled: Boolean = true,
    /** Seconds between rewind snapshots. */
    val rewindIntervalSec: Float = 0.5f,
    val fastForwardSpeed: Int = 4,
    val autoResume: Boolean = true,
    val smoothScaling: Boolean = false,
    val microphone: Boolean = false,
    val gyroPointer: Boolean = false,
    /** Skin identifier per console (absent = the built-in skin). */
    val skins: Map<Console, String> = emptyMap(),
)

/** SharedPreferences-backed settings exposed as a [StateFlow]. */
class AppSettings(context: Context) {
    private val prefs = context.getSharedPreferences("kd_settings", Context.MODE_PRIVATE)
    private val _state = MutableStateFlow(read())
    val state: StateFlow<Settings> = _state.asStateFlow()

    private fun read() = Settings(
        skinOpacity = prefs.getFloat("skinOpacity", 0.7f),
        buttonHaptics = prefs.getBoolean("buttonHaptics", true),
        volume = prefs.getFloat("volume", 1f),
        rewindEnabled = prefs.getBoolean("rewindEnabled", true),
        rewindIntervalSec = prefs.getFloat("rewindIntervalSec", 0.5f),
        fastForwardSpeed = prefs.getInt("fastForwardSpeed", 4),
        autoResume = prefs.getBoolean("autoResume", true),
        smoothScaling = prefs.getBoolean("smoothScaling", false),
        microphone = prefs.getBoolean("microphone", false),
        gyroPointer = prefs.getBoolean("gyroPointer", false),
        skins = Console.entries.mapNotNull { c -> prefs.getString("skin_${c.name}", null)?.let { c to it } }.toMap(),
    )

    fun update(f: (Settings) -> Settings) {
        val s = f(_state.value)
        prefs.edit()
            .putFloat("skinOpacity", s.skinOpacity)
            .putBoolean("buttonHaptics", s.buttonHaptics)
            .putFloat("volume", s.volume)
            .putBoolean("rewindEnabled", s.rewindEnabled)
            .putFloat("rewindIntervalSec", s.rewindIntervalSec)
            .putInt("fastForwardSpeed", s.fastForwardSpeed)
            .putBoolean("autoResume", s.autoResume)
            .putBoolean("smoothScaling", s.smoothScaling)
            .putBoolean("microphone", s.microphone)
            .putBoolean("gyroPointer", s.gyroPointer)
            .apply {
                Console.entries.forEach { c ->
                    val id = s.skins[c]
                    if (id == null) remove("skin_${c.name}") else putString("skin_${c.name}", id)
                }
            }
            .apply()
        _state.value = s
    }
}
