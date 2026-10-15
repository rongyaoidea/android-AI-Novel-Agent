package com.ainovel.engine

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat

/**
 * 前台服务：加载 Rust 引擎动态库，并在进程内启动本地服务（127.0.0.1:8787）。
 * 引擎生命周期与 UI 解耦，退出界面后创作任务仍可继续。
 */
class EngineService : Service() {

    override fun onCreate() {
        super.onCreate()
        createChannel()
        startForeground(NOTIF_ID, buildNotification())
        startEngine()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onBind(intent: Intent?): IBinder? = null

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

    private fun createChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = NotificationChannel(CHANNEL_ID, "创作引擎", NotificationManager.IMPORTANCE_LOW)
            ch.description = "本地 AI 创作引擎运行中"
            getSystemService(NotificationManager::class.java).createNotificationChannel(ch)
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
        const val NOTIF_ID = 1001
    }
}
