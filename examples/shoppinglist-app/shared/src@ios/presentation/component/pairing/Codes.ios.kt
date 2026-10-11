@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.shopping.presentation.component.pairing

import platform.AVFoundation.AVCaptureDevice
import platform.AVFoundation.AVMediaTypeVideo

actual val canScanCodes: Boolean get() = AVCaptureDevice.defaultDeviceWithMediaType(AVMediaTypeVideo) != null
