package com.ainovel.engine

import android.annotation.SuppressLint
import android.content.Intent
import android.os.Bundle
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.appcompat.app.AppCompatActivity
import java.net.InetSocketAddress
import java.net.Socket

/**
 * 壳：启动前台服务（Rust 引擎），轮询就绪后把内嵌 Web UI 载入 WebView。
 * 引擎只监听 127.0.0.1，与 UI 同进程，无任何外部运行时依赖。
 */
class MainActivity : AppCompatActivity() {

    private lateinit var web: WebView

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        web = WebView(this)
        setContentView(web)
        web.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            cacheMode = WebSettings.LOAD_DEFAULT
        }
        web.webViewClient = WebViewClient()

        startForegroundService(Intent(this, EngineService::class.java))
        pollAndLoad(0)
    }

    private fun pollAndLoad(attempt: Int) {
        Thread {
            val ok = try {
                Socket().use {
                    it.connect(InetSocketAddress("127.0.0.1", EngineService.PORT), 500)
                }
                true
            } catch (_: Exception) {
                false
            }
            runOnUiThread {
                when {
                    ok -> web.loadUrl("http://127.0.0.1:${EngineService.PORT}/")
                    attempt < 150 -> web.postDelayed({ pollAndLoad(attempt + 1) }, 400)
                    else -> {
                        // 超时也加载：至少显示错误页面，不白屏
                        web.loadUrl("http://127.0.0.1:${EngineService.PORT}/")
                    }
                }
            }
        }.start()
    }

    @Suppress("DEPRECATION")
    override fun onBackPressed() {
        if (web.canGoBack()) {
            web.goBack()
        } else {
            @Suppress("DEPRECATION")
            super.onBackPressed()
        }
    }
}
