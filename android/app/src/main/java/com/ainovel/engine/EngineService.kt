package com.ainovel.engine

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/**
 * 前台服务：加载 Rust 引擎动态库，并在进程内启动本地服务（127.0.0.1:8787）。
 * 同时轮询引擎状态，把「生成完成 / 运行中断 / 待确认」推成系统通知。
 * 引擎与 UI 解耦：退出界面后创作任务仍可继续。
 */
class EngineService : Service() {

    @Volatile
    private var polling = false

    /** 上一次看到的运行状态（novelId -> run status），用于识别状态跃迁后发通知 */
    private val runStates = HashMap<String, String>()

    override fun onCreate() {
        super.onCreate()
        createChannels()
        startForeground(NOTIF_ID, buildNotification())
        startEngine()
        startPolling()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onDestroy() {
        polling = false
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    // ---------- 引擎启动 ----------

    private fun startEngine() {
        Thread {
            try {
                System.loadLibrary("ainovel_engine")
                val code = nativeStart(filesDir.absolutePath, PORT)
                Log.i(TAG, "nativeStart returned $code")
            } catch (t: Throwable) {
                Log.e(TAG, "engine start failed", t)
            }
        }.start()
    }

    /** Rust 侧: Java_com_ainovel_engine_EngineService_nativeStart */
    private external fun nativeStart(dataDir: String, port: Int): Int

    // ---------- 状态轮询 → 本地通知 ----------

    private fun startPolling() {
        polling = true
        Thread {
            // 给引擎一点启动时间
            Thread.sleep(4_000)
            while (polling) {
                try {
                    pollOnce()
                } catch (_: Exception) {
                }
                Thread.sleep(POLL_INTERVAL_MS)
            }
        }.also {
            it.isDaemon = true
            it.start()
        }
    }

    private fun pollOnce() {
        val conn = URL("http://127.0.0.1:$PORT/api/novels").openConnection() as HttpURLConnection
        conn.connectTimeout = 3_000
        conn.readTimeout = 5_000
        val body = conn.inputStream.bufferedReader().use { it.readText() }
        conn.disconnect()

        val items = JSONObject(body).optJSONArray("items") ?: return
        for (i in 0 until items.length()) {
            val item = items.optJSONObject(i) ?: continue
            val novel = item.optJSONObject("novel") ?: continue
            val id = novel.optString("id")
            if (id.isEmpty()) continue
            val title = novel.optString("title").ifBlank { "未命名作品" }
            val runStatus = item.optString("run_status")
            val prev = runStates[id]
            runStates[id] = runStatus
            if (prev == null || prev == runStatus) continue
            when (runStatus) {
                "done" -> notifyEvent("《$title》生成完成", "本次运行已完成，点击查看章节")
                "failed" -> notifyEvent("《$title》运行中断", item.optString("last_error").ifBlank { "点击查看详情" })
                "paused" -> notifyEvent("《$title》待确认", "章节规划已生成，确认后继续创作")
            }
        }
    }

    private fun notifyEvent(title: String, text: String) {
        val intent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val n = NotificationCompat.Builder(this, EVENTS_CHANNEL_ID)
            .setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_menu_edit)
            .setContentIntent(intent)
            .setAutoCancel(true)
            .build()
        try {
            getSystemService(NotificationManager::class.java)
                .notify((System.currentTimeMillis() % Int.MAX_VALUE).toInt(), n)
        } catch (_: Exception) {
        }
    }

    // ---------- 通知渠道 ----------

    private fun createChannels() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "创作引擎", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "本地 AI 创作引擎运行中"
                },
            )
            nm.createNotificationChannel(
                NotificationChannel(EVENTS_CHANNEL_ID, "创作进度", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "生成完成 / 中断 / 待确认提醒"
                },
            )
        }
    }

    private fun buildNotification(): Notification =
        NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("AI Novel Agent")
            .setContentText("本地引擎运行中（127.0.0.1:$PORT）")
            .setSmallIcon(android.R.drawable.ic_menu_edit)
            .setOngoing(true)
            .build()

    companion object {
        const val TAG = "AINOVEL-ENGINE"
        const val PORT = 8787
        const val CHANNEL_ID = "ainovel-engine"
        const val EVENTS_CHANNEL_ID = "ainovel-events"
        const val NOTIF_ID = 1001
        private const val POLL_INTERVAL_MS = 8_000L
    }
}
