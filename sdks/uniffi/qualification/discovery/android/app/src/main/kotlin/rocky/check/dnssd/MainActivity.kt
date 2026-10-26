package rocky.check.dnssd

import android.app.Activity
import android.net.wifi.WifiManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.TextView
import uniffi.rocky_discovery_probe.Probe
import java.io.File

class MainActivity : Activity() {
    private lateinit var probe: Probe
    private lateinit var display: TextView
    private lateinit var multicast: WifiManager.MulticastLock
    private val handler = Handler(Looper.getMainLooper())
    private val refresh = object : Runnable {
        override fun run() {
            val status = probe.status()
            display.text = status
            File(filesDir, "result.json").writeText(status)
            handler.postDelayed(this, 1000)
        }
    }
    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        display = TextView(this).apply {
            textSize = 13f
            setPadding(20, 50, 20, 20)
        }
        setContentView(display)
        multicast = (applicationContext.getSystemService(WIFI_SERVICE) as WifiManager).createMulticastLock("rocky-dnssd-check")
        probe = Probe()
    }
    override fun onStart() {
        super.onStart()
        multicast.acquire()
        probe.foreground(true)
        handler.post(refresh)
    }
    override fun onStop() {
        handler.removeCallbacks(refresh)
        probe.foreground(false)
        File(filesDir, "result.json").writeText(probe.status())
        multicast.release()
        super.onStop()
    }
    override fun onDestroy() {
        probe.shutdown()
        probe.destroy()
        super.onDestroy()
    }
}
