package io.github.horhik.sample_creator

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.storage.StorageManager
import android.os.storage.StorageVolume
import android.provider.Settings
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/** Storage for reading recordings in place: "All files access" on Android 11+
 *  (the plain read permission before that) and the list of mounted volumes,
 *  which is where a recorder plugged in over USB OTG shows up. */
class MainActivity : FlutterActivity() {
    private var channel: MethodChannel? = null

    // Tells Dart when a volume comes or goes, e.g. the recorder is plugged in.
    private val mounts = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            channel?.invokeMethod("volumesChanged", null)
        }
    }

    // cpal opens audio through JNI and needs the app context; Rust keeps it.
    private external fun initAndroidContext(context: Context)

    override fun onCreate(savedInstanceState: Bundle?) {
        System.loadLibrary("sample_creator_bridge")
        initAndroidContext(applicationContext)
        super.onCreate(savedInstanceState)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val channel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "sample_creator/storage")
        this.channel = channel
        channel.setMethodCallHandler { call, result ->
                when (call.method) {
                    "granted" -> result.success(granted())
                    "volumes" -> result.success(volumes())
                    "request" -> {
                        request()
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_MEDIA_MOUNTED)
            addAction(Intent.ACTION_MEDIA_UNMOUNTED)
            addAction(Intent.ACTION_MEDIA_REMOVED)
            addAction(Intent.ACTION_MEDIA_EJECT)
            addDataScheme("file")
        }
        registerReceiver(mounts, filter)
    }

    override fun onDestroy() {
        try {
            unregisterReceiver(mounts)
        } catch (e: IllegalArgumentException) {
            // Never registered.
        }
        super.onDestroy()
    }

    /** Mounted volumes: path, system label, whether it can be unplugged. */
    private fun volumes(): List<Map<String, Any>> {
        val manager = getSystemService(StorageManager::class.java) ?: return emptyList()
        return manager.storageVolumes.mapNotNull { v ->
            val state = v.state
            if (state != Environment.MEDIA_MOUNTED && state != Environment.MEDIA_MOUNTED_READ_ONLY) {
                return@mapNotNull null
            }
            val path = volumePath(v) ?: return@mapNotNull null
            mapOf(
                "path" to path,
                "label" to v.getDescription(this),
                "removable" to v.isRemovable,
                "primary" to v.isPrimary,
            )
        }
    }

    private fun volumePath(v: StorageVolume): String? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            v.directory?.path
        } else {
            // Before Android 11 the path is only reachable through a hidden getter.
            try {
                v.javaClass.getMethod("getPath").invoke(v) as? String
            } catch (e: Exception) {
                null
            }
        }

    private fun granted(): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) ==
                PackageManager.PERMISSION_GRANTED
        }

    private fun request() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val intent = Intent(
                Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                Uri.parse("package:$packageName"),
            )
            try {
                startActivity(intent)
            } catch (e: Exception) {
                startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
            }
        } else {
            requestPermissions(arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE), 1)
        }
    }
}
