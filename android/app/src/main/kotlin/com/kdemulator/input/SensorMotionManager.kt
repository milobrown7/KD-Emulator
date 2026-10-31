package com.kdemulator.input

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import kotlin.math.ln
import kotlin.math.sin

/**
 * Motion and light sensors:
 * * `TYPE_GAME_ROTATION_VECTOR` → GBA tilt cartridges.
 * * `TYPE_LIGHT` → Boktai-style solar sensor.
 */
class SensorMotionManager(context: Context) : SensorEventListener {
    private val sensors = context.getSystemService(SensorManager::class.java)
    private val rotation = sensors.getDefaultSensor(Sensor.TYPE_GAME_ROTATION_VECTOR)
    private val lightSensor = sensors.getDefaultSensor(Sensor.TYPE_LIGHT)
    private val matrix = FloatArray(9)
    private val orientation = FloatArray(3)

    fun start() {
        rotation?.let { sensors.registerListener(this, it, SensorManager.SENSOR_DELAY_GAME) }
        lightSensor?.let { sensors.registerListener(this, it, SensorManager.SENSOR_DELAY_NORMAL) }
    }

    fun stop() {
        sensors.unregisterListener(this)
    }

    override fun onSensorChanged(event: SensorEvent) {
        when (event.sensor.type) {
            Sensor.TYPE_GAME_ROTATION_VECTOR -> {
                SensorManager.getRotationMatrixFromVector(matrix, event.values)
                SensorManager.getOrientation(matrix, orientation)
                val (pitch, roll) = orientation[1] to orientation[2]
                // Tilt in g along the screen axes (landscape holding).
                InputHub.tiltX = sin(roll)
                InputHub.tiltY = sin(pitch)
                InputHub.push()
            }
            Sensor.TYPE_LIGHT -> {
                // ~1 lux .. ~100k lux mapped logarithmically onto 0..255.
                val lux = event.values[0].coerceAtLeast(1f)
                InputHub.light = ((ln(lux) / ln(100_000f)) * 255f).toInt().coerceIn(0, 255)
            }
        }
    }

    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit
}
