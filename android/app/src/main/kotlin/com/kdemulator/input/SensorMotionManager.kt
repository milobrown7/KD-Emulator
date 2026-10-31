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
 * * `TYPE_GAME_ROTATION_VECTOR` → GBA tilt cartridges, Wii Remote
 *   accelerometer and (optionally) gyro aiming of the IR pointer.
 * * `TYPE_LIGHT` → Boktai-style solar sensor.
 */
class SensorMotionManager(context: Context) : SensorEventListener {
    private val sensors = context.getSystemService(SensorManager::class.java)
    private val rotation = sensors.getDefaultSensor(Sensor.TYPE_GAME_ROTATION_VECTOR)
    private val lightSensor = sensors.getDefaultSensor(Sensor.TYPE_LIGHT)
    private val matrix = FloatArray(9)
    private val orientation = FloatArray(3)
    private var yawCentre: Float? = null

    /** Aim the Wii pointer with the phone instead of touch. */
    var gyroPointer = false

    fun start() {
        rotation?.let { sensors.registerListener(this, it, SensorManager.SENSOR_DELAY_GAME) }
        lightSensor?.let { sensors.registerListener(this, it, SensorManager.SENSOR_DELAY_NORMAL) }
    }

    fun stop() {
        sensors.unregisterListener(this)
        yawCentre = null
    }

    /** Re-centre the gyro pointer on the current heading. */
    fun recentre() {
        yawCentre = null
    }

    override fun onSensorChanged(event: SensorEvent) {
        when (event.sensor.type) {
            Sensor.TYPE_GAME_ROTATION_VECTOR -> {
                SensorManager.getRotationMatrixFromVector(matrix, event.values)
                SensorManager.getOrientation(matrix, orientation)
                val (yaw, pitch, roll) = Triple(orientation[0], orientation[1], orientation[2])
                // Tilt in g along the screen axes (landscape holding).
                InputHub.tiltX = sin(roll)
                InputHub.tiltY = sin(pitch)
                if (gyroPointer) {
                    val centre = yawCentre ?: yaw.also { yawCentre = it }
                    InputHub.pointerX = ((yaw - centre) / 0.6f).coerceIn(-1f, 1f)
                    InputHub.pointerY = (pitch / 0.45f).coerceIn(-1f, 1f)
                    InputHub.pointerValid = true
                }
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
