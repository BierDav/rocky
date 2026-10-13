package org.example.examples.shopping

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.text.AnnotatedString
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.presentation.screen.lists.ItemRow
import org.example.examples.shopping.presentation.screen.lists.ListsState
import kotlin.test.*

@OptIn(ExperimentalComposeUiApi::class)
class ItemEditingTest {
    @Test fun `one title click edits inline and Done saves without toggling completion`() {
        val row = mutableStateOf(ShoppingItem(1, "milk", "Milk", false, id = 1))
        val renamed = mutableListOf<String>()
        var toggled = 0
        lateinit var dismissFocus: () -> Unit
        val scene = ImageComposeScene(400, 120) {
            MaterialTheme {
                val focus = LocalFocusManager.current
                dismissFocus = { focus.clearFocus(force = true) }
                ItemRow(row.value, ListsState(items = listOf(row.value)), { toggled++ }, { _, _ -> }, {}, {}, renamed::add, {}, {})
            }
        }
        try {
            fun frame() = scene.render().close()
            fun nodes(): List<SemanticsNode> {
                fun descendants(node: SemanticsNode): List<SemanticsNode> = listOf(node) + node.children.flatMap(::descendants)
                return scene.semanticsOwners.flatMap { descendants(it.unmergedRootSemanticsNode) }
            }
            fun editor() = nodes().single { it.config.contains(SemanticsActions.SetText) }
            fun clickTitle(title: String) {
                val label = nodes().single { it.config.getOrNull(SemanticsProperties.Text)?.any { text -> text.text == title } == true }
                assertTrue(requireNotNull(label.config[SemanticsActions.OnClick].action).invoke())
                repeat(3) { frame() }
            }
            frame()
            clickTitle("Milk")
            assertTrue(editor().config[SemanticsProperties.Focused])
            assertTrue(requireNotNull(editor().config[SemanticsActions.SetText].action).invoke(AnnotatedString("Oat milk")))
            frame()
            row.value = row.value.copy(completed = true)
            frame()
            assertEquals("Oat milk", editor().config[SemanticsProperties.EditableText].text)
            assertTrue(requireNotNull(editor().config[SemanticsActions.OnImeAction].action).invoke())
            frame()
            assertEquals(listOf("Oat milk"), renamed)
            assertEquals(0, toggled)
            assertTrue(nodes().none { it.config.contains(SemanticsActions.SetText) })
            clickTitle("Milk")
            assertTrue(requireNotNull(editor().config[SemanticsActions.SetText].action).invoke(AnnotatedString("  ")))
            frame()
            assertTrue(requireNotNull(editor().config[SemanticsActions.OnImeAction].action).invoke())
            frame()
            assertEquals(listOf("Oat milk"), renamed)
            assertEquals(0, toggled)
            clickTitle("Milk")
            assertTrue(requireNotNull(editor().config[SemanticsActions.SetText].action).invoke(AnnotatedString("Cream")))
            frame()
            dismissFocus()
            frame()
            assertEquals(listOf("Oat milk", "Cream"), renamed)
            assertTrue(nodes().none { it.config.contains(SemanticsActions.SetText) })
            val checkbox = nodes().single { it.config.getOrNull(SemanticsProperties.Role) == Role.Checkbox }
            assertTrue(requireNotNull(checkbox.config[SemanticsActions.OnClick].action).invoke())
            assertEquals(1, toggled)
        } finally {
            scene.close()
        }
    }
}
