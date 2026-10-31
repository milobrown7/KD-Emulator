package com.kdemulator.ui.library

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.RomEntry
import com.kdemulator.data.RomRepository
import com.kdemulator.ui.theme.Island
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** What the Animal Crossing profile turns on, per console (mirrors kd-core). */
fun animalCrossingBlurb(console: Console): String = when (console) {
    Console.GBA -> "Link-cable companion: island trips, NES games and pattern editing over JoyBus"
    Console.NDS -> "Stylus-first controls and a time-travel clock for every season"
    Console.GAMECUBE -> "Memory Card 251, GBA link port and a time-travel clock"
    Console.WII -> "Pointer controls, SD card and a time-travel clock"
}

fun consoleAccent(console: Console): Color = when (console) {
    Console.GBA -> Island.Lagoon
    Console.NDS -> Island.Sky
    Console.GAMECUBE -> Island.Leaf
    Console.WII -> Island.Sunset
}

@Composable
fun RomLibraryScreen(repo: RomRepository, deviceSummary: String, onPlay: (RomEntry) -> Unit) {
    var roms by remember { mutableStateOf<List<RomEntry>>(emptyList()) }
    var scanning by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    fun rescan() {
        scope.launch {
            scanning = true
            roms = withContext(Dispatchers.IO) { repo.scan() }
            scanning = false
        }
    }

    val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri: Uri? ->
        if (uri != null) {
            repo.persistFolder(uri)
            rescan()
        }
    }
    val pickFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri: Uri? ->
        if (uri != null) {
            scope.launch {
                val name = uri.lastPathSegment?.substringAfterLast('/') ?: "game"
                val entry = withContext(Dispatchers.IO) { repo.peek(uri, name) }
                if (entry != null) onPlay(entry)
            }
        }
    }

    LaunchedEffect(Unit) { if (repo.romFolder != null) rescan() }

    Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).padding(20.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(14.dp).clip(CircleShape).background(Island.Leaf))
            Spacer(Modifier.width(10.dp))
            Text("KD-Emulator", style = MaterialTheme.typography.headlineMedium, color = MaterialTheme.colorScheme.onBackground)
        }
        Text(deviceSummary, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.7f))
        Spacer(Modifier.height(16.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Button(onClick = { pickFolder.launch(null) }) { Text("Choose game folder") }
            OutlinedButton(onClick = { pickFile.launch(arrayOf("*/*")) }) { Text("Open a file") }
        }
        Spacer(Modifier.height(16.dp))
        when {
            scanning -> Box(Modifier.fillMaxWidth().padding(32.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            roms.isEmpty() -> Text(
                "Pick the folder with your own game dumps (.gba, .nds, .iso/.gcm, .dol, .wbfs). " +
                    "No BIOS or firmware files are needed.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onBackground,
            )
            else -> LazyColumn(verticalArrangement = Arrangement.spacedBy(12.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
                items(roms, key = { it.uri.toString() }) { rom -> RomCard(rom) { onPlay(rom) } }
            }
        }
    }
}

@Composable
private fun RomCard(rom: RomEntry, onClick: () -> Unit) {
    val accent = consoleAccent(rom.console)
    Card(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
        colors = CardDefaults.cardColors(
            containerColor = if (rom.animalCrossing) accent.copy(alpha = 0.18f) else MaterialTheme.colorScheme.surface,
        ),
        shape = MaterialTheme.shapes.medium,
    ) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(44.dp).clip(CircleShape).background(accent),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    when (rom.console) {
                        Console.GBA -> "GBA"
                        Console.NDS -> "DS"
                        Console.GAMECUBE -> "GC"
                        Console.WII -> "Wii"
                    },
                    color = Color.White,
                    fontWeight = FontWeight.Bold,
                )
            }
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Text(rom.title, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurface)
                Text("${rom.console.label} · ${rom.gameCode}", style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f))
                if (rom.animalCrossing) {
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "🍃 Animal Crossing profile — ${animalCrossingBlurb(rom.console)}",
                        style = MaterialTheme.typography.bodyMedium,
                        color = Island.DeepLeaf,
                    )
                }
            }
        }
    }
}
