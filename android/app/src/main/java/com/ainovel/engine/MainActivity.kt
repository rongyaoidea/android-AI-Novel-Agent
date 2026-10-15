package com.ainovel.engine

import android.Manifest
import android.annotation.SuppressLint
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.MediaStore
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import java.net.HttpURLConnection
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URL

/**
 * 壳：启动前台服务（Rust 引擎），轮询就绪后把内嵌 Web UI 载入 WebView。
 * 引擎只监听 127.0.0.1，与 UI 同进程；/api 请求用本机令牌（?token=）鉴权，
 * 防止同机其它 App 通过 8787 端口读取作品数据。
 *
 * 另补齐 WebView 两个缺口：
 * - DownloadListener：导出 TXT / 备份 zip 保存到系统「下载」
 * - onShowFileChooser：恢复备份时选 zip 文件
 */
class MainActivity : AppCompatActivity() {

    private lateinit var web: WebView
    private var pendingFilePathCallback: ValueCallback<Array<Uri>>? = null

    private val authToken: String by lazy { SecretStore.getOrCreateAuthToken(applicationContext) }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        web = WebView(this)
        setContentView(web)
        // Android 15（targetSdk 35）起系统强制 edge-to-edge：页面会绘制到状态栏 / 导航栏之下，
        // 导致顶部导航被系统栏压住、点不到。这里把系统栏 insets 转为 WebView 内边距；
        // 35+ 再叠加输入法 insets（edge-to-edge 下 adjustResize 不再生效）。
        ViewCompat.setOnApplyWindowInsetsListener(web) { view, insets ->
            val sys = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout(),
            )
            var bottom = sys.bottom
            if (Build.VERSION.SDK_INT >= 35) {
                bottom = maxOf(bottom, insets.getInsets(WindowInsetsCompat.Type.ime()).bottom)
            }
            view.setPadding(sys.left, sys.top, sys.right, bottom)
            insets
        }
        web.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            cacheMode = WebSettings.LOAD_DEFAULT
        }
        web.webViewClient = WebViewClient()
        web.webChromeClient = object : WebChromeClient() {
            override fun onShowFileChooser(
                webView: WebView?,
                filePathCallback: ValueCallback<Array<Uri>>?,
                fileChooserParams: FileChooserParams?,
            ): Boolean {
                pendingFilePathCallback?.onReceiveValue(null)
                pendingFilePathCallback = filePathCallback
                return try {
                    val accept = fileChooserParams?.acceptTypes?.firstOrNull { it.isNotEmpty() } ?: "*/*"
                    val intent = Intent(Intent.ACTION_GET_CONTENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        type = if (accept.startsWith(".")) "*/*" else accept
                    }
                    startActivityForResult(intent, REQUEST_FILE)
                    true
                } catch (_: Exception) {
                    pendingFilePathCallback = null
                    false
                }
            }
        }
        web.setDownloadListener { url, _, contentDisposition, mimeType, _ ->
            // 只放行本机引擎的下载（导出 TXT / 备份 zip）
            if (url.startsWith("http://127.0.0.1:${EngineService.PORT}/")) {
                Thread { saveToDownloads(url, contentDisposition, mimeType) }.start()
            }
        }

        requestNotificationPermission()
        startForegroundService(Intent(this, EngineService::class.java))
        pollAndLoad(0)
    }

    /** Android 13+ 需要运行时授权才能显示「创作进度」通知 */
    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1001)
        }
    }

    // ---------- 下载（导出 / 备份） ----------

    private fun saveToDownloads(url: String, contentDisposition: String?, mimeType: String?) {
        try {
            val conn = URL(url).openConnection() as HttpURLConnection
            conn.connectTimeout = 5_000
            conn.readTimeout = 120_000
            conn.setRequestProperty("x-ainovel-token", authToken)
            val disposition = contentDisposition ?: conn.getHeaderField("Content-Disposition")
            val name = filenameFrom(disposition) ?: "ainovel-download.bin"
            val type = mimeType?.takeIf { it.isNotBlank() }
                ?: conn.getHeaderField("Content-Type")?.substringBefore(';')?.trim()
                ?: "application/octet-stream"
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, type)
            }
            val uri = contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: throw IllegalStateException("无法创建下载条目")
            conn.inputStream.use { input ->
                contentResolver.openOutputStream(uri)?.use { output -> input.copyTo(output) }
            }
            conn.disconnect()
            toast("已保存到「下载」：$name")
        } catch (t: Throwable) {
            toast("下载失败：${t.message}")
        }
    }

    private fun filenameFrom(disposition: String?): String? {
        val m = Regex("filename=\"?([^\";]+)\"?", RegexOption.IGNORE_CASE).find(disposition ?: "")
            ?: return null
        return m.groupValues[1].trim().takeIf { it.isNotEmpty() }
    }

    private fun toast(msg: String) {
        runOnUiThread { Toast.makeText(this, msg, Toast.LENGTH_LONG).show() }
    }

    // ---------- 文件选择（恢复备份） ----------

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == REQUEST_FILE) {
            val cb = pendingFilePathCallback
            pendingFilePathCallback = null
            val uris = if (resultCode == RESULT_OK && data?.data != null) {
                arrayOf(data.data!!)
            } else {
                null
            }
            cb?.onReceiveValue(uris)
            return
        }
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
    }

    // ---------- 加载 UI ----------

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
                    ok -> web.loadUrl(uiUrl())
                    attempt < 150 -> web.postDelayed({ pollAndLoad(attempt + 1) }, 400)
                    else -> {
                        // 超时也加载：至少显示错误页面，不白屏
                        web.loadUrl(uiUrl())
                    }
                }
            }
        }.start()
    }

    /** 令牌走查询参数：页面 JS 会暂存后从地址栏清掉，之后改为请求头 */
    private fun uiUrl(): String =
        "http://127.0.0.1:${EngineService.PORT}/?token=$authToken#/shelf"

    @Suppress("DEPRECATION")
    override fun onBackPressed() {
        if (web.canGoBack()) {
            web.goBack()
        } else {
            @Suppress("DEPRECATION")
            super.onBackPressed()
        }
    }

    companion object {
        private const val REQUEST_FILE = 2001
    }
}
