# Menus

Standard application menus for loadngo apps, added 2026-10-10 at Jay's
request with the code editor (`docs/CODE_EDITOR.md`) as the first user.

## What every app gets

On macOS the host installs the standard menu bar at launch, before the app
asks for anything:

- the application menu: Hide (Cmd-H), Hide Others (Cmd-Option-H), Show All,
  Quit (Cmd-Q);
- a Window menu: Minimize (Cmd-M), Zoom.

Before this, loadngo apps had no menu bar at all, and Cmd-Q did nothing. Quit
closes the app unless the app asks to decide (below).

The other hosts have no system menu bar, so they install nothing.

## Describing an app's menus

An app describes its menus as a `ui_core::MenuBar`: menus of `MenuItem`s,
each a `MenuCommand` (a number the app chooses), a title, an optional
`Shortcut` and an enabled flag. A shortcut always includes the platform's
primary modifier: Cmd on macOS, Ctrl elsewhere.

```rust
let bar = MenuBar {
    menus: vec![Menu::new("File", vec![
        MenuItem::command(SAVE, "Save")
            .with_shortcut(Shortcut::primary('s'))
            .enabled(dirty),
    ])],
    quit: Some(QUIT), // or None: the host quits by itself
};
let native = loadngo_host_desktop::set_menu_bar(&bar);
```

`set_menu_bar` returns whether the host shows the menus itself.

- **macOS (`true`).** The menus go between the application and Window
  menus. A chosen item, whether clicked or reached by its key equivalent,
  arrives in the next frame's `InputSnapshot::menu_commands` and wakes an
  idle frame. Cmd-key presses go to the menu bar first, as in any AppKit
  app; a key an enabled item takes never reaches `key_events`, so a shortcut
  cannot fire twice. Call `set_menu_bar` again when titles or enabled states
  change; each call rebuilds the menu bar, so call it on change, not every
  frame. With `quit: Some(command)`, the application menu's Quit sends that
  command, and the app can ask about unsaved work before quitting.
- **Linux, Windows, iOS, Android (`false`).** The app draws a
  `ui_core::MenuBarModel` across the top of its window. It feeds the model
  events before its own handling (the model takes shortcut keys, and all
  pointer input while a menu is open), and paints it after its own content,
  so an open menu draws on top. The model turns clicks, arrow keys plus
  Enter, and shortcuts into the same `MenuCommand`s. There is no
  application menu here, so an app lists its own Quit item.

Either way the app acts on commands, never on the key chord itself.

## Not done yet

- Windows and Linux could later use native menus (a Win32 `HMENU`; GTK or
  a D-Bus global menu on Linux). The drawn bar is what they have today; it
  is checked by unit tests and, on macOS, with `CODE_EDITOR_DRAWN_MENU=1`.
  It has not been run on those hosts.
- Submenus, checkmarks and per-window menus.
- The window's close button still closes the app at once; there is no
  way to veto it yet.
