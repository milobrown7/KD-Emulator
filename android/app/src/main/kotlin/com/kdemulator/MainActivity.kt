package com.kdemulator

import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.AppServices
import com.kdemulator.data.Game
import com.kdemulator.input.HardwareGamepadManager
import com.kdemulator.ui.game.CheatsSheet
import com.kdemulator.ui.game.GameScreen
import com.kdemulator.ui.library.LaunchMode
import com.kdemulator.ui.library.LibraryScreen
import com.kdemulator.ui.settings.SettingsScreen
import com.kdemulator.ui.settings.SkinPickerScreen
import com.kdemulator.ui.theme.IslandTheme

/** Screens of the app. */
sealed interface Route {
    data object Library : Route
    data class Playing(val game: Game, val resume: Boolean) : Route
    data object Settings : Route
    data class Skins(val console: Console) : Route
    data class Cheats(val game: Game) : Route
}

class MainActivity : ComponentActivity() {
    private val services by lazy { AppServices(applicationContext) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val summary = NativeEmulatorBridge.nativeInit(cacheDir.absolutePath)
        setContent {
            IslandTheme {
                App(services, summary, ::immersive)
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

@Composable
fun App(services: AppServices, deviceSummary: String, immersive: (Boolean) -> Unit) {
    var route by remember { mutableStateOf<Route>(Route.Library) }
    var message by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(route) { immersive(route is Route.Playing) }

    when (val r = route) {
        Route.Library -> LibraryScreen(
            services = services,
            deviceSummary = deviceSummary,
            message = message,
            onMessageShown = { message = null },
            onPlay = { game, mode ->
                if (services.library.boot(game)) {
                    val resume = when (mode) {
                        LaunchMode.RESUME -> true
                        LaunchMode.FRESH -> false
                        LaunchMode.NORMAL -> services.settings.state.value.autoResume
                    }
                    route = Route.Playing(game, resume)
                } else {
                    message = "Couldn't start ${game.title}. If it's a disc on removable storage, check it's still there."
                }
            },
            onSettings = { route = Route.Settings },
            onCheats = { route = Route.Cheats(it) },
        )
        is Route.Playing -> GameScreen(services, r.game, r.resume) { route = Route.Library }
        Route.Settings -> SettingsScreen(services, deviceSummary, onSkins = { route = Route.Skins(it) }) { route = Route.Library }
        is Route.Skins -> SkinPickerScreen(services, r.console) { route = Route.Settings }
        is Route.Cheats -> {
            BackHandler { route = Route.Library }
            CheatsSheet(services, r.game) { route = Route.Library }
        }
    }
}
