package com.kdemulator.ui.emulation

import android.view.Choreographer
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.RomEntry
import com.kdemulator.data.RomRepository
import com.kdemulator.input.InputHub
import com.kdemulator.input.SensorMotionManager
import com.kdemulator.performance.TensorThermalHintManager
import com.kdemulator.ui.debugger.MobileDebuggerSheet
import com.kdemulator.ui.overlay.ScreenTouchLayer
import com.kdemulator.ui.overlay.TouchControllerOverlay
import com.kdemulator.ui.overlay.layoutFromNative
import com.kdemulator.ui.theme.Island
import kotlinx.coroutines.delay

private const val DAY = 86_400L
private const val HOUR = 3_600L

@Composable
fun EmulationSurfaceScreen(entry: RomEntry, console: Console, repo: RomRepository, onExit: () -> Unit) {
    val context = LocalContext.current
    val thermal = remember { TensorThermalHintManager(context) }
    val sensors = remember { SensorMotionManager(context) }
    val thermalStatus by thermal.status.collectAsState()
    var acInfo by remember { mutableStateOf(emptyMap<String, String>()) }
    var menu by remember { mutableStateOf(false) }
    var showClock by remember { mutableStateOf(false) }
    var showDebugger by remember { mutableStateOf(false) }
    var toast by remember { mutableStateOf<String?>(null) }

    val layout = layoutFromNative(acInfo["layout"], console)

    DisposableEffect(entry) {
        InputHub.reset()
        acInfo = NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGetAnimalCrossingInfo())
        NativeEmulatorBridge.nativeStart()
        sensors.start()
        thermal.start(if (console == Console.GBA) 59.73 else 59.94)
        val choreographer = Choreographer.getInstance()
        val callback = object : Choreographer.FrameCallback {
            override fun doFrame(frameTimeNanos: Long) {
                thermal.reportFrame()
                choreographer.postFrameCallback(this)
            }
        }
        choreographer.postFrameCallback(callback)
        onDispose {
            choreographer.removeFrameCallback(callback)
            NativeEmulatorBridge.nativePause()
            repo.writeBackup(entry)
            repo.storeClockOffset(entry, NativeEmulatorBridge.nativeGetClockOffset())
            sensors.stop()
            thermal.stop()
            NativeEmulatorBridge.nativeSetSurface(null)
            NativeEmulatorBridge.nativeStop()
        }
    }

    // Keep the time-travel clock readout fresh while the dialog is open.
    LaunchedEffect(showClock) {
        while (showClock) {
            acInfo = NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGetAnimalCrossingInfo())
            delay(1000)
        }
    }
    LaunchedEffect(toast) {
        if (toast != null) {
            delay(1500)
            toast = null
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        Box(
            Modifier.align(Alignment.Center).aspectRatio(console.screenWidth.toFloat() / console.screenHeight),
        ) {
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { ctx ->
                    SurfaceView(ctx).apply {
                        holder.addCallback(object : SurfaceHolder.Callback {
                            override fun surfaceCreated(h: SurfaceHolder) = NativeEmulatorBridge.nativeSetSurface(h.surface)
                            override fun surfaceChanged(h: SurfaceHolder, f: Int, w: Int, hh: Int) =
                                NativeEmulatorBridge.nativeSetSurface(h.surface)
                            override fun surfaceDestroyed(h: SurfaceHolder) = NativeEmulatorBridge.nativeSetSurface(null)
                        })
                    }
                },
            )
            ScreenTouchLayer(layout, Modifier.fillMaxSize())
        }
        TouchControllerOverlay(console, layout)

        Box(Modifier.align(Alignment.TopEnd).padding(12.dp)) {
            FilledTonalButton(onClick = { menu = true }) { Text("☰") }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("Save state") }, onClick = {
                    menu = false
                    toast = if (repo.saveState(entry, 0)) "State saved" else "Save failed"
                })
                DropdownMenuItem(text = { Text("Load state") }, onClick = {
                    menu = false
                    toast = if (repo.loadState(entry, 0)) "State loaded" else "No saved state"
                })
                DropdownMenuItem(text = { Text(if (acInfo.isNotEmpty()) "🍃 Time travel" else "Clock") }, onClick = {
                    menu = false
                    showClock = true
                })
                DropdownMenuItem(text = { Text("Debugger") }, onClick = {
                    menu = false
                    showDebugger = true
                })
                DropdownMenuItem(text = { Text("Quit") }, onClick = {
                    menu = false
                    onExit()
                })
            }
        }

        if (acInfo.isNotEmpty()) {
            Text(
                "🍃 ${acInfo["title"]}",
                Modifier.align(Alignment.TopStart).padding(16.dp).background(Island.accent(acInfo["accent"]).copy(alpha = 0.75f), MaterialTheme.shapes.small)
                    .padding(horizontal = 10.dp, vertical = 4.dp),
                color = Color.White,
                style = MaterialTheme.typography.labelLarge,
            )
        }
        toast?.let {
            Text(
                it,
                Modifier.align(Alignment.Center).background(Color(0xCC000000), MaterialTheme.shapes.small).padding(16.dp),
                color = Color.White,
            )
        }
    }

    if (showClock) {
        TimeTravelDialog(
            isAnimalCrossing = acInfo.isNotEmpty(),
            guestTime = acInfo["clock"],
            onShift = { NativeEmulatorBridge.nativeTimeTravel(it) },
            onReset = { NativeEmulatorBridge.nativeSetClockOffset(0) },
            onDismiss = {
                showClock = false
                repo.storeClockOffset(entry, NativeEmulatorBridge.nativeGetClockOffset())
            },
        )
    }
    if (showDebugger) MobileDebuggerSheet(thermalStatus) { showDebugger = false }
}

/** Shift the guest RTC: seasons, holidays and visitors follow it in Animal Crossing. */
@Composable
private fun TimeTravelDialog(
    isAnimalCrossing: Boolean,
    guestTime: String?,
    onShift: (Long) -> Unit,
    onReset: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = { TextButton(onClick = onDismiss) { Text("Done") } },
        dismissButton = { TextButton(onClick = onReset) { Text("Back to real time") } },
        title = { Text(if (isAnimalCrossing) "🍃 Time travel" else "Console clock") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Game clock: ${guestTime ?: "—"}")
                if (isAnimalCrossing) {
                    Text("Your phone's clock is never changed — only the game's.", style = MaterialTheme.typography.bodyMedium)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilledTonalButton(onClick = { onShift(-HOUR) }) { Text("−1 h") }
                    FilledTonalButton(onClick = { onShift(HOUR) }) { Text("+1 h") }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilledTonalButton(onClick = { onShift(-DAY) }) { Text("−1 day") }
                    FilledTonalButton(onClick = { onShift(DAY) }) { Text("+1 day") }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilledTonalButton(onClick = { onShift(-30 * DAY) }) { Text("−1 month") }
                    FilledTonalButton(onClick = { onShift(30 * DAY) }) { Text("+1 month") }
                }
            }
        },
    )
}
