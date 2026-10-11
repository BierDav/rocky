package org.example.examples.shopping.presentation.theme

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.graphics.vector.group
import androidx.compose.ui.unit.dp

/** The app icon's colour, which seeds the theme; Android's `@color/brand` and iOS's `LaunchBackground` match it. */
val BrandColor = Color(0xFF98D4E3)

/** The app icon's white cart, cropped to its bounds, as on the launch screens. */
val BrandLogo: ImageVector = ImageVector.Builder("BrandLogo", 96.dp, 96.dp, 411f, 411f).apply {
    group(translationX = -44f, translationY = -45f) {
            addPath(addPathNodes("M396.6,264.81l39,-133c0.88,-3.01 0.3,-6.27 -1.58,-8.79s-4.83,-4.01 -7.97,-4.03l-293.94,-1.51C122.82,93.62 99.58,77 73,77c-5.52,0 -10,4.48 -10,10s4.48,10 10,10c21.78,0 40.29,16.29 43.07,37.95l19.18,142.54c2.07,16.11 10.04,30.75 22.45,41.24c11.73,9.91 26.48,15.29 41.77,15.29c0.82,0 1.63,-0.02 2.45,-0.05L399.05,333c5.52,-0.03 9.98,-4.53 9.95,-10.05c-0.03,-5.51 -4.5,-9.95 -10,-9.95c-0.02,0 -0.03,0 -0.05,0l-197.3,0.97c-0.12,0 -0.24,0 -0.35,0.01c-23.37,0.94 -43.23,-15.84 -46.22,-39.1l-0.24,-1.76l232.2,-1.13C391.47,271.98 395.35,269.06 396.6,264.81zM153.13,253.14l-15.85,-115.62l275.38,1.41l-33.17,113.1L153.13,253.14z"), fill = SolidColor(Color.White))
            addPath(addPathNodes("M180,360c13.23,0 24,10.77 24,24s-10.77,24 -24,24s-24,-10.77 -24,-24S166.77,360 180,360M180,345c-21.54,0 -39,17.46 -39,39s17.46,39 39,39s39,-17.46 39,-39S201.54,345 180,345L180,345z"), fill = SolidColor(Color.White))
            addPath(addPathNodes("M352,360c13.23,0 24,10.77 24,24s-10.77,24 -24,24s-24,-10.77 -24,-24S338.77,360 352,360M352,345c-21.54,0 -39,17.46 -39,39s17.46,39 39,39s39,-17.46 39,-39S373.54,345 352,345L352,345z"), fill = SolidColor(Color.White))
    }
}.build()
