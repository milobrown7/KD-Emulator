package com.kdemulator.ui.overlay

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import com.kdemulator.bridge.NativeEmulatorBridge.Buttons
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.input.InputHub
import com.kdemulator.ui.theme.Island
import kotlin.math.roundToInt
import kotlin.math.sqrt

/** On-screen control schemes; the Animal Crossing profile picks one per console. */
enum class ControlLayout { CLASSIC, STYLUS, DUAL_STICK, POINTER }

fun defaultLayout(console: Console): ControlLayout = when (console) {
    Console.GBA -> ControlLayout.CLASSIC
    Console.NDS -> ControlLayout.STYLUS
    Console.GAMECUBE -> ControlLayout.DUAL_STICK
    Console.WII -> ControlLayout.POINTER
}

fun layoutFromNative(name: String?, console: Console): ControlLayout = when (name) {
    "classic" -> ControlLayout.CLASSIC
    "stylus" -> ControlLayout.STYLUS
    "dualstick" -> ControlLayout.DUAL_STICK
    "pointer" -> ControlLayout.POINTER
    else -> defaultLayout(console)
}

private val ButtonFill = Color(0x66FFFFFF)
private val ButtonEdge = Color(0x99FFFFFF)

@Composable
private fun PadButton(label: String, bit: Int, size: Int = 56, accent: Color = Island.Leaf, onPress: ((Boolean) -> Unit)? = null) {
    var pressed by remember { mutableStateOf(false) }
    Box(
        Modifier
            .size(size.dp)
            .clip(CircleShape)
            .background(if (pressed) accent.copy(alpha = 0.7f) else ButtonFill)
            .border(2.dp, ButtonEdge, CircleShape)
            .pointerInput(bit) {
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false)
                    pressed = true
                    InputHub.touchButtons = InputHub.touchButtons or bit
                    onPress?.invoke(true)
                    InputHub.push()
                    waitForUpOrCancellation()
                    pressed = false
                    InputHub.touchButtons = InputHub.touchButtons and bit.inv()
                    onPress?.invoke(false)
                    InputHub.push()
                }
            },
        contentAlignment = Alignment.Center,
    ) { Text(label, color = Color.White) }
}

@Composable
private fun DPad() {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        PadButton("▲", Buttons.UP, 48)
        Row(horizontalArrangement = Arrangement.spacedBy(48.dp)) {
            PadButton("◀", Buttons.LEFT, 48)
            PadButton("▶", Buttons.RIGHT, 48)
        }
        PadButton("▼", Buttons.DOWN, 48)
    }
}

/** Virtual analog stick; `onMove` receives -1..1 (y up). */
@Composable
private fun AnalogStick(onMove: (Float, Float) -> Unit) {
    var knob by remember { mutableStateOf(Offset.Zero) }
    Box(
        Modifier
            .size(130.dp)
            .clip(CircleShape)
            .background(ButtonFill)
            .border(2.dp, ButtonEdge, CircleShape)
            .pointerInput(Unit) {
                val r = size.width / 2f
                detectDragGestures(
                    onDragStart = { knob = Offset.Zero },
                    onDragEnd = { knob = Offset.Zero; onMove(0f, 0f) },
                    onDragCancel = { knob = Offset.Zero; onMove(0f, 0f) },
                ) { change, drag ->
                    change.consume()
                    var k = knob + drag
                    val len = sqrt(k.x * k.x + k.y * k.y)
                    if (len > r) k = k * (r / len)
                    knob = k
                    onMove(k.x / r, -k.y / r)
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier
                .offset { IntOffset((knob.x / 2.5f).roundToInt(), (knob.y / 2.5f).roundToInt()) }
                .size(56.dp)
                .clip(CircleShape)
                .background(Island.Sand.copy(alpha = 0.8f)),
        )
    }
}

@Composable
private fun Shoulder(label: String, bit: Int, analog: ((Boolean) -> Unit)? = null) {
    var pressed by remember { mutableStateOf(false) }
    Box(
        Modifier
            .size(width = 84.dp, height = 40.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(if (pressed) Island.Wood.copy(alpha = 0.7f) else ButtonFill)
            .border(2.dp, ButtonEdge, RoundedCornerShape(14.dp))
            .pointerInput(bit) {
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false)
                    pressed = true
                    InputHub.touchButtons = InputHub.touchButtons or bit
                    analog?.invoke(true)
                    InputHub.push()
                    waitForUpOrCancellation()
                    pressed = false
                    InputHub.touchButtons = InputHub.touchButtons and bit.inv()
                    analog?.invoke(false)
                    InputHub.push()
                }
            },
        contentAlignment = Alignment.Center,
    ) { Text(label, color = Color.White) }
}

/**
 * Touch area exactly covering the emulated screen. In STYLUS layout the
 * lower half is the DS touchscreen; in POINTER layout the whole screen aims
 * the Wii Remote.
 */
@Composable
fun ScreenTouchLayer(layout: ControlLayout, modifier: Modifier = Modifier) {
    when (layout) {
        ControlLayout.STYLUS -> Box(
            modifier.pointerInput(Unit) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    fun map(p: Offset): Boolean {
                        val half = size.height / 2f
                        if (p.y < half) return false
                        InputHub.touchX = (p.x / size.width * 256f).roundToInt().coerceIn(0, 255)
                        InputHub.touchY = ((p.y - half) / half * 192f).roundToInt().coerceIn(0, 191)
                        InputHub.push()
                        return true
                    }
                    map(down.position)
                    while (true) {
                        val event = awaitPointerEvent()
                        val change = event.changes.firstOrNull { it.id == down.id } ?: break
                        if (!change.pressed) break
                        map(change.position)
                        change.consume()
                    }
                    InputHub.touchX = -1
                    InputHub.touchY = -1
                    InputHub.push()
                }
            },
        )
        ControlLayout.POINTER -> Box(
            modifier.pointerInput(Unit) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    fun aim(p: Offset) {
                        InputHub.pointerX = (p.x / size.width * 2f - 1f).coerceIn(-1f, 1f)
                        InputHub.pointerY = (p.y / size.height * 2f - 1f).coerceIn(-1f, 1f)
                        InputHub.pointerValid = true
                        InputHub.push()
                    }
                    aim(down.position)
                    while (true) {
                        val event = awaitPointerEvent()
                        val change = event.changes.firstOrNull { it.id == down.id } ?: break
                        if (!change.pressed) break
                        aim(change.position)
                        change.consume()
                    }
                    // The pointer stays where it was last aimed.
                }
            },
        )
        else -> Unit
    }
}

/** Buttons and sticks drawn around the screen. */
@Composable
fun BoxScope.TouchControllerOverlay(console: Console, layout: ControlLayout) {
    val accent = when (console) {
        Console.GBA -> Island.Lagoon
        Console.NDS -> Island.Sky
        Console.GAMECUBE -> Island.Leaf
        Console.WII -> Island.Sunset
    }
    // Shoulders
    Row(Modifier.align(Alignment.TopCenter).padding(12.dp), horizontalArrangement = Arrangement.spacedBy(180.dp)) {
        when (layout) {
            ControlLayout.DUAL_STICK -> {
                Shoulder("L", Buttons.L) { InputHub.triggerL = if (it) 1f else 0f }
                Shoulder("R", Buttons.R) { InputHub.triggerR = if (it) 1f else 0f }
            }
            ControlLayout.POINTER -> {
                Shoulder("C", Buttons.C)
                Shoulder("Z", Buttons.Z)
            }
            else -> {
                Shoulder("L", Buttons.L)
                Shoulder("R", Buttons.R)
            }
        }
    }
    // Left cluster
    Box(Modifier.align(Alignment.BottomStart).padding(24.dp)) {
        when (layout) {
            ControlLayout.DUAL_STICK, ControlLayout.POINTER -> AnalogStick { x, y ->
                InputHub.overlayStickX = x
                InputHub.overlayStickY = y
                InputHub.push()
            }
            else -> DPad()
        }
    }
    // Right cluster
    Box(Modifier.align(Alignment.BottomEnd).padding(24.dp)) {
        when (layout) {
            ControlLayout.CLASSIC -> Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                PadButton("B", Buttons.B, accent = accent)
                Box(Modifier.offset(y = (-28).dp)) { PadButton("A", Buttons.A, accent = accent) }
            }
            ControlLayout.STYLUS, ControlLayout.DUAL_STICK -> Column(horizontalAlignment = Alignment.CenterHorizontally) {
                PadButton(if (layout == ControlLayout.STYLUS) "X" else "Y", if (layout == ControlLayout.STYLUS) Buttons.X else Buttons.Y, 50, accent)
                Row(horizontalArrangement = Arrangement.spacedBy(50.dp)) {
                    PadButton(if (layout == ControlLayout.STYLUS) "Y" else "X", if (layout == ControlLayout.STYLUS) Buttons.Y else Buttons.X, 50, accent)
                    PadButton("A", Buttons.A, 50, accent)
                }
                PadButton("B", Buttons.B, 50, accent)
                if (layout == ControlLayout.DUAL_STICK) Shoulder("Z", Buttons.Z)
            }
            ControlLayout.POINTER -> Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                PadButton("A", Buttons.A, 64, accent)
                PadButton("B", Buttons.B, 52, accent)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    PadButton("1", Buttons.ONE, 40, accent)
                    PadButton("2", Buttons.TWO, 40, accent)
                }
            }
        }
    }
    // System buttons
    Row(Modifier.align(Alignment.BottomCenter).padding(12.dp), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        when (layout) {
            ControlLayout.POINTER -> {
                PadButton("−", Buttons.MINUS, 40)
                PadButton("⌂", Buttons.HOME, 40)
                PadButton("+", Buttons.PLUS, 40)
            }
            ControlLayout.DUAL_STICK -> PadButton("Start", Buttons.START, 52)
            else -> {
                PadButton("Sel", Buttons.SELECT, 44)
                PadButton("Start", Buttons.START, 44)
            }
        }
    }
}
