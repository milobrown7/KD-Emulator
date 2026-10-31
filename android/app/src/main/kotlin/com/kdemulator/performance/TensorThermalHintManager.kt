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
    private val power = context.getSystemService(PowerManager::class.java)
    private val hints = context.getSystemService(PerformanceHintManager::class.java)
    private var session: PerformanceHintManager.Session? = null

    private val _status = MutableStateFlow(PowerManager.THERMAL_STATUS_NONE)
    val status: StateFlow<Int> = _status.asStateFlow()

    private val listener = PowerManager.OnThermalStatusChangedListener { s ->
        _status.value = s
        NativeEmulatorBridge.nativeSetThermalStatus(s)
    }

    fun start(targetFps: Double) {
        power.addThermalStatusListener(context.mainExecutor, listener)
        listener.onThermalStatusChanged(power.currentThermalStatus)
        val tid = NativeEmulatorBridge.nativeGetEmuThreadTid()
        if (tid > 0 && session == null) {
            session = hints?.createHintSession(intArrayOf(tid), (1_000_000_000.0 / targetFps).toLong())
        }
    }

    /** Call once per presented frame (from a Choreographer callback). */
    fun reportFrame() {
        val ns = NativeEmulatorBridge.nativeGetLastFrameNanos()
        if (ns > 0) session?.reportActualWorkDuration(ns)
    }

    fun stop() {
        power.removeThermalStatusListener(listener)
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
