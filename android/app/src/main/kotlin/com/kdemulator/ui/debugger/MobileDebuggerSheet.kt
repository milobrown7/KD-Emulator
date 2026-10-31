package com.kdemulator.ui.debugger

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.performance.TensorThermalHintManager
import kotlinx.coroutines.delay

private val labels = linkedMapOf(
    "system" to "Console",
    "gpu" to "Active GPU profile",
    "host_page_size" to "Host page size (bytes)",
    "soc" to "SoC / cluster layout",
    "clusters" to "Cluster max clocks",
    "armv9" to "ARMv9.2 fast paths",
    "thermal" to "Thermal status",
    "ubershaders" to "Async uber-shaders",
    "dsp" to "DSP mode",
    "max_scale" to "Max internal scale",
    "frame_ms" to "Emulated frame time (ms)",
    "frames" to "Frames",
)

/** Live native diagnostics: GPU profile, page size, Tensor clusters, thermal knobs. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MobileDebuggerSheet(thermalStatus: Int, onDismiss: () -> Unit) {
    var info by remember { mutableStateOf(emptyMap<String, String>()) }
    LaunchedEffect(Unit) {
        while (true) {
            info = NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGetDebugInfo())
            delay(500)
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 8.dp)) {
            Text("Mobile debugger", style = MaterialTheme.typography.titleLarge)
            Text("Android thermal: ${TensorThermalHintManager.label(thermalStatus)}", style = MaterialTheme.typography.bodyMedium)
            Spacer(Modifier.height(12.dp))
            for ((key, label) in labels) {
                val value = info[key] ?: continue
                Row(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
                    Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                    Text(value, style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace))
                }
                HorizontalDivider()
            }
            Spacer(Modifier.height(32.dp))
        }
    }
}
