package com.rrmtools.fetchrail

import android.Manifest
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.util.Locale

private val Ember = Color(0xFFFF8A3D)
private val Background = Color(0xFF101319)
private val Panel = Color(0xFF1B2028)
private val Muted = Color(0xFF9AA5B4)

class MainActivity : ComponentActivity() {
    private data class IncomingLink(val url: String, val sequence: Int)
    private val incomingLink = mutableStateOf(IncomingLink("", 0))

    private fun receiveLink(intent: Intent?) {
        IncomingLinks.from(intent)?.let { url ->
            incomingLink.value = IncomingLink(url, incomingLink.value.sequence + 1)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        receiveLink(intent)
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 101)
        }
        setContent {
            MaterialTheme(
                colorScheme = darkColorScheme(
                    primary = Ember, onPrimary = Color(0xFF241307),
                    background = Background, surface = Panel,
                    onSurface = Color(0xFFF0F3F7)
                )
            ) {
                FetchrailScreen(
                    root = downloadRoot, incoming = incomingLink.value.url,
                    incomingSequence = incomingLink.value.sequence,
                    start = { input ->
                        val id = NativeBridge.result(NativeBridge.enqueue(downloadRoot, input)).getString("value")
                        try {
                            JobControl.enqueue(this, id)
                        } catch (error: Exception) {
                            NativeBridge.pause(downloadRoot, id)
                            throw error
                        }
                    },
                    pause = { item ->
                        NativeBridge.result(NativeBridge.pause(downloadRoot, item.id))
                        JobControl.cancel(this, item.id)
                    },
                    resume = { item ->
                        NativeBridge.result(NativeBridge.resume(downloadRoot, item.id))
                        JobControl.enqueue(this, item.id)
                    },
                    remove = { item ->
                        NativeBridge.result(NativeBridge.remove(downloadRoot, item.id))
                    },
                    export = { item -> exportToDownloads(item) }
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        receiveLink(intent)
    }

    /** Export on demand, avoiding an automatic second copy of multi-gigabyte files. */
    private fun exportToDownloads(item: TransferRecord) {
        val source = File(item.savedPath ?: error("File is not finished"))
        val privateRoot = File(downloadRoot, "fetchrail").canonicalPath + File.separator
        require(source.isFile && source.canonicalPath.startsWith(privateRoot)) {
            "Saved file is outside Fetchrail storage"
        }
        val mime = MimeTypeMap.getSingleton()
            .getMimeTypeFromExtension(source.extension.lowercase(Locale.ROOT))
            ?: "application/octet-stream"
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, item.fileName)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/Fetchrail")
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val uri = contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: error("Android could not create the Downloads entry")
        try {
            source.inputStream().buffered(256 * 1024).use { input ->
                val output = contentResolver.openOutputStream(uri) ?: error("Cannot open destination")
                output.buffered(256 * 1024).use { input.copyTo(it, 256 * 1024) }
            }
            contentResolver.update(uri, ContentValues().apply {
                put(MediaStore.Downloads.IS_PENDING, 0)
            }, null, null)
        } catch (failure: Exception) {
            contentResolver.delete(uri, null, null)
            throw failure
        }
    }
}

@Composable
private fun FetchrailScreen(
    root: String,
    incoming: String,
    incomingSequence: Int,
    start: (String) -> Unit,
    pause: (TransferRecord) -> Unit,
    resume: (TransferRecord) -> Unit,
    remove: (TransferRecord) -> Unit,
    export: (TransferRecord) -> Unit
) {
    var address by remember { mutableStateOf(incoming) }
    var transfers by remember { mutableStateOf<List<TransferRecord>>(emptyList()) }
    var filter by remember { mutableStateOf("All") }
    var errorMessage by remember { mutableStateOf<String?>(null) }
    var successMessage by remember { mutableStateOf<String?>(null) }
    var removing by remember { mutableStateOf<TransferRecord?>(null) }
    var exporting by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    LaunchedEffect(incomingSequence) { if (incoming.isNotBlank()) address = incoming }
    LaunchedEffect(root) {
        while (isActive) {
            try {
                transfers = withContext(Dispatchers.IO) { NativeBridge.downloads(root) }
            } catch (failure: Exception) {
                errorMessage = failure.message ?: "Cannot read downloads"
                break
            }
            delay(950)
        }
    }
    fun perform(action: () -> Unit) {
        try {
            action()
            transfers = NativeBridge.downloads(root)
        } catch (failure: Exception) {
            errorMessage = failure.message ?: "Action failed"
        }
    }
    val active = transfers.count { it.status in setOf("queued", "connecting", "downloading") }
    val completed = transfers.count { it.status == "completed" }
    val visible = transfers.filter {
        when (filter) {
            "Active" -> it.status in setOf("queued", "connecting", "downloading")
            "Finished" -> it.status == "completed"
            else -> true
        }
    }

    Column(
        Modifier.fillMaxSize().background(Background)
            .statusBarsPadding().navigationBarsPadding()
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(top = 24.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(44.dp).background(Ember, RoundedCornerShape(13.dp)),
                    contentAlignment = Alignment.Center) {
                    Text("↓", color = Color(0xFF221307),
                        style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Black)
                }
                Spacer(Modifier.width(12.dp))
                Column {
                    Text("Fetchrail", style = MaterialTheme.typography.headlineMedium,
                        fontWeight = FontWeight.Bold)
                    Text("Downloads on your terms", color = Muted,
                        style = MaterialTheme.typography.bodySmall)
                }
            }
            Spacer(Modifier.height(26.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Statistic("In progress", active.toString(), Modifier.weight(1f))
                Statistic("Completed", completed.toString(), Modifier.weight(1f))
            }
            Spacer(Modifier.height(20.dp))
            Text("NEW DOWNLOAD", color = Muted, style = MaterialTheme.typography.labelSmall)
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = address, onValueChange = { address = it },
                modifier = Modifier.fillMaxWidth(), label = { Text("File URL") },
                placeholder = { Text("https://example.com/file.zip") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                shape = RoundedCornerShape(16.dp)
            )
            Spacer(Modifier.height(10.dp))
            Button(
                onClick = { perform { start(address.trim()); address = "" } },
                enabled = address.trim().startsWith("http", ignoreCase = true),
                modifier = Modifier.fillMaxWidth().height(48.dp),
                shape = RoundedCornerShape(14.dp)
            ) { Text("Start download", fontWeight = FontWeight.SemiBold) }
            Spacer(Modifier.height(22.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Your transfers", style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
                Text(transfers.size.toString() + " total", color = Muted,
                    style = MaterialTheme.typography.bodySmall)
            }
            Spacer(Modifier.height(10.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf("All", "Active", "Finished").forEach { option ->
                    FilterChip(selected = filter == option, onClick = { filter = option },
                        label = { Text(option) })
                }
            }
        }
        if (visible.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text("↓", color = Ember, style = MaterialTheme.typography.displayLarge)
                    Spacer(Modifier.height(12.dp))
                    Text(if (filter == "All") "No downloads yet" else "Nothing here",
                        fontWeight = FontWeight.Bold)
                    Spacer(Modifier.height(6.dp))
                    Text("Paste a URL or share a link from another app.", color = Muted)
                }
            }
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(start = 20.dp, end = 20.dp, top = 10.dp, bottom = 28.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp)
            ) {
                items(visible, key = { it.id }) { entry ->
                    TransferCard(
                        item = entry, exporting = exporting == entry.id,
                        onPause = { perform { pause(entry) } },
                        onResume = { perform { resume(entry) } },
                        onRemove = { removing = entry },
                        onExport = {
                            exporting = entry.id
                            scope.launch {
                                try {
                                    withContext(Dispatchers.IO) { export(entry) }
                                    successMessage = "Saved " + entry.fileName + " to Downloads/Fetchrail"
                                } catch (failure: Exception) {
                                    errorMessage = failure.message ?: "Export failed"
                                } finally { exporting = null }
                            }
                        }
                    )
                }
            }
        }
    }
    removing?.let { entry ->
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text("Remove download?") },
            text = { Text("This removes the local file and its saved progress. Export it first to keep a copy.") },
            confirmButton = { TextButton(onClick = {
                removing = null
                perform { remove(entry) }
            }) { Text("Remove") } },
            dismissButton = { TextButton(onClick = { removing = null }) { Text("Cancel") } }
        )
    }
    errorMessage?.let { message ->
        AlertDialog(onDismissRequest = { errorMessage = null }, title = { Text("Download error") },
            text = { Text(message) },
            confirmButton = { TextButton(onClick = { errorMessage = null }) { Text("OK") } })
    }
    successMessage?.let { message ->
        AlertDialog(onDismissRequest = { successMessage = null }, title = { Text("Saved") },
            text = { Text(message) },
            confirmButton = { TextButton(onClick = { successMessage = null }) { Text("Done") } })
    }
}

@Composable
private fun Statistic(title: String, count: String, modifier: Modifier = Modifier) {
    Surface(modifier, shape = RoundedCornerShape(17.dp), color = Panel,
        border = BorderStroke(1.dp, Color(0xFF303642))) {
        Column(Modifier.padding(18.dp)) {
            Text(title, color = Muted, style = MaterialTheme.typography.labelMedium)
            Spacer(Modifier.height(6.dp))
            Text(count, color = Color.White, style = MaterialTheme.typography.headlineMedium,
                fontWeight = FontWeight.Bold)
        }
    }
}

@Composable
private fun TransferCard(
    item: TransferRecord, exporting: Boolean,
    onPause: () -> Unit, onResume: () -> Unit,
    onRemove: () -> Unit, onExport: () -> Unit
) {
    val busy = item.status in setOf("queued", "connecting", "downloading")
    val done = item.status == "completed"
    val fraction = item.totalBytes?.takeIf { it > 0 }?.let {
        (item.downloadedBytes.toDouble() / it).coerceIn(0.0, 1.0).toFloat()
    }
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(18.dp),
        color = Panel, border = BorderStroke(1.dp, Color(0xFF303642))) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.Top) {
                Box(Modifier.size(40.dp).background(Color(0xFF30323B), RoundedCornerShape(12.dp)),
                    contentAlignment = Alignment.Center) {
                    Text(if (done) "✓" else "↓",
                        color = if (done) Color(0xFF6DDFB3) else Ember, fontWeight = FontWeight.Bold)
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(item.fileName, maxLines = 2, overflow = TextOverflow.Ellipsis,
                        fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(3.dp))
                    Text(item.url.substringAfter("://").substringBefore("/"),
                        color = Muted, style = MaterialTheme.typography.bodySmall)
                }
                Text(item.status.replaceFirstChar { it.uppercaseChar() },
                    color = if (done) Color(0xFF6DDFB3) else if (busy) Ember else Muted,
                    style = MaterialTheme.typography.labelSmall)
            }
            Spacer(Modifier.height(14.dp))
            if (fraction != null) {
                LinearProgressIndicator(progress = { fraction }, modifier = Modifier.fillMaxWidth().height(5.dp),
                    color = Ember, trackColor = Color(0xFF343C47))
            } else if (busy) {
                LinearProgressIndicator(modifier = Modifier.fillMaxWidth().height(5.dp),
                    color = Ember, trackColor = Color(0xFF343C47))
            }
            Spacer(Modifier.height(8.dp))
            Row {
                Text(readableBytes(item.downloadedBytes) +
                    (item.totalBytes?.let { " / " + readableBytes(it) } ?: ""),
                    style = MaterialTheme.typography.bodySmall, color = Muted,
                    modifier = Modifier.weight(1f))
                if (busy) Text(readableBytes(item.speedBps) + "/s · " + item.connections + " rails",
                    style = MaterialTheme.typography.bodySmall, color = Muted)
            }
            item.error?.let {
                Spacer(Modifier.height(6.dp))
                Text(it, color = Color(0xFFFF9D94), style = MaterialTheme.typography.bodySmall)
            }
            Spacer(Modifier.height(12.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                when {
                    done -> Button(onClick = onExport, enabled = !exporting,
                        shape = RoundedCornerShape(12.dp)) {
                        Text(if (exporting) "Saving…" else "Save to Downloads")
                    }
                    busy -> OutlinedButton(onClick = onPause, shape = RoundedCornerShape(12.dp)) {
                        Text("Pause")
                    }
                    else -> Button(onClick = onResume, shape = RoundedCornerShape(12.dp)) {
                        Text(if (item.status == "failed") "Retry" else "Resume")
                    }
                }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = onRemove, enabled = !busy) { Text("Remove") }
            }
        }
    }
}

private fun readableBytes(count: Long): String {
    if (count < 1024) return "$count B"
    var value = count.toDouble()
    val units = arrayOf("B", "KB", "MB", "GB", "TB")
    var index = 0
    while (value >= 1024 && index < units.lastIndex) {
        value /= 1024.0
        index++
    }
    return String.format(Locale.ROOT, "%.1f %s", value, units[index])
}
