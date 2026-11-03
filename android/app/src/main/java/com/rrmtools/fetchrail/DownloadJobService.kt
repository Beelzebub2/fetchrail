package com.rrmtools.fetchrail

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.job.JobParameters
import android.app.job.JobService
import android.content.Intent
import android.os.Build
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Android 14+ user-initiated data transfers, not a long-lived service started
 * from the Activity. The OS may restart the job; Rust validates persisted ranges.
 */
class DownloadJobService : JobService() {
    private val stopped = ConcurrentHashMap.newKeySet<Int>()
    private val workers = ConcurrentHashMap<Int, Thread>()

    override fun onStartJob(params: JobParameters): Boolean {
        val id = params.extras.getString("downloadId") ?: return false
        val jobId = params.jobId
        val notifications = getSystemService(NotificationManager::class.java)
        notifications.createNotificationChannel(
            NotificationChannel(CHANNEL, "File downloads", NotificationManager.IMPORTANCE_LOW)
        )
        val initial = notification("Preparing download", 0, true)
        // UIDT requires an immediate notification while the job is executing.
        setNotification(params, jobId, initial, JOB_END_NOTIFICATION_POLICY_REMOVE)

        val worker = Thread({
            val finished = AtomicBoolean(false)
            val monitor = Thread({
                while (!finished.get()) {
                    try {
                        val entry = NativeBridge.downloads(downloadRoot).firstOrNull { it.id == id }
                        if (entry != null) {
                            val percent = entry.totalBytes?.takeIf { it > 0 }?.let {
                                ((entry.downloadedBytes.coerceAtMost(it) * 100.0) / it).toInt()
                            } ?: 0
                            notifications.notify(
                                jobId,
                                notification(entry.fileName, percent, entry.totalBytes == null)
                            )
                        }
                    } catch (_: Exception) {
                        // A transient state read cannot interrupt the transfer itself.
                    }
                    try {
                        Thread.sleep(1000)
                    } catch (_: InterruptedException) {
                        break
                    }
                }
            }, "fetchrail-progress-$jobId").apply { isDaemon = true }
            monitor.start()
            try {
                NativeBridge.result(NativeBridge.run(downloadRoot, id))
            } catch (_: Exception) {
                // Rust persisted the failure and checkpoint; the UI shows retry.
            } finally {
                finished.set(true)
                monitor.interrupt()
                workers.remove(jobId)
                if (!stopped.remove(jobId)) jobFinished(params, false)
            }
        }, "fetchrail-transfer-$jobId")
        workers[jobId] = worker
        worker.start()
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        stopped.add(params.jobId)
        val id = params.extras.getString("downloadId") ?: return false
        try {
            NativeBridge.result(NativeBridge.stop(downloadRoot, id))
            val status = NativeBridge.downloads(downloadRoot).firstOrNull { it.id == id }?.status
            return status != "paused" && status != "completed" && status != "failed"
        } catch (_: Exception) {
            return true
        }
    }

    private fun notification(title: String, percent: Int, indefinite: Boolean): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_fetchrail)
            .setContentTitle("Fetchrail")
            .setContentText(title)
            .setContentIntent(open)
            .setCategory(Notification.CATEGORY_PROGRESS)
            .setOnlyAlertOnce(true)
            .setOngoing(true)
            .setProgress(100, percent, indefinite)
            .build()
    }

    companion object {
        private const val CHANNEL = "fetchrail-transfers"
    }
}
