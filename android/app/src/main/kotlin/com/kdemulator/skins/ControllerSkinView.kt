package com.kdemulator.skins

import android.graphics.PointF
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.input.pointer.PointerId
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.sp
import com.kdemulator.bridge.NativeEmulatorBridge.Buttons
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.input.InputHub
import com.kdemulator.ui.theme.Island
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.min

/** Front-end actions a skin button can trigger. */
interface SkinActions {
    fun menu()
    fun quickSave()
    fun quickLoad()
    fun setFastForward(on: Boolean)
    fun toggleFastForward()
    fun setRewinding(on: Boolean)
}

/** Maps skin input names (Delta's plus KD-Emulator's) to button bits. */
object SkinInputMap {
    fun bit(name: String): Int? = when (name) {
        "a" -> Buttons.A
        "b" -> Buttons.B
        "x" -> Buttons.X
        "y" -> Buttons.Y
        "l" -> Buttons.L
        "r" -> Buttons.R
        "z", "zl", "zr" -> Buttons.Z
        "c" -> Buttons.C
        "start" -> Buttons.START
        "select" -> Buttons.SELECT
        "up" -> Buttons.UP
        "down" -> Buttons.DOWN
        "left" -> Buttons.LEFT
        "right" -> Buttons.RIGHT
        "one", "1" -> Buttons.ONE
        "two", "2" -> Buttons.TWO
        "plus" -> Buttons.PLUS
        "minus" -> Buttons.MINUS
        "home" -> Buttons.HOME
        else -> null
    }

    fun label(name: String): String = when (name) {
        "one" -> "1"
        "two" -> "2"
        "plus" -> "+"
        "minus" -> "−"
        "home" -> "⌂"
        "start" -> "START"
        "select" -> "SELECT"
        else -> name.uppercase()
    }
}

/** Applies a frame of skin input to [InputHub] and fires actions on edges. */
class SkinInputState(private val actions: SkinActions, private val onPress: () -> Unit) {
    private var pressed = emptySet<String>()
    var holdMode = false

    fun update(names: Set<String>, stick: PointF?, cStick: PointF?, touch: Pair<SkinItem, PointF>?, console: Console) {
        val added = names - pressed
        val removed = pressed - names
        if (added.isNotEmpty()) onPress()
        for (n in added) when (n) {
            "menu" -> actions.menu()
            "quickSave" -> actions.quickSave()
            "quickLoad" -> actions.quickLoad()
            "toggleFastForward" -> actions.toggleFastForward()
            "fastForward" -> actions.setFastForward(true)
            "rewind" -> actions.setRewinding(true)
            else -> if (holdMode) SkinInputMap.bit(n)?.let { InputHub.heldButtons = InputHub.heldButtons xor it }
        }
        for (n in removed) when (n) {
            "fastForward" -> actions.setFastForward(false)
            "rewind" -> actions.setRewinding(false)
        }
        pressed = names
        InputHub.touchButtons = if (holdMode) 0 else names.mapNotNull(SkinInputMap::bit).fold(0) { a, b -> a or b }
        InputHub.overlayStickX = stick?.x ?: 0f
        InputHub.overlayStickY = stick?.y ?: 0f
        InputHub.cStickX = cStick?.x ?: 0f
        InputHub.cStickY = cStick?.y ?: 0f
        when {
            touch == null -> {
                InputHub.touchX = -1
                InputHub.touchY = -1
            }
            else -> {
                InputHub.touchX = (touch.second.x * 255f).toInt()
                InputHub.touchY = (touch.second.y * 191f).toInt()
            }
        }
        InputHub.push()
    }

    fun releaseAll(console: Console) = update(emptySet(), null, null, null, console)
}

private sealed interface Capture {
    data class Stick(val item: SkinItem) : Capture
    data class Touch(val item: SkinItem) : Capture
    data object Free : Capture
}

/**
 * Draws a controller skin and turns touches into input. Every finger is
 * tracked: sliding across buttons presses what is under the finger now,
 * a touch that starts on a thumbstick or touchscreen keeps controlling it.
 */
@Composable
fun ControllerSkinView(
    layout: SkinLayout,
    console: Console,
    opacity: Float,
    input: SkinInputState,
    modifier: Modifier = Modifier,
) {
    val pressed = remember { mutableStateMapOf<String, Boolean>() }
    val sticks = remember { mutableStateMapOf<SkinItem, Offset>() }
    var art by remember { mutableStateOf<ImageBitmap?>(null) }
    val asset = layout.rep.asset
    LaunchedEffect(asset, layout.viewW, layout.viewH) {
        art = asset?.let {
            val r = layout.skinRect
            withContext(Dispatchers.IO) { SkinRepository.renderAsset(it, r.width().toInt(), r.height().toInt())?.asImageBitmap() }
        }
    }
    val text = rememberTextMeasurer()

    Canvas(
        modifier.fillMaxSize().pointerInput(layout, console) {
            val captures = mutableMapOf<PointerId, Capture>()
            awaitPointerEventScope {
                while (true) {
                    val ev = awaitPointerEvent()
                    for (c in ev.changes) {
                        if (c.pressed && !c.previousPressed) {
                            val hit = layout.itemsAt(c.position.x, c.position.y)
                            captures[c.id] = hit.firstOrNull { it.inputs is SkinInputs.Thumbstick }?.let { Capture.Stick(it) }
                                ?: hit.firstOrNull { it.inputs is SkinInputs.Touchscreen }?.let { Capture.Touch(it) }
                                ?: Capture.Free
                        }
                    }
                    val names = mutableSetOf<String>()
                    var stick: PointF? = null
                    var cStick: PointF? = null
                    var touch: Pair<SkinItem, PointF>? = null
                    sticks.clear()
                    for (c in ev.changes.filter { it.pressed }) {
                        when (val cap = captures[c.id] ?: Capture.Free) {
                            is Capture.Stick -> {
                                val v = layout.relative(cap.item, c.position.x, c.position.y)
                                val t = cap.item.inputs as SkinInputs.Thumbstick
                                sticks[cap.item] = Offset(v.x, -v.y)
                                when {
                                    t.up.startsWith("analogStick") -> stick = v
                                    t.up.startsWith("cStick") -> cStick = v
                                    else -> names += dpadDirections(SkinInputs.DPad(t.up, t.down, t.left, t.right), v)
                                }
                            }
                            is Capture.Touch -> touch = cap.item to layout.fraction(cap.item, c.position.x, c.position.y)
                            Capture.Free -> for (item in layout.itemsAt(c.position.x, c.position.y)) {
                                when (val inp = item.inputs) {
                                    is SkinInputs.Buttons -> names += inp.names
                                    is SkinInputs.DPad -> names += dpadDirections(inp, layout.relative(item, c.position.x, c.position.y))
                                    else -> {}
                                }
                            }
                        }
                    }
                    captures.keys.retainAll(ev.changes.filter { it.pressed }.map { it.id }.toSet())
                    ev.changes.forEach { it.consume() }
                    pressed.keys.retainAll(names)
                    names.forEach { pressed[it] = true }
                    input.update(names, stick, cStick, touch, console)
                }
            }
        },
    ) {
        val skinArt = art
        if (skinArt != null) {
            val r = layout.skinRect
            drawImage(skinArt, dstOffset = androidx.compose.ui.unit.IntOffset(r.left.toInt(), r.top.toInt()), dstSize = IntSize(r.width().toInt(), r.height().toInt()), alpha = opacity)
        } else if (layout.rep.asset == null) {
            drawProcedural(layout, console, opacity, pressed, sticks, text)
        }
    }
}

private fun DrawScope.drawProcedural(
    layout: SkinLayout,
    console: Console,
    opacity: Float,
    pressed: Map<String, Boolean>,
    sticks: Map<SkinItem, Offset>,
    text: TextMeasurer,
) {
    val rep = layout.rep
    if (!rep.translucent) {
        // Controller deck below the screens: night sky fading into deep sea.
        val top = rep.screens.mapNotNull { it.output?.let(layout::toView)?.bottom }.maxOrNull() ?: 0f
        drawRect(
            Brush.verticalGradient(listOf(Island.Night, Color(0xFF13202C)), startY = top, endY = size.height),
            topLeft = Offset(0f, top),
            size = Size(size.width, size.height - top),
        )
    }
    val fill = Color.White.copy(alpha = 0.16f * opacity + 0.06f)
    val edge = Color.White.copy(alpha = 0.55f * opacity)
    val ink = Color.White.copy(alpha = 0.9f * opacity.coerceAtLeast(0.35f))
    val accentA = when (console) {
        Console.NDS -> Island.Sky
        Console.GBA -> Island.Lagoon
    }
    for (item in rep.items) {
        val r = layout.toView(item.frame)
        val tl = Offset(r.left, r.top)
        val sz = Size(r.width(), r.height())
        when (val inp = item.inputs) {
            is SkinInputs.DPad -> {
                val arm = min(sz.width, sz.height) * 0.34f
                val cx = r.centerX()
                val cy = r.centerY()
                val hRect = Size(sz.width, arm)
                val vRect = Size(arm, sz.height)
                drawRoundRect(fill, Offset(r.left, cy - arm / 2), hRect, CornerRadius(arm / 3))
                drawRoundRect(fill, Offset(cx - arm / 2, r.top), vRect, CornerRadius(arm / 3))
                drawRoundRect(edge, Offset(r.left, cy - arm / 2), hRect, CornerRadius(arm / 3), style = Stroke(2f))
                drawRoundRect(edge, Offset(cx - arm / 2, r.top), vRect, CornerRadius(arm / 3), style = Stroke(2f))
                val hi = Island.Sand.copy(alpha = 0.55f)
                if (pressed[inp.up] == true) drawRoundRect(hi, Offset(cx - arm / 2, r.top), Size(arm, sz.height / 2), CornerRadius(arm / 3))
                if (pressed[inp.down] == true) drawRoundRect(hi, Offset(cx - arm / 2, cy), Size(arm, sz.height / 2), CornerRadius(arm / 3))
                if (pressed[inp.left] == true) drawRoundRect(hi, Offset(r.left, cy - arm / 2), Size(sz.width / 2, arm), CornerRadius(arm / 3))
                if (pressed[inp.right] == true) drawRoundRect(hi, Offset(cx, cy - arm / 2), Size(sz.width / 2, arm), CornerRadius(arm / 3))
                for ((angle, _) in listOf(0f to inp.up, 90f to inp.right, 180f to inp.down, 270f to inp.left)) {
                    rotate(angle, Offset(cx, cy)) {
                        val tip = cy - sz.height * 0.42f
                        val p = Path().apply {
                            moveTo(cx, tip)
                            lineTo(cx - arm * 0.22f, tip + arm * 0.3f)
                            lineTo(cx + arm * 0.22f, tip + arm * 0.3f)
                            close()
                        }
                        drawPath(p, ink)
                    }
                }
            }
            is SkinInputs.Thumbstick -> {
                val rad = min(sz.width, sz.height) / 2
                drawCircle(fill, rad, Offset(r.centerX(), r.centerY()))
                drawCircle(edge, rad, Offset(r.centerX(), r.centerY()), style = Stroke(2f))
                val off = sticks[item] ?: Offset.Zero
                val knob = Offset(r.centerX() + off.x * rad * 0.55f, r.centerY() + off.y * rad * 0.55f)
                val knobColor = if (inp.up.startsWith("cStick")) Color(0xFFF2D25A) else Island.Sand
                drawCircle(knobColor.copy(alpha = 0.75f * opacity + 0.15f), rad * 0.45f, knob)
            }
            is SkinInputs.Touchscreen -> {}
            is SkinInputs.Buttons -> {
                val name = inp.names.firstOrNull() ?: continue
                val down = inp.names.any { pressed[it] == true } || (SkinInputMap.bit(name)?.let { InputHub.heldButtons and it != 0 } ?: false)
                val wide = sz.width > sz.height * 1.3f
                val base = when {
                    name == "a" -> accentA.copy(alpha = 0.55f * opacity + 0.1f)
                    else -> fill
                }
                val color = if (down) Island.Sand.copy(alpha = 0.7f) else base
                if (wide) {
                    val cr = CornerRadius(sz.height / 2)
                    drawRoundRect(color, tl, sz, cr)
                    drawRoundRect(edge, tl, sz, cr, style = Stroke(2f))
                } else {
                    val rad = min(sz.width, sz.height) / 2
                    drawCircle(color, rad, Offset(r.centerX(), r.centerY()))
                    drawCircle(edge, rad, Offset(r.centerX(), r.centerY()), style = Stroke(2f))
                }
                val c = Offset(r.centerX(), r.centerY())
                val u = min(sz.width, sz.height)
                when (name) {
                    "menu" -> for (k in -1..1) drawLine(ink, Offset(c.x - u * 0.22f, c.y + k * u * 0.16f), Offset(c.x + u * 0.22f, c.y + k * u * 0.16f), 3f, StrokeCap.Round)
                    "toggleFastForward", "fastForward" -> triangles(c, u, ink, forward = true)
                    "rewind" -> triangles(c, u, ink, forward = false)
                    "quickSave", "quickLoad" -> label(text, if (name == "quickSave") "QS" else "QL", c, u * 0.32f, ink)
                    else -> label(text, SkinInputMap.label(name), c, if (wide) sz.height * 0.38f else u * 0.42f, ink)
                }
            }
        }
    }
}

private fun DrawScope.triangles(c: Offset, u: Float, ink: Color, forward: Boolean) {
    val s = u * 0.18f
    for (k in listOf(-1, 1)) {
        val x0 = c.x + k * s * 0.9f - (if (forward) s * 0.5f else -s * 0.5f)
        val tip = if (forward) x0 + s else x0 - s
        drawPath(Path().apply { moveTo(x0, c.y - s); lineTo(tip, c.y); lineTo(x0, c.y + s); close() }, ink)
    }
}

private fun DrawScope.label(text: TextMeasurer, s: String, c: Offset, px: Float, ink: Color) {
    val style = TextStyle(color = ink, fontSize = (px / density).coerceIn(6f, 40f).sp, fontWeight = FontWeight.Bold)
    val m = text.measure(s, style)
    drawText(m, topLeft = Offset(c.x - m.size.width / 2f, c.y - m.size.height / 2f))
}
