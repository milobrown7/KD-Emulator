package com.kdemulator.ui.settings

import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.AppServices
import com.kdemulator.skins.ControllerSkin
import com.kdemulator.skins.ControllerSkinView
import com.kdemulator.skins.SkinActions
import com.kdemulator.skins.SkinInputState
import com.kdemulator.skins.SkinLayout
import com.kdemulator.ui.theme.Island

@Composable
private fun Header(title: String, onBack: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(4.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
        Text(title, style = MaterialTheme.typography.titleLarge)
    }
}

@Composable
private fun Section(title: String) {
    Text(
        title.uppercase(),
        Modifier.padding(start = 20.dp, top = 24.dp, bottom = 6.dp),
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
    )
}

@Composable
private fun SwitchRow(title: String, detail: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable { onChange(!checked) }.padding(horizontal = 20.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            detail?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.6f)) }
        }
        Switch(checked, onChange)
    }
}

@Composable
private fun SliderRow(title: String, value: Float, range: ClosedFloatingPointRange<Float>, label: String, onChange: (Float) -> Unit) {
    Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
        Row {
            Text(title, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            Text(label, style = MaterialTheme.typography.bodyMedium)
        }
        Slider(value, onChange, valueRange = range)
    }
}

@Composable
fun SettingsScreen(services: AppServices, deviceSummary: String, onSkins: (Console) -> Unit, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val s by services.settings.state.collectAsState()
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Header("Settings", onBack)
            LazyColumn(contentPadding = PaddingValues(bottom = 32.dp)) {
                item { Section("Controller Skins") }
                items(Console.entries) { c ->
                    val skin = remember(s.skins[c]) { services.skins.find(c, s.skins[c]) }
                    Row(
                        Modifier.fillMaxWidth().clickable { onSkins(c) }.padding(horizontal = 20.dp, vertical = 14.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(c.label, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                        Text(skin.name, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.6f))
                        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null)
                    }
                }
                item {
                    SliderRow("Controller opacity", s.skinOpacity, 0.1f..1f, "${(s.skinOpacity * 100).toInt()}%") { v ->
                        services.settings.update { it.copy(skinOpacity = v) }
                    }
                    SwitchRow("Button haptics", "Vibrate when a button is pressed", s.buttonHaptics) { v -> services.settings.update { it.copy(buttonHaptics = v) } }
                    SwitchRow("Smooth scaling", "Off keeps pixels crisp", s.smoothScaling) { v -> services.settings.update { it.copy(smoothScaling = v) } }
                }
                item {
                    Section("Gameplay")
                    SwitchRow("Resume where you left off", "Leaving a game makes an auto save; opening it again loads it", s.autoResume) { v ->
                        services.settings.update { it.copy(autoResume = v) }
                    }
                    Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
                        Text("Fast forward speed", style = MaterialTheme.typography.titleMedium)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 6.dp)) {
                            for (speed in listOf(2, 3, 4, 8)) {
                                FilterChip(s.fastForwardSpeed == speed, { services.settings.update { it.copy(fastForwardSpeed = speed) } }, { Text("$speed×") })
                            }
                        }
                    }
                    SwitchRow("Rewind", "Keeps recent moments so you can step back", s.rewindEnabled) { v -> services.settings.update { it.copy(rewindEnabled = v) } }
                    if (s.rewindEnabled) {
                        SliderRow("Rewind snapshot every", s.rewindIntervalSec, 0.25f..2f, "%.2f s".format(s.rewindIntervalSec)) { v ->
                            services.settings.update { it.copy(rewindIntervalSec = v) }
                        }
                    }
                }
                item {
                    Section("Audio & Motion")
                    SliderRow("Game volume", s.volume, 0f..1f, "${(s.volume * 100).toInt()}%") { v -> services.settings.update { it.copy(volume = v) } }
                    SwitchRow("DS microphone", "Use the phone's microphone for blowing and voice", s.microphone) { v -> services.settings.update { it.copy(microphone = v) } }
                    SwitchRow("Aim the Wii pointer by tilting", "Uses the gyroscope instead of touch", s.gyroPointer) { v -> services.settings.update { it.copy(gyroPointer = v) } }
                }
                item {
                    Section("About")
                    Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text("KD-Emulator 0.2", style = MaterialTheme.typography.titleMedium)
                        Text(deviceSummary, style = MaterialTheme.typography.bodyMedium)
                        Text(
                            "Game Boy Advance, Nintendo DS and GameCube games run without BIOS or firmware files. Animal Crossing games get their own profile on every console (🍃).",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Text(
                            "KD-Emulator isn't affiliated with Nintendo. Controller skins in Delta's .deltaskin format can be imported for GBA and DS.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.6f),
                        )
                    }
                }
            }
        }
    }
}

private val noActions = object : SkinActions {
    override fun menu() {}
    override fun quickSave() {}
    override fun quickLoad() {}
    override fun setFastForward(on: Boolean) {}
    override fun toggleFastForward() {}
    override fun setRewinding(on: Boolean) {}
}

/** Pick or import a controller skin for one console. */
@Composable
fun SkinPickerScreen(services: AppServices, console: Console, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val s by services.settings.state.collectAsState()
    var version by remember { mutableIntStateOf(0) }
    val skins = remember(version) { services.skins.skins(console) }
    var message by remember { mutableStateOf<String?>(null) }
    val importer = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            services.skins.import(uri).fold(
                onSuccess = { skin ->
                    message = if (skin.console == console) "Imported ${skin.name}" else "Imported ${skin.name} for ${skin.console.label}"
                    if (skin.console == console) services.settings.update { it.copy(skins = it.skins + (console to skin.id)) }
                },
                onFailure = { message = it.message ?: "Couldn't import that skin" },
            )
            version++
        }
    }
    val selected = s.skins[console] ?: skins.first().id
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Header("${console.label} Skins", onBack)
            message?.let { Text(it, Modifier.padding(horizontal = 20.dp), color = Island.Leaf) }
            LazyRow(contentPadding = PaddingValues(20.dp), horizontalArrangement = Arrangement.spacedBy(16.dp), modifier = Modifier.weight(1f)) {
                items(skins, key = { it.id }) { skin ->
                    SkinPreview(skin, console, skin.id == selected, s.skinOpacity) {
                        services.settings.update { it.copy(skins = if (skin.builtIn) it.skins - console else it.skins + (console to skin.id)) }
                    }
                    if (!skin.builtIn) {
                        IconButton(onClick = {
                            services.skins.delete(skin)
                            if (selected == skin.id) services.settings.update { it.copy(skins = it.skins - console) }
                            version++
                        }) { Icon(Icons.Filled.Delete, "Delete skin") }
                    }
                }
            }
            OutlinedButton(onClick = { importer.launch(arrayOf("*/*")) }, modifier = Modifier.padding(20.dp).fillMaxWidth()) {
                Text(if (console == Console.GBA || console == Console.NDS) "Import a .deltaskin" else "Import a .kdskin")
            }
        }
    }
}

@Composable
private fun SkinPreview(skin: ControllerSkin, console: Console, selected: Boolean, opacity: Float, onSelect: () -> Unit) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        BoxWithConstraints(
            Modifier.height(420.dp).width(194.dp).clip(RoundedCornerShape(24.dp)).background(Color.Black)
                .border(if (selected) 3.dp else 1.dp, if (selected) Island.Leaf else Color.White.copy(alpha = 0.2f), RoundedCornerShape(24.dp)),
        ) {
            val density = LocalDensity.current
            val rep = remember(skin) { skin.representation(false, 194f, 420f, rewind = true) }
            if (rep != null) {
                val layout = remember(rep) { SkinLayout(rep, with(density) { 194.dp.toPx() }, with(density) { 420.dp.toPx() }, console.screenWidth, console.screenHeight) }
                // Placeholder screens so the preview shows where the game goes.
                for ((_, dst) in layout.screenRects()) {
                    Box(
                        Modifier.padding(
                            start = with(density) { dst.left.toDp() },
                            top = with(density) { dst.top.toDp() },
                        ).width(with(density) { dst.width().toDp() }).height(with(density) { dst.height().toDp() })
                            .background(Island.NightSurface),
                    )
                }
                val input = remember { SkinInputState(noActions) {} }
                ControllerSkinView(layout, console, opacity, input)
            }
            // Taps select the skin instead of pressing its buttons.
            Box(Modifier.fillMaxSize().clickable(onClick = onSelect))
        }
        Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            if (selected) Icon(Icons.Filled.CheckCircle, null, tint = Island.Leaf)
            Text(" ${skin.name}", style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
        }
        HorizontalDivider(Modifier.width(40.dp).padding(top = 4.dp), color = Color.Transparent)
    }
}
