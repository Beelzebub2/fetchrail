package com.rrmtools.fetchrail

import android.app.job.JobInfo
import android.app.job.JobScheduler
import android.content.ComponentName
import android.content.Context
import android.os.PersistableBundle
import org.json.JSONArray
import org.json.JSONObject

/** Only the file paths returned by this context are ever passed to native code. */
val Context.downloadRoot: String
    get() = (getExternalFilesDir(null) ?: filesDir).absolutePath

object NativeBridge {
    init {
        System.loadLibrary("fetchrail_android_engine")
    }

    external fun enqueue(root: String, url: String): String
    external fun list(root: String): String
    external fun run(root: String, id: String): String
    external fun pause(root: String, id: String): String
    external fun stop(root: String, id: String): String
    external fun resume(root: String, id: String): String
    external fun remove(root: String, id: String): String

    fun result(response: String): JSONObject {
        val payload = JSONObject(response)
        if (!payload.optBoolean("ok")) {
            throw IllegalStateException(payload.optString("error", "Unknown download error"))
        }
        return payload
    }

    fun downloads(root: String): List<TransferRecord> {
        val array = result(list(root)).optJSONArray("value") ?: JSONArray()
        return (0 until array.length()).map { index ->
            val entry = array.getJSONObject(index)
            TransferRecord(
                id = entry.getString("id"),
                fileName = entry.getString("fileName"),
                status = entry.getString("status"),
                url = entry.optString("url"),
                totalBytes = entry.optLong("totalBytes").takeIf { !entry.isNull("totalBytes") },
                downloadedBytes = entry.optLong("downloadedBytes"),
                speedBps = entry.optLong("speedBps"),
                connections = entry.optInt("connections"),
                savedPath = entry.optString("savedPath").takeIf { !entry.isNull("savedPath") },
                error = entry.optString("error").takeIf { !entry.isNull("error") },
            )
        }
    }
}

data class TransferRecord(
    val id: String,
    val fileName: String,
    val status: String,
    val url: String,
    val totalBytes: Long?,
    val downloadedBytes: Long,
    val speedBps: Long,
    val connections: Int,
    val savedPath: String?,
    val error: String?,
)

object JobControl {
    private fun jobId(id: String) = (id.hashCode() and Int.MAX_VALUE).coerceAtLeast(1)

    fun enqueue(context: Context, id: String) {
        // UIDT must be scheduled directly after a visible user gesture.
        val extras = PersistableBundle().apply { putString("downloadId", id) }
        val job = JobInfo.Builder(
            jobId(id),
            ComponentName(context, DownloadJobService::class.java)
        )
            .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
            .setUserInitiated(true)
            .setExtras(extras)
            .build()
        val scheduler = context.getSystemService(JobScheduler::class.java)
        check(scheduler.schedule(job) == JobScheduler.RESULT_SUCCESS) {
            "Android could not schedule the download. Check battery or network restrictions."
        }
    }

    fun cancel(context: Context, id: String) {
        context.getSystemService(JobScheduler::class.java).cancel(jobId(id))
    }
}
