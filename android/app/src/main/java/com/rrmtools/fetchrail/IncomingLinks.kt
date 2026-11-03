package com.rrmtools.fetchrail

import android.content.Intent
import android.net.Uri

/**
 * Only a deliberate HTTP/HTTPS link is accepted, with its query string and
 * percent-encoding preserved (important for signed download URLs).
 */
internal object IncomingLinks {
    fun fromView(intent: Intent?): Uri? {
        if (intent?.action != Intent.ACTION_VIEW) return null
        val uri = intent.data ?: return null
        val scheme = uri.scheme?.lowercase()
        return uri.takeIf { scheme in listOf("http", "https") && !it.host.isNullOrBlank() }
    }

    fun from(intent: Intent?): String? {
        fromView(intent)?.let { return it.toString() }
        if (intent?.action != Intent.ACTION_SEND) return null
        val text = intent.getStringExtra(Intent.EXTRA_TEXT).orEmpty()
        val candidate = Regex("https?://[^\\s<>]+", RegexOption.IGNORE_CASE)
            .find(text)?.value?.trimEnd('.', ',') ?: return null
        val uri = Uri.parse(candidate)
        val scheme = uri.scheme?.lowercase()
        return candidate.takeIf { scheme in listOf("http", "https") && !uri.host.isNullOrBlank() }
    }
}
