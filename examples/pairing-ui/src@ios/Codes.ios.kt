@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.pairing

import platform.AVFoundation.AVCaptureDevice
import platform.AVFoundation.AVMediaTypeVideo

actual val canScanCodes: Boolean get() = AVCaptureDevice.defaultDeviceWithMediaType(AVMediaTypeVideo) != null
