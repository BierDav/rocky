@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.pairing

import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.UIKitInteropInteractionMode
import androidx.compose.ui.viewinterop.UIKitInteropProperties
import androidx.compose.ui.viewinterop.UIKitView
import platform.CoreGraphics.CGRectMake
import platform.Foundation.NSNotificationCenter
import platform.Foundation.NSOperationQueue
import platform.Foundation.setValue
import platform.UIKit.UIColor
import platform.UIKit.UIPasteConfiguration
import platform.UIKit.UIPasteConfigurationSupportingProtocol
import platform.UIKit.UIPasteControl
import platform.UIKit.UIPasteControlConfiguration
import platform.UIKit.UITextView
import platform.UIKit.UITextViewTextDidChangeNotification
import platform.UIKit.UIView
import platform.UIKit.accessibilityElementsHidden
import platform.UIKit.accessibilityLabel

@Composable
actual fun PasteCode(label: String, onText: (String) -> Unit, modifier: Modifier) {
    val receive by rememberUpdatedState(onText)
    // UIKit owns text decoding and paste eligibility; Compose owns the visible field.
    val input = remember {
        UITextView(frame = CGRectMake(0.0, 0.0, 150.0, 44.0)).apply {
            (this as UIPasteConfigurationSupportingProtocol).setPasteConfiguration(UIPasteConfiguration(acceptableTypeIdentifiers = listOf("public.utf8-plain-text")))
            backgroundColor = UIColor.clearColor
            textColor = UIColor.clearColor
            scrollEnabled = false
            inputView = UIView()
            accessibilityElementsHidden = true
            userInteractionEnabled = false
        }
    }
    DisposableEffect(input) {
        val center = NSNotificationCenter.defaultCenter
        val observer = center.addObserverForName(UITextViewTextDidChangeNotification, input, NSOperationQueue.mainQueue) {
            val pasted = input.text.orEmpty()
            input.text = ""
            input.resignFirstResponder()
            receive(pasted)
        }
        onDispose { center.removeObserver(observer) }
    }
    val content = remember(input) {
        UIView(frame = CGRectMake(0.0, 0.0, 150.0, 44.0)).apply {
            backgroundColor = UIColor.clearColor
            addSubview(input)
            addSubview(
                UIPasteControl(configuration = UIPasteControlConfiguration()).apply {
                    setFrame(CGRectMake(0.0, 0.0, 150.0, 44.0))
                    target = input as UIPasteConfigurationSupportingProtocol
                    accessibilityLabel = label
                    setValue(label, forKey = "accessibilityIdentifier")
                },
            )
        }
    }
    UIKitView(
        factory = { content },
        modifier = modifier.width(150.dp).height(44.dp),
        properties = UIKitInteropProperties(interactionMode = UIKitInteropInteractionMode.NonCooperative, isNativeAccessibilityEnabled = true),
    )
}
