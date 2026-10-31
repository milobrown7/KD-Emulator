package com.kdemulator.input

import com.kdemulator.bridge.NativeEmulatorBridge

/**
 * Merges the touch overlay, hardware gamepads and motion sensors into one
 * snapshot and forwards it to native code whenever something changes.
 */
object InputHub {
    @Volatile var touchButtons = 0
    @Volatile var padButtons = 0

    /** Buttons latched on by the pause menu's "Hold Buttons". */
    @Volatile var heldButtons = 0
    @Volatile var stickX = 0f
    @Volatile var stickY = 0f
    @Volatile var cStickX = 0f
    @Volatile var cStickY = 0f
    @Volatile var triggerL = 0f
    @Volatile var triggerR = 0f

    /** NDS touchscreen point in 256x192 pixels, or -1 when the stylus is up. */
    @Volatile var touchX = -1
    @Volatile var touchY = -1

    /** Wii Remote pointer in -1..1, valid while aiming. */
    @Volatile var pointerX = 0f
    @Volatile var pointerY = 0f
    @Volatile var pointerValid = false

    @Volatile var tiltX = 0f
    @Volatile var tiltY = 0f
    @Volatile var light = 128

    /** Overlay stick wins when it is being used; otherwise the gamepad stick. */
    @Volatile var overlayStickX = 0f
    @Volatile var overlayStickY = 0f

    fun push() {
        val sx = if (overlayStickX != 0f || overlayStickY != 0f) overlayStickX else stickX
        val sy = if (overlayStickX != 0f || overlayStickY != 0f) overlayStickY else stickY
        NativeEmulatorBridge.nativeSetInput(
            touchButtons or padButtons or heldButtons,
            sx, sy, cStickX, cStickY, triggerL, triggerR,
            touchX, touchY,
            pointerX, pointerY, pointerValid,
            tiltX, tiltY, light,
        )
    }

    fun reset() {
        touchButtons = 0
        padButtons = 0
        heldButtons = 0
        stickX = 0f; stickY = 0f; cStickX = 0f; cStickY = 0f
        overlayStickX = 0f; overlayStickY = 0f
        triggerL = 0f; triggerR = 0f
        touchX = -1; touchY = -1
        pointerValid = false
    }
}
