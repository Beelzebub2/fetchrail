package com.rrmtools.fetchrail

import android.app.Activity
import android.content.Intent
import android.os.Bundle

/**
 * URLCheck asks Android for ACTION_VIEW candidates and launches its chosen
 * component explicitly. Keep that exported entry point separate from the main
 * launcher so the chooser says "Download with Fetchrail", not just "Fetchrail".
 * No network request starts here: the user must confirm in the main screen.
 */
class DownloadLinkActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val url = IncomingLinks.fromView(intent)
        if (url != null) {
            val open = Intent(this, MainActivity::class.java).apply {
                action = Intent.ACTION_VIEW
                data = url
                flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
            }
            startActivity(open)
        }
        finish()
    }
}
