package com.kdemulator.performance

import android.content.Context
import android.os.PerformanceHintManager
import android.os.PowerManager
import com.kdemulator.bridge.NativeEmulatorBridge
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Android Dynamic Performance Framework integration:
 * * forwards `PowerManager` thermal status to native code, which switches
 *   to async uber-shaders, HLE DSP and a lower internal resolution at
 *   `THERMAL_STATUS_MODERATE` (Tensor G5 / Pixel 10 runs hot under load);
 * * creates a `PerformanceHintManager` session for the native emulation
 *   thread and reports each frame's real work duration, so the scheduler
 *   ramps the Prime/Mid clusters exactly as much as needed.
 */
class TensorThermalHintManager(private val context: Context) {
    // Either service can be missing (some devices, test environments).
    private val power: PowerManager? = runCatching { context.getSystemService(PowerManager::class.java) }.getOrNull()
    private val hints: PerformanceHintManager? = runCatching { context.getSystemService(PerformanceHintManager::class.java) }.getOrNull()
    private var session: PerformanceHintManager.Session? = null

    private val _status = MutableStateFlow(PowerManager.THERMAL_STATUS_NONE)
    val status: StateFlow<Int> = _status.asStateFlow()

    private val listener = PowerManager.OnThermalStatusChangedListener { s ->
        _status.value = s
        NativeEmulatorBridge.nativeSetThermalStatus(s)
    }

    fun start(targetFps: Double) {
        // Some devices (and test environments) have no thermal service.
        runCatching {
            power?.addThermalStatusListener(context.mainExecutor, listener)
            power?.let { listener.onThermalStatusChanged(it.currentThermalStatus) }
        }
        val tid = NativeEmulatorBridge.nativeGetEmuThreadTid()
        if (tid > 0 && session == null) {
            session = runCatching { hints?.createHintSession(intArrayOf(tid), (1_000_000_000.0 / targetFps).toLong()) }.getOrNull()
        }
    }

    /** Call once per presented frame (from a Choreographer callback). */
    fun reportFrame() {
        val ns = NativeEmulatorBridge.nativeGetLastFrameNanos()
        if (ns > 0) session?.reportActualWorkDuration(ns)
    }

    fun stop() {
        runCatching { power?.removeThermalStatusListener(listener) }
        session?.close()
        session = null
    }

    companion object {
        fun label(status: Int): String = when (status) {
            PowerManager.THERMAL_STATUS_NONE -> "Cool"
            PowerManager.THERMAL_STATUS_LIGHT -> "Light"
            PowerManager.THERMAL_STATUS_MODERATE -> "Moderate — mitigations on"
            PowerManager.THERMAL_STATUS_SEVERE -> "Severe"
            PowerManager.THERMAL_STATUS_CRITICAL -> "Critical"
            else -> "Emergency"
        }
    }
}
