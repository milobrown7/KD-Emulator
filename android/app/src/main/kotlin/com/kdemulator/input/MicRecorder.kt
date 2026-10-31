package com.kdemulator.input

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import com.kdemulator.bridge.NativeEmulatorBridge

/**
 * Feeds the phone's microphone to the DS (blowing on the screen, voice
 * commands). Runs only while the player has the microphone switched on and
 * has granted RECORD_AUDIO.
 */
class MicRecorder {
    @Volatile private var running = false
    private var thread: Thread? = null

    @SuppressLint("MissingPermission") // callers check RECORD_AUDIO first
    fun start() {
        if (running) return
        val rate = 16_000
        val min = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (min <= 0) return
        val rec = runCatching {
            AudioRecord(MediaRecorder.AudioSource.MIC, rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, min * 2)
        }.getOrNull()?.takeIf { it.state == AudioRecord.STATE_INITIALIZED } ?: return
        running = true
        thread = Thread({
            val buf = ShortArray(256)
            rec.startRecording()
            while (running) {
                val n = rec.read(buf, 0, buf.size)
                if (n > 0) NativeEmulatorBridge.nativePushMic(buf, n)
            }
            rec.stop()
            rec.release()
        }, "kd-mic").apply { start() }
    }

    fun stop() {
        running = false
        thread?.join(500)
        thread = null
    }
}
