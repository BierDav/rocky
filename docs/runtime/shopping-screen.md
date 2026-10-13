# Shopping list screen

The shared Compose screen separates committed data from editing state:

- `ListsViewModel.state` observes the selected list's complete items and categories.
  Changing a search, category or completion filter does not restart a Room query.
- `ListsEditor` owns the search and new-item `TextFieldState`, category selection
  and completion visibility. Compose reads these synchronously, including caret
  selection and IME composition. Database emissions never overwrite editor text.
- `ListsState.content` filters the observed rows without reordering them.
  Checking an item leaves it in place. Progress always describes the complete
  list. Category identity includes its list; a deleted or other-list category falls back to all
  items.

The editors live with the screen ViewModel, so returning from another tab keeps
unfinished input. They do not persist across process termination. Filtering is
in memory and is intended for the example's shopping-sized lists.

Adding an item captures the current list, category and trimmed draft before the
write starts. Another submission is disabled while it is pending. The draft clears
only after a successful commit, and only if its text has not changed meanwhile.
Failures retain the draft and use the existing snackbar error channel.

## Controls

The list overflow menu contains **Share list** for every device type and
**Show/Hide completed**. Trusted devices also get rename and confirmed deletion.
Trusted devices select recipients from the issuer registry; members select
discovered peers without reading a registry. Sharing retains the existing
whole-list reader and delegation checks.

Selecting a category filters the rows to its items and puts new items in it.
An item's menu toggles each category and moves it up or down the whole list,
whatever the filters show; see [schema 7](shopping-schema-7.md).
**All items** clears that selection. Category creation and deletion are under
**Categories**; deleting one keeps its items, as defined by the repository.

While search, new-item input or an inline item editor has focus, a visible **Done** button above the
composer clears focus and hides the software keyboard on every platform,
including iOS. Search's keyboard action dismisses focus; the new-item keyboard
Done action submits nonblank input and dismisses focus. The add button keeps
focus for entering several items. Switching lists also dismisses focus.

Keyboard insets are applied by the navigation shell before it measures the top
bar and screen. The bottom tab bar hides while the keyboard is open. The screen's
headers and rows share one scrolling region, with the composer pinned below it,
so a short keyboard viewport can scroll without clipping either editor. Android
explicitly uses `adjustResize` to deliver IME insets instead of panning the window.

Tapping an item's title opens an editor directly in that row. Keyboard **Done**,
leaving the field, or leaving the row saves a changed, nonblank title. Empty
edits keep the existing title. The checkbox changes completion without moving
the row; the item menu contains categorization, moving and deletion.

## Verification

`ListsStateTest` checks intersecting filters, progress independent of filters,
stable ordering after checking and filtering, deleted and other-list category
identities, and immediate text filtering with caret preservation across a data
refresh. `ItemEditingTest` activates the title in one click, types into the real
Compose editor, keeps the draft across a row update, saves on Done or focus loss,
and checks that the checkbox remains a separate action. The application checks
compile the shared code for JVM, Android and iOS; device interaction remains a
separate [platform gate](shopping-devices.md).
