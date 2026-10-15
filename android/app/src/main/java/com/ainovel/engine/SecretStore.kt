package com.ainovel.engine

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import java.security.SecureRandom
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * 本机密钥管理（信封加密）：
 *
 * - [getOrCreateSecretHex]：随机 32 字节「数据密钥」，用 Android Keystore 里的主密钥
 *   （不可导出、随应用卸载销毁）包裹后存本应用私有 SharedPreferences；进程内解包为 hex
 *   交给 Rust 引擎，用于 API Key 等字段的 AES-256-GCM 加密。
 * - [getOrCreateAuthToken]：本地服务访问令牌（32 位 hex）。引擎要求 /api 请求携带
 *   `x-ainovel-token`，防止同机其它 App 读取作品与密钥；令牌存私有 SharedPreferences，
 *   服务重启后保持不变。
 */
object SecretStore {

    private const val PREF = "ainovel-secrets"
    private const val PREF_WRAPPED_KEY = "wrapped_data_key"
    private const val PREF_AUTH_TOKEN = "auth_token"
    private const val KEY_ALIAS = "ainovel-master"
    private const val TRANSFORM = "AES/GCM/NoPadding"
    private const val GCM_TAG_BITS = 128
    private const val IV_LEN = 12

    /** 数据密钥（hex，64 字符）；失败时返回空串（引擎退化为明文存储） */
    @Synchronized
    fun getOrCreateSecretHex(context: Context): String {
        val prefs = context.getSharedPreferences(PREF, Context.MODE_PRIVATE)
        prefs.getString(PREF_WRAPPED_KEY, null)?.let { stored ->
            runCatching { unwrap(stored) }.getOrNull()?.let { return toHex(it) }
            // 解包失败（Keystore 数据丢失）→ 重新生成；旧密文将无法解密，由用户重填 Key
        }
        return runCatching {
            val secret = ByteArray(32).also { SecureRandom().nextBytes(it) }
            prefs.edit().putString(PREF_WRAPPED_KEY, wrap(secret)).apply()
            toHex(secret)
        }.getOrDefault("")
    }

    /** 本地服务访问令牌（hex）；生成一次后持久保存 */
    @Synchronized
    fun getOrCreateAuthToken(context: Context): String {
        val prefs = context.getSharedPreferences(PREF, Context.MODE_PRIVATE)
        prefs.getString(PREF_AUTH_TOKEN, null)?.takeIf { it.length >= 16 }?.let { return it }
        val token = UUID.randomUUID().toString().replace("-", "") +
            UUID.randomUUID().toString().replace("-", "").substring(0, 8)
        prefs.edit().putString(PREF_AUTH_TOKEN, token).apply()
        return token
    }

    private fun masterKey(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getEntry(KEY_ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    private fun wrap(plain: ByteArray): String {
        val cipher = Cipher.getInstance(TRANSFORM)
        cipher.init(Cipher.ENCRYPT_MODE, masterKey())
        val iv = cipher.iv
        val ct = cipher.doFinal(plain)
        return Base64.encodeToString(iv + ct, Base64.NO_WRAP)
    }

    private fun unwrap(stored: String): ByteArray {
        val raw = Base64.decode(stored, Base64.NO_WRAP)
        require(raw.size > IV_LEN) { "wrapped key too short" }
        val iv = raw.copyOfRange(0, IV_LEN)
        val ct = raw.copyOfRange(IV_LEN, raw.size)
        val cipher = Cipher.getInstance(TRANSFORM)
        cipher.init(Cipher.DECRYPT_MODE, masterKey(), GCMParameterSpec(GCM_TAG_BITS, iv))
        return cipher.doFinal(ct)
    }

    private fun toHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) sb.append(String.format("%02x", b))
        return sb.toString()
    }
}
