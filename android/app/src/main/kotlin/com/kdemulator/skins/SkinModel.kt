package com.kdemulator.skins

import android.graphics.PointF
import android.graphics.RectF
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min

/** Extra touch margin around an item, in mapping units. */
data class Edges(val top: Float = 0f, val bottom: Float = 0f, val left: Float = 0f, val right: Float = 0f) {
    companion object {
        val ZERO = Edges()
        fun all(v: Float) = Edges(v, v, v, v)

        fun parse(o: JSONObject?): Edges? = o?.let {
            Edges(it.optDouble("top", 0.0).toFloat(), it.optDouble("bottom", 0.0).toFloat(), it.optDouble("left", 0.0).toFloat(), it.optDouble("right", 0.0).toFloat())
        }
    }
}

/** What an item does when touched. */
sealed interface SkinInputs {
    /** One or more named inputs pressed together (`["a"]`, `["a","b"]`, `["menu"]`). */
    data class Buttons(val names: List<String>) : SkinInputs

    /** Eight-way directional pad. */
    data class DPad(val up: String, val down: String, val left: String, val right: String) : SkinInputs

    /** Analog stick: the touch that starts on it drags the stick. */
    data class Thumbstick(val up: String, val down: String, val left: String, val right: String, val image: String?) : SkinInputs

    /** DS touchscreen area. */
    data object Touchscreen : SkinInputs
}

data class SkinItem(val inputs: SkinInputs, val frame: RectF, val extended: Edges?, val thumbstickSize: Pair<Float, Float>? = null)

/**
 * Where a part of the game frame appears. [input] is in game pixels (null =
 * the whole frame); [output] is in mapping units (null = automatic).
 */
data class SkinScreen(val input: RectF?, val output: RectF?)

/** One device/orientation layout of a skin, in its own mapping units. */
data class SkinRepresentation(
    val mappingWidth: Float,
    val mappingHeight: Float,
    val items: List<SkinItem>,
    val screens: List<SkinScreen>,
    val extended: Edges,
    val translucent: Boolean,
    /** PDF or PNG artwork (imported skins); null = drawn procedurally. */
    val asset: File?,
)

/**
 * Placement of a representation inside a view of [viewW]x[viewH] pixels:
 * the skin is scaled to fit and anchored to the bottom, like Delta.
 */
class SkinLayout(val rep: SkinRepresentation, val viewW: Float, val viewH: Float, private val frameW: Int, private val frameH: Int) {
    val scale = min(viewW / rep.mappingWidth, viewH / rep.mappingHeight)
    val offsetX = (viewW - rep.mappingWidth * scale) / 2f
    val offsetY = viewH - rep.mappingHeight * scale

    fun toView(r: RectF) = RectF(offsetX + r.left * scale, offsetY + r.top * scale, offsetX + r.right * scale, offsetY + r.bottom * scale)

    fun toMapping(x: Float, y: Float) = PointF((x - offsetX) / scale, (y - offsetY) / scale)

    /** Skin artwork bounds in view pixels. */
    val skinRect get() = RectF(offsetX, offsetY, offsetX + rep.mappingWidth * scale, viewH)

    /**
     * (source rect in frame pixels — null for the whole frame, destination
     * rect in view pixels) for each screen.
     */
    fun screenRects(): List<Pair<RectF?, RectF>> {
        val explicit = rep.screens.filter { it.output != null }
        if (explicit.isNotEmpty()) return explicit.map { it.input to toView(it.output!!) }
        // No screen frames: fit the whole frame into the space the skin leaves free.
        val area = if (rep.translucent || offsetY < viewH * 0.15f) RectF(0f, 0f, viewW, viewH) else RectF(0f, 0f, viewW, offsetY)
        return listOf(null to fit(frameW.toFloat(), frameH.toFloat(), area))
    }

    /** Items the point (view pixels) touches, preferring exact hits over extended margins. */
    fun itemsAt(x: Float, y: Float): List<SkinItem> {
        val p = toMapping(x, y)
        val exact = rep.items.filter { it.frame.contains(p.x, p.y) }
        if (exact.isNotEmpty()) return exact
        val near = rep.items.filter { grow(it.frame, it.extended ?: rep.extended).contains(p.x, p.y) }
        return near.minByOrNull { hypot(it.frame.centerX() - p.x, it.frame.centerY() - p.y) }?.let { listOf(it) } ?: emptyList()
    }

    /** Normalised (-1..1) position of a view point inside an item, y up. */
    fun relative(item: SkinItem, x: Float, y: Float): PointF {
        val p = toMapping(x, y)
        val f = item.frame
        return PointF(((p.x - f.centerX()) / (f.width() / 2f)).coerceIn(-1f, 1f), (-(p.y - f.centerY()) / (f.height() / 2f)).coerceIn(-1f, 1f))
    }

    /** Fractional position (0..1) inside an item, y down. */
    fun fraction(item: SkinItem, x: Float, y: Float): PointF {
        val p = toMapping(x, y)
        val f = item.frame
        return PointF(((p.x - f.left) / f.width()).coerceIn(0f, 1f), ((p.y - f.top) / f.height()).coerceIn(0f, 1f))
    }

    companion object {
        fun grow(r: RectF, e: Edges) = RectF(r.left - e.left, r.top - e.top, r.right + e.right, r.bottom + e.bottom)

        /** Largest rect with aspect [w]:[h] centred in [area]. */
        fun fit(w: Float, h: Float, area: RectF): RectF {
            val s = min(area.width() / w, area.height() / h)
            val cx = area.centerX()
            val cy = area.centerY()
            return RectF(cx - w * s / 2f, cy - h * s / 2f, cx + w * s / 2f, cy + h * s / 2f)
        }
    }
}

/** Inputs pressed by a d-pad touch at normalised position [p] (y up). */
fun dpadDirections(d: SkinInputs.DPad, p: PointF, deadZone: Float = 0.3f): List<String> {
    if (abs(p.x) < deadZone && abs(p.y) < deadZone) return emptyList()
    val out = mutableListOf<String>()
    // Diagonals when the touch is closer to 45° than to an axis.
    val ax = abs(p.x)
    val ay = abs(p.y)
    val diagonal = min(ax, ay) > max(ax, ay) * 0.414f
    if (p.y > 0 && (ay >= ax || diagonal)) out += d.up
    if (p.y < 0 && (ay >= ax || diagonal)) out += d.down
    if (p.x < 0 && (ax >= ay || diagonal)) out += d.left
    if (p.x > 0 && (ax >= ay || diagonal)) out += d.right
    return out
}

/** A controller skin: built-in or imported (`.deltaskin`). */
data class ControllerSkin(
    val id: String,
    val name: String,
    val console: Console,
    val builtIn: Boolean,
    /** Imported skins: representations keyed by "portrait"/"landscape". */
    val representations: Map<String, SkinRepresentation> = emptyMap(),
) {
    fun representation(landscape: Boolean, widthDp: Float, heightDp: Float, rewind: Boolean): SkinRepresentation? =
        if (builtIn) {
            BuiltInSkins.layout(console, widthDp, heightDp, landscape, rewind)
        } else {
            representations[if (landscape) "landscape" else "portrait"] ?: representations.values.firstOrNull()
        }

    companion object {
        /** Delta game type identifiers (plus KD-Emulator's own aliases). */
        fun consoleForGameType(id: String): Console? = when (id) {
            "com.rileytestut.delta.game.gba", "com.kdemulator.game.gba" -> Console.GBA
            "com.rileytestut.delta.game.ds", "com.kdemulator.game.ds" -> Console.NDS
            else -> null
        }

        private fun rect(o: JSONObject?): RectF? = o?.let {
            val x = it.optDouble("x", 0.0).toFloat()
            val y = it.optDouble("y", 0.0).toFloat()
            RectF(x, y, x + it.optDouble("width", 0.0).toFloat(), y + it.optDouble("height", 0.0).toFloat())
        }

        private fun inputs(v: Any?): SkinInputs? = when (v) {
            is JSONArray -> SkinInputs.Buttons((0 until v.length()).map { v.getString(it) })
            is JSONObject -> when {
                v.has("x") && v.has("y") -> SkinInputs.Touchscreen
                v.has("up") -> {
                    val up = v.getString("up")
                    val down = v.optString("down")
                    val left = v.optString("left")
                    val right = v.optString("right")
                    if (up.startsWith("analogStick") || up.startsWith("cStick")) SkinInputs.Thumbstick(up, down, left, right, null)
                    else SkinInputs.DPad(up, down, left, right)
                }
                else -> null
            }
            else -> null
        }

        /** Parse one representation; [dir] holds the skin's unpacked files. */
        fun parseRepresentation(o: JSONObject, dir: File?): SkinRepresentation? {
            val mapping = o.optJSONObject("mappingSize") ?: return null
            val items = o.optJSONArray("items")?.let { arr ->
                (0 until arr.length()).mapNotNull { i ->
                    val it = arr.getJSONObject(i)
                    var inp = inputs(it.opt("inputs")) ?: return@mapNotNull null
                    val thumb = it.optJSONObject("thumbstick")
                    if (thumb != null && inp is SkinInputs.DPad) inp = SkinInputs.Thumbstick(inp.up, inp.down, inp.left, inp.right, thumb.optString("name"))
                    if (thumb != null && inp is SkinInputs.Thumbstick) inp = inp.copy(image = thumb.optString("name"))
                    SkinItem(
                        inp,
                        rect(it.optJSONObject("frame")) ?: return@mapNotNull null,
                        Edges.parse(it.optJSONObject("extendedEdges")),
                        thumb?.let { t -> t.optDouble("width", 0.0).toFloat() to t.optDouble("height", 0.0).toFloat() },
                    )
                }
            }.orEmpty()
            val screens = o.optJSONArray("screens")?.let { arr ->
                (0 until arr.length()).map { i ->
                    val s = arr.getJSONObject(i)
                    SkinScreen(rect(s.optJSONObject("inputFrame")), rect(s.optJSONObject("outputFrame")))
                }
            } ?: listOfNotNull(rect(o.optJSONObject("gameScreenFrame"))?.let { SkinScreen(null, it) })
            val assets = o.optJSONObject("assets")
            val assetName = assets?.let { a -> listOf("resizable", "large", "medium", "small").firstNotNullOfOrNull { k -> a.optString(k).takeIf { it.isNotEmpty() } } }
            return SkinRepresentation(
                mapping.optDouble("width", 0.0).toFloat(),
                mapping.optDouble("height", 0.0).toFloat(),
                items,
                screens,
                Edges.parse(o.optJSONObject("extendedEdges")) ?: Edges.ZERO,
                o.optBoolean("translucent", false),
                assetName?.let { n -> dir?.let { File(it, n) } },
            )
        }

        /**
         * Parse a skin's `info.json`. Phones use the iPhone layouts (edge-to-
         * edge first), falling back to the iPad ones.
         */
        fun parse(json: String, dir: File?): ControllerSkin? {
            val o = JSONObject(json)
            val console = consoleForGameType(o.optString("gameTypeIdentifier")) ?: return null
            val reps = o.optJSONObject("representations") ?: return null
            val chosen = mutableMapOf<String, SkinRepresentation>()
            for (device in listOf("iphone", "ipad")) {
                val d = reps.optJSONObject(device) ?: continue
                for (display in listOf("edgeToEdge", "standard", "splitView")) {
                    val disp = d.optJSONObject(display) ?: continue
                    for (orientation in listOf("portrait", "landscape")) {
                        if (orientation in chosen) continue
                        disp.optJSONObject(orientation)?.let(fun(r: JSONObject) { parseRepresentation(r, dir)?.let { chosen[orientation] = it } })
                    }
                }
            }
            if (chosen.isEmpty()) return null
            return ControllerSkin(o.optString("identifier", o.optString("name")), o.optString("name", "Imported skin"), console, false, chosen)
        }
    }
}
