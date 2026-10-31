package com.kdemulator

import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.RomEntry
import com.kdemulator.data.RomRepository
import com.kdemulator.input.HardwareGamepadManager
import com.kdemulator.ui.emulation.EmulationSurfaceScreen
import com.kdemulator.ui.library.RomLibraryScreen
import com.kdemulator.ui.theme.IslandTheme

class MainActivity : ComponentActivity() {
    private lateinit var repo: RomRepository

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        repo = RomRepository(this)
        val summary = NativeEmulatorBridge.nativeInit(cacheDir.absolutePath)

        setContent {
            IslandTheme {
                var playing by remember { mutableStateOf<Pair<RomEntry, Console>?>(null) }
                var error by remember { mutableStateOf<String?>(null) }
                val current = playing
                if (current == null) {
                    immersive(false)
                    RomLibraryScreen(repo, error ?: summary) { entry ->
                        val console = repo.load(entry)
                        if (console != null) {
                            error = null
                            playing = entry to console
                        } else {
                            error = "Could not start ${entry.fileName} — see logcat (tag KD-Emulator)."
                        }
                    }
                } else {
                    immersive(true)
                    BackHandler { playing = null }
                    EmulationSurfaceScreen(current.first, current.second, repo) { playing = null }
                }
            }
        }
    }

    private fun immersive(on: Boolean) {
        val controller = WindowCompat.getInsetsController(window, window.decorView)
        if (on) {
            controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            controller.hide(WindowInsetsCompat.Type.systemBars())
        } else {
            controller.show(WindowInsetsCompat.Type.systemBars())
        }
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean =
        HardwareGamepadManager.onKeyEvent(event) || super.dispatchKeyEvent(event)

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean =
        HardwareGamepadManager.onMotionEvent(event) || super.dispatchGenericMotionEvent(event)
}
