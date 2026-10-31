package com.kdemulator.ui.library

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Spa
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.AppServices
import com.kdemulator.data.Game
import com.kdemulator.data.ImportResult
import com.kdemulator.data.SaveStateKind
import com.kdemulator.ui.theme.Island
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

fun consoleAccent(console: Console): Color = when (console) {
    Console.GBA -> Island.Lagoon
    Console.NDS -> Island.Sky
    Console.GAMECUBE -> Island.Leaf
    Console.WII -> Island.Sunset
}

/** What the Animal Crossing profile turns on, per console (mirrors kd-core). */
fun animalCrossingBlurb(console: Console): String = when (console) {
    Console.GBA -> "Link-cable companion for the GameCube game"
    Console.NDS -> "Stylus-first controls and a time-travel clock"
    Console.GAMECUBE -> "Memory Card 251, GBA link and a time-travel clock"
    Console.WII -> "Pointer controls, SD card and a time-travel clock"
}

/** Ways to start a game from the library. */
enum class LaunchMode { NORMAL, RESUME, FRESH }

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun LibraryScreen(
    services: AppServices,
    deviceSummary: String,
    message: String?,
    onMessageShown: () -> Unit,
    onPlay: (Game, LaunchMode) -> Unit,
    onSettings: () -> Unit,
    onCheats: (Game) -> Unit,
) {
    val games by services.library.games.collectAsState()
    val artVersion by services.library.artworkVersion.collectAsState()
    val consoles = Console.entries.filter { c -> games.any { it.console == c } }
    val pager = rememberPagerState { consoles.size.coerceAtLeast(1) }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    var importing by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf<Game?>(null) }
    var deleting by remember { mutableStateOf<Game?>(null) }
    var artworkFor by remember { mutableStateOf<Game?>(null) }
    var saveFileFor by remember { mutableStateOf<Game?>(null) }

    LaunchedEffect(message) {
        if (message != null) {
            snackbar.showSnackbar(message)
            onMessageShown()
        }
    }

    val importer = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris: List<Uri> ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        scope.launch {
            importing = true
            val results = withContext(Dispatchers.IO) { services.library.import(uris) }
            importing = false
            val added = results.filterIsInstance<ImportResult.Added>()
            val failed = results.filterIsInstance<ImportResult.Failed>()
            val dupes = results.filterIsInstance<ImportResult.Duplicate>()
            added.firstOrNull()?.let { first ->
                val page = Console.entries.filter { c -> services.library.games.value.any { it.console == c } }.indexOf(first.game.console)
                if (page >= 0) pager.animateScrollToPage(page)
            }
            snackbar.showSnackbar(
                buildList {
                    if (added.isNotEmpty()) add("Imported ${added.size} game${if (added.size == 1) "" else "s"}")
                    if (dupes.isNotEmpty()) add("${dupes.size} already in your library")
                    failed.firstOrNull()?.let { add("${it.name}: ${it.reason}") }
                }.joinToString(" · ").ifEmpty { "Nothing imported" },
            )
        }
    }
    val artworkPicker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        val g = artworkFor
        artworkFor = null
        if (uri != null && g != null) scope.launch(Dispatchers.IO) { services.library.setArtwork(g.id, uri) }
    }
    val saveImporter = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        val g = saveFileFor
        saveFileFor = null
        if (uri != null && g != null) scope.launch {
            val ok = withContext(Dispatchers.IO) { services.library.importBackup(g, uri) }
            snackbar.showSnackbar(if (ok) "Save file imported" else "Couldn't read that save file")
        }
    }
    val saveExporter = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val g = saveFileFor
        saveFileFor = null
        if (uri != null && g != null) scope.launch {
            val ok = withContext(Dispatchers.IO) { services.library.exportBackup(g, uri) }
            snackbar.showSnackbar(if (ok) "Save file exported" else "This game has no save file yet")
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            Row(Modifier.fillMaxWidth().safeDrawingPadding().padding(horizontal = 4.dp, vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onSettings) { Icon(Icons.Filled.Settings, "Settings") }
                Text(
                    consoles.getOrNull(pager.currentPage)?.label ?: "KD-Emulator",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleLarge,
                    textAlign = TextAlign.Center,
                )
                IconButton(onClick = { importer.launch(arrayOf("*/*")) }) { Icon(Icons.Filled.Add, "Import games") }
            }
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            if (consoles.isEmpty()) {
                EmptyLibrary(deviceSummary) { importer.launch(arrayOf("*/*")) }
            } else {
                Column(Modifier.fillMaxSize()) {
                    HorizontalPager(pager, Modifier.weight(1f)) { page ->
                        val console = consoles[page]
                        val list = games.filter { it.console == console }
                            .sortedWith(compareByDescending<Game> { it.animalCrossing }.thenBy { it.title.lowercase() })
                        LazyVerticalGrid(
                            columns = GridCells.Adaptive(if (console == Console.GAMECUBE || console == Console.WII) 150.dp else 104.dp),
                            contentPadding = PaddingValues(16.dp),
                            horizontalArrangement = Arrangement.spacedBy(14.dp),
                            verticalArrangement = Arrangement.spacedBy(18.dp),
                        ) {
                            items(list, key = { it.id }) { game ->
                                var menu by remember { mutableStateOf(false) }
                                val hasAuto = remember(game.id, menu) { services.states.latest(game.id, SaveStateKind.AUTO) != null }
                                Box {
                                    GameTile(services, game, artVersion, Modifier.combinedClickable(onClick = { onPlay(game, LaunchMode.NORMAL) }, onLongClick = { menu = true }))
                                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                        Text(game.title, Modifier.padding(horizontal = 16.dp, vertical = 8.dp), style = MaterialTheme.typography.titleMedium)
                                        if (game.animalCrossing) {
                                            Text("🍃 " + animalCrossingBlurb(game.console), Modifier.padding(horizontal = 16.dp), style = MaterialTheme.typography.bodyMedium, color = Island.Leaf)
                                        }
                                        HorizontalDivider(Modifier.padding(vertical = 4.dp))
                                        if (hasAuto) DropdownMenuItem(text = { Text("Resume") }, onClick = { menu = false; onPlay(game, LaunchMode.RESUME) })
                                        DropdownMenuItem(text = { Text(if (hasAuto) "Start Over" else "Play") }, onClick = { menu = false; onPlay(game, LaunchMode.FRESH) })
                                        DropdownMenuItem(text = { Text("Cheat Codes") }, onClick = { menu = false; onCheats(game) })
                                        DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; renaming = game })
                                        DropdownMenuItem(text = { Text("Change Artwork") }, onClick = {
                                            menu = false
                                            artworkFor = game
                                            artworkPicker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                                        })
                                        DropdownMenuItem(text = { Text("Import Save File") }, onClick = {
                                            menu = false
                                            saveFileFor = game
                                            saveImporter.launch(arrayOf("*/*"))
                                        })
                                        DropdownMenuItem(text = { Text("Export Save File") }, onClick = {
                                            menu = false
                                            saveFileFor = game
                                            saveExporter.launch("${game.title}.sav")
                                        })
                                        DropdownMenuItem(text = { Text("Delete", color = MaterialTheme.colorScheme.error) }, onClick = { menu = false; deleting = game })
                                    }
                                }
                            }
                        }
                    }
                    if (consoles.size > 1) {
                        Row(Modifier.fillMaxWidth().padding(bottom = 12.dp), horizontalArrangement = Arrangement.Center) {
                            consoles.forEachIndexed { i, c ->
                                Box(
                                    Modifier.padding(4.dp).size(if (i == pager.currentPage) 9.dp else 7.dp).clip(CircleShape)
                                        .background(if (i == pager.currentPage) consoleAccent(c) else MaterialTheme.colorScheme.onBackground.copy(alpha = 0.25f)),
                                )
                            }
                        }
                    }
                }
            }
            if (importing) {
                Box(Modifier.fillMaxSize().background(Color(0x66000000)), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        CircularProgressIndicator()
                        Text("Importing…", Modifier.padding(top = 12.dp), color = Color.White)
                    }
                }
            }
        }
    }

    renaming?.let { g ->
        var name by remember { mutableStateOf(g.title) }
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename") },
            text = { OutlinedTextField(name, { name = it }, singleLine = true) },
            confirmButton = { TextButton(onClick = { services.library.rename(g.id, name); renaming = null }) { Text("Save") } },
            dismissButton = { TextButton(onClick = { renaming = null }) { Text("Cancel") } },
        )
    }
    deleting?.let { g ->
        AlertDialog(
            onDismissRequest = { deleting = null },
            title = { Text("Delete ${g.title}?") },
            text = { Text("The game, its save file, save states and cheats are removed from KD-Emulator.") },
            confirmButton = {
                TextButton(onClick = { services.library.delete(g.id); deleting = null }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { deleting = null }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun EmptyLibrary(deviceSummary: String, onImport: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(Icons.Filled.Spa, null, tint = Island.Leaf, modifier = Modifier.size(72.dp))
        Spacer(Modifier.height(16.dp))
        Text("No Games", style = MaterialTheme.typography.headlineMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "Import your own game files: .gba, .nds, .iso/.gcm, .wbfs, .dol or a .zip. No BIOS or firmware is needed.",
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.75f),
        )
        Spacer(Modifier.height(20.dp))
        Button(onClick = onImport) { Text("Import Games") }
        Spacer(Modifier.height(24.dp))
        Text(deviceSummary, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.5f), textAlign = TextAlign.Center)
    }
}

/** Artwork + title, like a cartridge on a shelf. */
@Composable
fun GameTile(services: AppServices, game: Game, artVersion: Int, modifier: Modifier = Modifier) {
    val art = remember(game.id, artVersion) { services.library.artwork(game.id)?.asImageBitmap() }
    val wide = game.console == Console.GAMECUBE || game.console == Console.WII
    Column(modifier) {
        Box(
            Modifier.fillMaxWidth().aspectRatio(if (wide) 1.5f else 1f).clip(RoundedCornerShape(16.dp))
                .background(Brush.linearGradient(listOf(consoleAccent(game.console), consoleAccent(game.console).copy(alpha = 0.55f), Island.Night))),
            contentAlignment = Alignment.Center,
        ) {
            if (art != null) {
                // Small native icons (DS 32x32, GC 96x32) stay pixel-crisp; photos fill the tile.
                val tiny = art.width <= 96
                Image(
                    art,
                    game.title,
                    if (tiny) Modifier.fillMaxSize().padding(if (art.width == art.height) 18.dp else 10.dp) else Modifier.fillMaxSize(),
                    contentScale = if (tiny) ContentScale.Fit else ContentScale.Crop,
                    filterQuality = if (tiny) FilterQuality.None else FilterQuality.Medium,
                )
            } else {
                Column(Modifier.padding(10.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(game.console.shortLabel, color = Color.White.copy(alpha = 0.75f), fontSize = 11.sp, fontWeight = FontWeight.Bold)
                    Text(
                        game.title,
                        color = Color.White,
                        fontWeight = FontWeight.Bold,
                        fontSize = 15.sp,
                        textAlign = TextAlign.Center,
                        maxLines = 3,
                        overflow = TextOverflow.Ellipsis,
                        lineHeight = 18.sp,
                    )
                }
            }
            if (game.animalCrossing) {
                Box(Modifier.align(Alignment.TopEnd).padding(6.dp).size(24.dp).clip(CircleShape).background(Island.Cream), contentAlignment = Alignment.Center) {
                    Text("🍃", fontSize = 13.sp)
                }
            }
        }
        Text(
            game.title,
            Modifier.padding(top = 6.dp),
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            color = MaterialTheme.colorScheme.onBackground,
        )
    }
}
