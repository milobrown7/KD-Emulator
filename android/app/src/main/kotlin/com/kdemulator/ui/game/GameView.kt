package com.kdemulator.ui.game

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.view.Choreographer
import android.view.View
import com.kdemulator.bridge.NativeEmulatorBridge
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Draws the emulator's frames. Each vsync it asks native code for the
 * newest frame (a straight memcpy into a direct buffer), uploads it into a
 * Bitmap and draws every skin screen (source rect in game pixels →
 * destination rect in view pixels) with GPU scaling — crisp nearest-
 * neighbour by default, bilinear when [smooth] is on.
 */
class GameView(context: Context) : View(context), Choreographer.FrameCallback {
    private var bitmap: Bitmap? = null
    private var buffer: ByteBuffer? = null
    private var seq = 0L
    private val paint = Paint()
    private var running = false

    /** Called once per vsync (Tensor ADPF reporting lives here). */
    var onVsync: (() -> Unit)? = null

    var screens: List<Pair<RectF?, RectF>> = emptyList()
        set(value) {
            field = value
            invalidate()
        }

    var smooth = false
        set(value) {
            field = value
            paint.isFilterBitmap = value
            invalidate()
        }

    init {
        paint.isFilterBitmap = false
    }

    /** The current frame (for save-state thumbnails and screenshots). */
    fun snapshot(): Bitmap? = bitmap?.let { it.copy(Bitmap.Config.ARGB_8888, false) }

    /** Pull the newest frame from native code; true if it changed. */
    fun pullFrame(): Boolean {
        val packed = NativeEmulatorBridge.nativeGetFrameSize()
        val w = packed ushr 16
        val h = packed and 0xFFFF
        if (w == 0 || h == 0) return false
        var bmp = bitmap
        if (bmp == null || bmp.width != w || bmp.height != h) {
            bmp = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
            bitmap = bmp
            buffer = ByteBuffer.allocateDirect(w * h * 4).order(ByteOrder.nativeOrder())
            seq = 0
        }
        val buf = buffer ?: return false
        val s = NativeEmulatorBridge.nativeCopyFrame(buf, seq)
        if (s < 0 || s == seq) return false
        buf.rewind()
        bmp.copyPixelsFromBuffer(buf)
        seq = s
        invalidate()
        return true
    }

    override fun doFrame(frameTimeNanos: Long) {
        if (!running) return
        pullFrame()
        onVsync?.invoke()
        schedule()
    }

    private val tick = Runnable { doFrame(0) }

    private fun schedule() {
        if (vsyncPump) Choreographer.getInstance().postFrameCallback(this) else postDelayed(tick, 16)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        running = true
        schedule()
    }

    override fun onDetachedFromWindow() {
        running = false
        Choreographer.getInstance().removeFrameCallback(this)
        removeCallbacks(tick)
        super.onDetachedFromWindow()
    }

    companion object {
        /**
         * Pull frames on every vsync (devices). JVM UI tests switch to a
         * 16 ms handler pump, because Robolectric delivers a re-armed vsync
         * callback immediately and would spin forever.
         */
        @Volatile
        var vsyncPump = true
    }

    private val src = Rect()

    override fun onDraw(canvas: Canvas) {
        val bmp = bitmap ?: return
        for ((s, d) in screens) {
            if (s == null) src.set(0, 0, bmp.width, bmp.height) else src.set(s.left.toInt(), s.top.toInt(), s.right.toInt(), s.bottom.toInt())
            canvas.drawBitmap(bmp, src, d, paint)
        }
    }
}
