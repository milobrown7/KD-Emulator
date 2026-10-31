package com.kdemulator.ui.game

import android.Manifest
import android.content.ContentValues
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.provider.MediaStore
import android.view.HapticFeedbackConstants
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.AppServices
import com.kdemulator.data.Game
import com.kdemulator.data.SaveStateKind
import com.kdemulator.input.InputHub
import com.kdemulator.input.MicRecorder
import com.kdemulator.input.SensorMotionManager
import com.kdemulator.performance.TensorThermalHintManager
import com.kdemulator.skins.ControllerSkinView
import com.kdemulator.skins.SkinActions
import com.kdemulator.skins.SkinInputState
import com.kdemulator.skins.SkinLayout
import com.kdemulator.ui.debugger.MobileDebuggerSheet
import com.kdemulator.ui.theme.Island
import kotlinx.coroutines.delay

/** Overlays on top of the paused game. */
enum class GameOverlay { NONE, MENU, SAVE_STATES, LOAD_STATES, CHEATS, TIME_TRAVEL, DEBUGGER }

/**
 * Playing a game: the frame view, the controller skin and the pause menu.
 * Leaving writes the battery save and an auto save state (with a
 * screenshot) so the game resumes exactly where it was.
 */
@Composable
fun GameScreen(services: AppServices, game: Game, resume: Boolean, onExit: () -> Unit) {
    val context = LocalContext.current
    val view = LocalView.current
    val settings by services.settings.state.collectAsState()
    val thermal = remember { TensorThermalHintManager(context) }
    val sensors = remember { SensorMotionManager(context) }
    val mic = remember { MicRecorder() }
    var gameView by remember { mutableStateOf<GameView?>(null) }
    var overlay by remember { mutableStateOf(GameOverlay.NONE) }
    var fastForward by remember { mutableStateOf(false) }
    var rewinding by remember { mutableStateOf(false) }
    var holdMode by remember { mutableStateOf(false) }
    var micOn by remember { mutableStateOf(false) }
    var toast by remember { mutableStateOf<String?>(null) }
    var acTitle by remember { mutableStateOf<String?>(null) }
    val paused = overlay != GameOverlay.NONE
    val currentSettings by rememberUpdatedState(settings)

    fun snapshot(): Bitmap? = gameView?.let { it.pullFrame(); it.snapshot() }

    fun quickSave() {
        toast = if (services.states.save(game.id, SaveStateKind.QUICK, snapshot()) != null) "Quick saved" else "Couldn't save"
    }

    fun quickLoad() {
        val e = services.states.latest(game.id, SaveStateKind.QUICK)
        toast = if (e != null && services.states.load(e)) "Quick loaded" else "No quick save yet"
    }

    fun applySpeed(on: Boolean) {
        fastForward = on
        NativeEmulatorBridge.nativeSetSpeed(if (on) currentSettings.fastForwardSpeed else 1)
    }

    val input = remember {
        SkinInputState(
            actions = object : SkinActions {
                override fun menu() {
                    overlay = GameOverlay.MENU
                }
                override fun quickSave() = quickSave()
                override fun quickLoad() = quickLoad()
                override fun setFastForward(on: Boolean) = applySpeed(on)
                override fun toggleFastForward() = applySpeed(!fastForward)
                override fun setRewinding(on: Boolean) {
                    rewinding = on
                    NativeEmulatorBridge.nativeSetRewinding(on)
                }
            },
            onPress = {
                if (currentSettings.buttonHaptics) {
                    view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY, HapticFeedbackConstants.FLAG_IGNORE_VIEW_SETTING)
                }
            },
        )
    }
    input.holdMode = holdMode

    val micPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            mic.start()
            micOn = true
        } else {
            toast = "Microphone permission denied"
        }
    }

    fun toggleMic() {
        if (micOn) {
            mic.stop()
            micOn = false
        } else if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            mic.start()
            micOn = true
        } else {
            micPermission.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    // Session lifetime: configure, resume, run; save everything on the way out.
    DisposableEffect(game.id) {
        InputHub.reset()
        NativeEmulatorBridge.nativeSetSpeed(1)
        NativeEmulatorBridge.nativeSetVolume((settings.volume * 256).toInt())
        NativeEmulatorBridge.nativeSetRewindInterval(if (settings.rewindEnabled) (settings.rewindIntervalSec * 60).toInt().coerceAtLeast(1) else 0)
        services.cheats.activate(game.id).takeIf { it.isNotBlank() }?.let { toast = "Some cheats have errors" }
        if (resume) {
            services.states.latest(game.id, SaveStateKind.AUTO)?.let { if (services.states.load(it)) toast = "Resumed where you left off" }
        }
        acTitle = NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGetAnimalCrossingInfo())["title"]
        sensors.start()
        NativeEmulatorBridge.nativeStart()
        thermal.start(if (game.console == Console.GBA) 59.73 else 59.94)
        if (settings.microphone && game.console == Console.NDS &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
        ) {
            mic.start()
            micOn = true
        }
        onDispose {
            NativeEmulatorBridge.nativePause()
            NativeEmulatorBridge.nativeSetRewinding(false)
            services.states.save(game.id, SaveStateKind.AUTO, gameView?.snapshot())
            services.library.writeBackup(game)
            services.library.storeClockOffset(game, NativeEmulatorBridge.nativeGetClockOffset())
            mic.stop()
            sensors.stop()
            thermal.stop()
            InputHub.reset()
            NativeEmulatorBridge.nativeStop()
        }
    }

    // Pause behind menus.
    LaunchedEffect(paused) {
        if (paused) {
            input.releaseAll(game.console)
            NativeEmulatorBridge.nativePause()
            services.library.writeBackup(game)
        } else {
            NativeEmulatorBridge.nativeStart()
        }
    }

    // Background/foreground.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val obs = LifecycleEventObserver { _, e ->
            when (e) {
                Lifecycle.Event.ON_STOP -> {
                    NativeEmulatorBridge.nativePause()
                    services.library.writeBackup(game)
                }
                Lifecycle.Event.ON_START -> if (overlay == GameOverlay.NONE) NativeEmulatorBridge.nativeStart() else overlay = GameOverlay.MENU
                else -> {}
            }
        }
        lifecycle.addObserver(obs)
        onDispose { lifecycle.removeObserver(obs) }
    }

    LaunchedEffect(settings.volume) { NativeEmulatorBridge.nativeSetVolume((settings.volume * 256).toInt()) }
    LaunchedEffect(toast) {
        if (toast != null) {
            delay(1800)
            toast = null
        }
    }
    LaunchedEffect(acTitle) {
        if (acTitle != null) {
            delay(4000)
            acTitle = null
        }
    }

    BackHandler {
        overlay = when (overlay) {
            GameOverlay.NONE -> GameOverlay.MENU
            GameOverlay.MENU -> GameOverlay.NONE
            else -> GameOverlay.MENU
        }
    }

    BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black)) {
        val density = LocalDensity.current
        val wPx = with(density) { maxWidth.toPx() }
        val hPx = with(density) { maxHeight.toPx() }
        val landscape = maxWidth > maxHeight
        val skin = remember(settings.skins[game.console]) { services.skins.find(game.console, settings.skins[game.console]) }
        val rep = remember(skin, landscape, maxWidth, maxHeight, settings.rewindEnabled) {
            skin.representation(landscape, maxWidth.value, maxHeight.value, settings.rewindEnabled)
                ?: com.kdemulator.skins.BuiltInSkins.layout(game.console, maxWidth.value, maxHeight.value, landscape, settings.rewindEnabled)
        }
        val layout = remember(rep, wPx, hPx) { SkinLayout(rep, wPx, hPx, game.console.screenWidth, game.console.screenHeight) }

        Box(Modifier.fillMaxSize().then(if (paused) Modifier.blur(16.dp) else Modifier)) {
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { ctx -> GameView(ctx).also { gv -> gameView = gv; gv.onVsync = { thermal.reportFrame() } } },
                update = { gv ->
                    gv.screens = layout.screenRects()
                    gv.smooth = settings.smoothScaling
                },
            )
            ControllerSkinView(layout, game.console, settings.skinOpacity, input)
        }

        Row(Modifier.align(Alignment.TopCenter).padding(top = 10.dp)) {
            AnimatedVisibility(fastForward, enter = fadeIn(), exit = fadeOut()) { Pill("⏩ ${settings.fastForwardSpeed}×") }
            if (fastForward && rewinding) Spacer(Modifier.width(8.dp))
            AnimatedVisibility(rewinding, enter = fadeIn(), exit = fadeOut()) { Pill("⏪ Rewinding") }
        }
        AnimatedVisibility(acTitle != null, Modifier.align(Alignment.TopStart).padding(12.dp), enter = fadeIn(), exit = fadeOut()) {
            Pill("🍃 ${acTitle.orEmpty()} — time travel is in the menu", Island.DeepLeaf.copy(alpha = 0.85f))
        }
        if (holdMode) {
            Box(
                Modifier.align(Alignment.TopCenter).padding(top = 48.dp)
                    .background(Island.Sunset.copy(alpha = 0.92f), RoundedCornerShape(20.dp))
                    .clickable { holdMode = false }
                    .padding(horizontal = 16.dp, vertical = 10.dp),
            ) { Text("Tap buttons to hold them · tap here when done", color = Color.Black, fontWeight = FontWeight.SemiBold) }
        }
        toast?.let { Box(Modifier.align(Alignment.Center)) { Pill(it) } }

        when (overlay) {
            GameOverlay.NONE -> {}
            GameOverlay.MENU -> PauseMenu(
                title = game.title,
                console = game.console,
                isAnimalCrossing = acTitle != null || game.animalCrossing,
                fastForward = fastForward,
                micOn = micOn,
                heldCount = Integer.bitCount(InputHub.heldButtons),
                rewindStates = remember(overlay) { NativeEmulatorBridge.nativeGetRewindAvailable() },
                onResume = { overlay = GameOverlay.NONE },
                onAction = { action ->
                    when (action) {
                        PauseAction.SAVE_STATE -> overlay = GameOverlay.SAVE_STATES
                        PauseAction.LOAD_STATE -> overlay = GameOverlay.LOAD_STATES
                        PauseAction.CHEATS -> overlay = GameOverlay.CHEATS
                        PauseAction.FAST_FORWARD -> {
                            applySpeed(!fastForward)
                            overlay = GameOverlay.NONE
                        }
                        PauseAction.REWIND -> {
                            // Step back ~3 seconds from the paused moment.
                            val steps = (3f / settings.rewindIntervalSec).toInt().coerceAtLeast(1)
                            NativeEmulatorBridge.nativeSetRewinding(true)
                            NativeEmulatorBridge.nativeRunFrames(steps * 3)
                            NativeEmulatorBridge.nativeSetRewinding(false)
                            gameView?.pullFrame()
                            toast = "Rewound"
                            overlay = GameOverlay.NONE
                        }
                        PauseAction.HOLD_BUTTONS -> {
                            InputHub.heldButtons = 0
                            holdMode = true
                            overlay = GameOverlay.NONE
                        }
                        PauseAction.SCREENSHOT -> {
                            toast = if (saveScreenshot(context, game.title, snapshot())) "Screenshot saved to Photos" else "Couldn't save screenshot"
                        }
                        PauseAction.TIME_TRAVEL -> overlay = GameOverlay.TIME_TRAVEL
                        PauseAction.MICROPHONE -> toggleMic()
                        PauseAction.DEBUGGER -> overlay = GameOverlay.DEBUGGER
                        PauseAction.MAIN_MENU -> onExit()
                    }
                },
            )
            GameOverlay.SAVE_STATES, GameOverlay.LOAD_STATES -> SaveStatesSheet(
                services = services,
                game = game,
                saving = overlay == GameOverlay.SAVE_STATES,
                screenshot = { snapshot() },
                onDone = { loaded ->
                    if (loaded) gameView?.pullFrame()
                    overlay = if (loaded) GameOverlay.NONE else GameOverlay.MENU
                },
            )
            GameOverlay.CHEATS -> CheatsSheet(services, game) {
                val errors = services.cheats.activate(game.id)
                if (errors.isNotBlank()) toast = "Some cheats have errors"
                overlay = GameOverlay.MENU
            }
            GameOverlay.TIME_TRAVEL -> TimeTravelDialog(
                isAnimalCrossing = acTitle != null || game.animalCrossing,
                onShift = { NativeEmulatorBridge.nativeTimeTravel(it) },
                onReset = { NativeEmulatorBridge.nativeSetClockOffset(0) },
                onDismiss = {
                    services.library.storeClockOffset(game, NativeEmulatorBridge.nativeGetClockOffset())
                    overlay = GameOverlay.MENU
                },
            )
            GameOverlay.DEBUGGER -> MobileDebuggerSheet(thermal.status.collectAsState().value) { overlay = GameOverlay.MENU }
        }
    }
}

@Composable
private fun Pill(text: String, bg: Color = Color(0xCC000000)) {
    Text(
        text,
        Modifier.background(bg, RoundedCornerShape(16.dp)).padding(horizontal = 14.dp, vertical = 8.dp),
        color = Color.White,
        fontSize = 14.sp,
        fontWeight = FontWeight.SemiBold,
    )
}

/** Save the current frame (nearest-neighbour upscaled) to Pictures/KD-Emulator. */
fun saveScreenshot(context: android.content.Context, title: String, frame: Bitmap?): Boolean {
    frame ?: return false
    val scale = (1080 / maxOf(frame.width, frame.height)).coerceAtLeast(1)
    val big = Bitmap.createScaledBitmap(frame, frame.width * scale, frame.height * scale, false)
    val values = ContentValues().apply {
        put(MediaStore.Images.Media.DISPLAY_NAME, "${title.replace(Regex("[^A-Za-z0-9 _-]"), "")}_${System.currentTimeMillis()}.png")
        put(MediaStore.Images.Media.MIME_TYPE, "image/png")
        put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/KD-Emulator")
    }
    val uri = runCatching { context.contentResolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values) }.getOrNull() ?: return false
    return context.contentResolver.openOutputStream(uri)?.use { big.compress(Bitmap.CompressFormat.PNG, 100, it) } ?: false
}
