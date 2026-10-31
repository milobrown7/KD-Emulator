package com.kdemulator.input

import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import com.kdemulator.bridge.NativeEmulatorBridge.Buttons

/**
 * Maps Bluetooth/USB controllers (Xbox, DualSense, Stadia-style layouts) to
 * the emulator's logical buttons and axes. Call from the Activity's
 * `dispatchKeyEvent` / `dispatchGenericMotionEvent`.
 */
object HardwareGamepadManager {
    private var keyButtons = 0
    private var axisButtons = 0

    private val keyMap = mapOf(
        KeyEvent.KEYCODE_BUTTON_A to Buttons.A,
        KeyEvent.KEYCODE_BUTTON_B to Buttons.B,
        KeyEvent.KEYCODE_BUTTON_X to Buttons.X,
        KeyEvent.KEYCODE_BUTTON_Y to Buttons.Y,
        KeyEvent.KEYCODE_BUTTON_L1 to Buttons.L,
        KeyEvent.KEYCODE_BUTTON_R1 to Buttons.R,
        KeyEvent.KEYCODE_BUTTON_R2 to Buttons.Z,
        KeyEvent.KEYCODE_BUTTON_START to Buttons.START,
        KeyEvent.KEYCODE_BUTTON_SELECT to Buttons.SELECT,
        KeyEvent.KEYCODE_BUTTON_MODE to Buttons.HOME,
        KeyEvent.KEYCODE_BUTTON_THUMBL to Buttons.C,
        KeyEvent.KEYCODE_DPAD_UP to Buttons.UP,
        KeyEvent.KEYCODE_DPAD_DOWN to Buttons.DOWN,
        KeyEvent.KEYCODE_DPAD_LEFT to Buttons.LEFT,
        KeyEvent.KEYCODE_DPAD_RIGHT to Buttons.RIGHT,
    )

    private fun isGamepad(source: Int) =
        source and InputDevice.SOURCE_GAMEPAD == InputDevice.SOURCE_GAMEPAD ||
            source and InputDevice.SOURCE_JOYSTICK == InputDevice.SOURCE_JOYSTICK ||
            source and InputDevice.SOURCE_DPAD == InputDevice.SOURCE_DPAD

    /** Returns true if the event was consumed. */
    fun onKeyEvent(event: KeyEvent): Boolean {
        if (!isGamepad(event.source)) return false
        val bit = keyMap[event.keyCode] ?: return false
        keyButtons = when (event.action) {
            KeyEvent.ACTION_DOWN -> keyButtons or bit
            KeyEvent.ACTION_UP -> keyButtons and bit.inv()
            else -> keyButtons
        }
        InputHub.padButtons = keyButtons or axisButtons
        InputHub.push()
        return true
    }

    fun onMotionEvent(event: MotionEvent): Boolean {
        if (!isGamepad(event.source) || event.action != MotionEvent.ACTION_MOVE) return false
        fun axis(a: Int): Float {
            val range = event.device?.getMotionRange(a, event.source) ?: return event.getAxisValue(a)
            val v = event.getAxisValue(a)
            return if (kotlin.math.abs(v) <= range.flat) 0f else v
        }
        InputHub.stickX = axis(MotionEvent.AXIS_X)
        InputHub.stickY = -axis(MotionEvent.AXIS_Y)
        InputHub.cStickX = axis(MotionEvent.AXIS_Z)
        InputHub.cStickY = -axis(MotionEvent.AXIS_RZ)
        InputHub.triggerL = maxOf(axis(MotionEvent.AXIS_LTRIGGER), axis(MotionEvent.AXIS_BRAKE))
        InputHub.triggerR = maxOf(axis(MotionEvent.AXIS_RTRIGGER), axis(MotionEvent.AXIS_GAS))
        // Hat switches and analog triggers arrive as axes on many controllers.
        val hatX = axis(MotionEvent.AXIS_HAT_X)
        val hatY = axis(MotionEvent.AXIS_HAT_Y)
        var b = 0
        if (hatX < -0.5f) b = b or Buttons.LEFT
        if (hatX > 0.5f) b = b or Buttons.RIGHT
        if (hatY < -0.5f) b = b or Buttons.UP
        if (hatY > 0.5f) b = b or Buttons.DOWN
        if (InputHub.triggerL > 0.8f) b = b or Buttons.L
        if (InputHub.triggerR > 0.8f) b = b or Buttons.R
        axisButtons = b
        InputHub.padButtons = keyButtons or axisButtons
        InputHub.push()
        return true
    }
}
