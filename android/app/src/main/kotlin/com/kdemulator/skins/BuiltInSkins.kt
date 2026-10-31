package com.kdemulator.skins

import android.graphics.RectF
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import kotlin.math.min

/**
 * The default "Island" skins, laid out for the actual screen (in dp) so they
 * fit every phone in both orientations. Portrait puts the game screen(s) on
 * top and the controls underneath; landscape centres the game and floats
 * translucent controls at the sides.
 */
object BuiltInSkins {
    fun skin(console: Console) = ControllerSkin("builtin.${console.name.lowercase()}", "Island", console, builtIn = true)

    private val DPAD = SkinInputs.DPad("up", "down", "left", "right")
    private val PAD = Edges.all(10f)

    private fun box(cx: Float, cy: Float, w: Float, h: Float = w) = RectF(cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)
    private fun btn(name: String, r: RectF, edges: Edges = PAD) = SkinItem(SkinInputs.Buttons(listOf(name)), r, edges)

    fun layout(console: Console, w: Float, h: Float, landscape: Boolean, rewind: Boolean): SkinRepresentation =
        if (landscape) landscape(console, w, h, rewind) else portrait(console, w, h, rewind)

    private fun screens(console: Console, area: RectF): List<SkinScreen> =
        if (console == Console.NDS) {
            val half = area.height() / 2f
            listOf(
                SkinScreen(RectF(0f, 0f, 256f, 192f), RectF(area.left, area.top, area.right, area.top + half)),
                SkinScreen(RectF(0f, 192f, 256f, 384f), RectF(area.left, area.top + half, area.right, area.bottom)),
            )
        } else {
            listOf(SkinScreen(null, area))
        }

    private fun touchItems(console: Console, screens: List<SkinScreen>): List<SkinItem> = when (console) {
        Console.NDS -> listOf(SkinItem(SkinInputs.Touchscreen, RectF(screens[1].output!!), Edges.ZERO))
        Console.GBA -> emptyList()
    }

    /** Face buttons around (cx, cy) for a cluster of size d. */
    private fun face(console: Console, cx: Float, cy: Float, d: Float): List<SkinItem> = when (console) {
        Console.GBA -> listOf(
            btn("a", box(cx + d * 0.22f, cy - d * 0.12f, d * 0.4f)),
            btn("b", box(cx - d * 0.22f, cy + d * 0.12f, d * 0.4f)),
        )
        Console.NDS -> listOf(
            btn("x", box(cx, cy - d * 0.33f, d * 0.31f)),
            btn("a", box(cx + d * 0.33f, cy, d * 0.31f)),
            btn("b", box(cx, cy + d * 0.33f, d * 0.31f)),
            btn("y", box(cx - d * 0.33f, cy, d * 0.31f)),
        )
    }

    private fun shoulders(l: RectF, r: RectF): List<SkinItem> = listOf(btn("l", l), btn("r", r))

    private fun portrait(console: Console, w: Float, h: Float, rewind: Boolean): SkinRepresentation {
        val fw = console.screenWidth.toFloat()
        val fh = console.screenHeight.toFloat()
        var sh = w * fh / fw
        val maxScreen = h * if (console == Console.NDS) 0.6f else 0.46f
        if (sh > maxScreen) sh = maxScreen
        val sw = sh * fw / fh
        val screenArea = RectF((w - sw) / 2, 0f, (w + sw) / 2, sh)
        val scr = screens(console, screenArea)
        val y0 = sh
        val ch = h - y0
        val items = mutableListOf<SkinItem>()
        items += touchItems(console, scr)

        val shoulderY = y0 + 34f
        items += shoulders(box(w * 0.16f, shoulderY, w * 0.27f, 40f), box(w * 0.84f, shoulderY, w * 0.27f, 40f))
        items += btn("toggleFastForward", box(w * 0.5f + 28f, shoulderY, 40f))
        if (rewind) items += btn("rewind", box(w * 0.5f - 28f, shoulderY, 40f))

        val bottomY = h - 34f
        val d = min(w * 0.38f, (bottomY - shoulderY - 60f) * 0.9f)
        val midY = (shoulderY + 24f + bottomY - 24f) / 2f
        val left = box(w * 0.25f, midY, d)
        items += SkinItem(DPAD, left, PAD)
        items += face(console, w * 0.75f, midY, d)
        items += btn("select", box(w * 0.5f - 66f, bottomY, 64f, 30f))
        items += btn("menu", box(w * 0.5f, bottomY, 40f))
        items += btn("start", box(w * 0.5f + 66f, bottomY, 64f, 30f))
        return SkinRepresentation(w, h, items, scr, Edges.all(8f), translucent = false, asset = null)
    }

    private fun landscape(console: Console, w: Float, h: Float, rewind: Boolean): SkinRepresentation {
        val fw = console.screenWidth.toFloat()
        val fh = console.screenHeight.toFloat()
        val s = min(w * 0.62f / fw, h / fh)
        val screenArea = RectF((w - fw * s) / 2, (h - fh * s) / 2, (w + fw * s) / 2, (h + fh * s) / 2)
        val scr = screens(console, screenArea)
        val items = mutableListOf<SkinItem>()
        items += touchItems(console, scr)
        // Side columns hold the controls; keep them clear of the game screen.
        val side = (w - fw * s) / 2f
        val d = min(h * 0.42f, side * 0.85f)
        items += shoulders(box(side * 0.5f, 30f, side * 0.8f, 40f), box(w - side * 0.5f, 30f, side * 0.8f, 40f))
        val midY = h * 0.6f
        items += SkinItem(DPAD, box(side * 0.5f, midY, d), PAD)
        items += face(console, w - side * 0.5f, midY, d)
        items += btn("menu", box(w / 2f, 22f, 34f))
        items += btn("toggleFastForward", box(w / 2f + 44f, 22f, 34f))
        if (rewind) items += btn("rewind", box(w / 2f - 44f, 22f, 34f))
        items += btn("select", box(side * 0.45f, h - 24f, 60f, 28f))
        items += btn("start", box(w - side * 0.45f, h - 24f, 60f, 28f))
        return SkinRepresentation(w, h, items, scr, Edges.all(8f), translucent = true, asset = null)
    }
}
