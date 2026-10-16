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
import android.widget.FrameLayout
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
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

    /** 最近一次页面路由（#/novel/xxx 等）：Activity 重建后按此恢复，不至于跳回书架 */
    @Volatile
    private var lastRoute: String = "#/shelf"

    private val authToken: String by lazy { SecretStore.getOrCreateAuthToken(applicationContext) }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        lastRoute = savedInstanceState?.getString(STATE_ROUTE) ?: "#/shelf"

        web = WebView(this)
        // 关键 ①：insets 监听必须挂在外层容器上——WebView（Chromium）会自行消费系统 insets
        // （用于 CSS safe-area-inset-*），挂在 WebView 自身的监听器可能收不到回调，
        // 导致 Android 15/16 强制 edge-to-edge 时顶部被状态栏压住。
        // 关键 ②：全版本统一 edge-to-edge（setDecorFitsSystemWindows(false)）+ 自行处理
        // 系统栏与输入法 insets，行为跨版本一致；处理后返回 CONSUMED，避免子视图重复计算。
        val root = FrameLayout(this)
        root.addView(
            web,
            FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT,
            ),
        )
        setContentView(root)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout(),
            )
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            view.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
            WindowInsetsCompat.CONSUMED
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

        // Web 主题 → 原生状态栏联动：页面调用 window.AiNovelNative.setDark(true/false)
        // 路由记忆 → Activity 重建后恢复页面：window.AiNovelNative.setRoute("#/novel/xxx")
        web.addJavascriptInterface(
            object {
                @android.webkit.JavascriptInterface
                fun setDark(dark: Boolean) {
                    runOnUiThread {
                        val controller = WindowCompat.getInsetsController(window, root)
                        controller.isAppearanceLightStatusBars = !dark
                        controller.isAppearanceLightNavigationBars = !dark
                    }
                }

                @android.webkit.JavascriptInterface
                fun setRoute(route: String) {
                    if (route.startsWith("#/")) {
                        lastRoute = route
                    }
                }
            },
            "AiNovelNative",
        )

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
                } && engineHandshake()
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

    /**
     * 健康握手：确认 8787 上跑的确实是本应用引擎。
     * 任何同机 App 都能绑回环端口；不做校验就会把访问令牌交给占用端口的第三方页面。
     */
    private fun engineHandshake(): Boolean {
        return try {
            val conn = URL("http://127.0.0.1:${EngineService.PORT}/api/health").openConnection() as HttpURLConnection
            conn.connectTimeout = 1_000
            conn.readTimeout = 1_000
            val body = conn.inputStream.bufferedReader().use { it.readText() }
            conn.disconnect()
            body.contains("\"name\":\"ainovel-engine\"")
        } catch (_: Exception) {
            false
        }
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString(STATE_ROUTE, lastRoute)
    }

    /** 令牌走查询参数：页面 JS 会暂存后从地址栏清掉，之后改为请求头；附带上次路由 */
    private fun uiUrl(): String {
        val frag = if (lastRoute.startsWith("#/")) lastRoute else "#/shelf"
        return "http://127.0.0.1:${EngineService.PORT}/?token=$authToken$frag"
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

    companion object {
        private const val REQUEST_FILE = 2001
        private const val STATE_ROUTE = "ainovel.lastRoute"
    }
}
