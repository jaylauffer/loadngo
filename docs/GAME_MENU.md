# Game menus

`loadngo-game-menu` (`game-menu/`) holds the screens every loadngo game
shows around play: a title menu, a sound settings panel and an achievements
list. Added 2026-10-12 at Jay's request, so that sng-roguelite, sng-zhoenus
and sng-mahjong open on a real menu rather than "press Space / tap to
start". These are game screens drawn by the game; for an application's
menu bar (File, Edit), see [MENUS.md](MENUS.md).

## What it provides

| Type | Screen |
|---|---|
| `TitleMenu` | The game's name over a column of buttons. The game picks the entries from `TitleAction::{Start, Sound, Achievements, Quit}`; the first is the default choice and is focused when the menu opens. |
| `SoundPanel` | One slider per `SoundLevel` (Master, Music or Effects), and a close button. |
| `AchievementsPanel` | A scrolling list of `AchievementRow`s, earned ones in the theme's `earned` colour, with a close button and an optional desktop hint. |
| `MenuTheme` | The colours. `Default` is sng-roguelite's palette. |
| `menu_frame_demand` | The frame demand for a menu screen (below). |

The crate owns layout and input. The game owns everything else: the
strings (already localized), the colours, the audio mixer, persistence and
the achievement records. It depends on `ui-core`, `loadngo-host-core` and
`loadngo-touch`, not on `loadngo-host-desktop`, so it is plain Rust and its
tests run on every platform.

## Using it

Each screen is a model the game keeps across frames. Per frame:

1. `update(&frame.input, frame.surface, frame.insets, delta)` returns what
   the player did: a `TitleAction`, a `SoundPanelResponse` (volumes that
   changed, and whether the panel closed), or `true` when the achievements
   panel closed.
2. `paint(&mut ops, &theme, surface, insets)` appends `RenderOp`s; render
   them with `loadngo_host_desktop::render_ops(&ops, None)`. A panel paints
   a scrim and itself; paint the title menu first to keep it visible behind.
3. Await `next_frame`.

Make a new `SoundPanel` from the mixer's current volumes each time it
opens, apply each change to the mixer at once (`set_master_volume`,
`set_bus_volume`), and save `AudioPreferences` once, when it closes. Make a
new `AchievementsPanel` each time it opens, so it starts at the top. Call
`TitleMenu::reset` when returning to the title menu.

**After a screen changes, end the frame.** Hosts hand back the same input
until the next frame is awaited, so a game that switches screens and runs
the new one in the same pass shows it the press that opened it. The games
await `FrameDemand::after(Duration::ZERO)` after every change, including
the change from the title menu to play: in sng-zhoenus the Space that
chose Start would otherwise pause the game at once.

The three games' glue is short and worth copying: sng-mahjong `src/menu.rs`
(title only), sng-zhoenus `src/menu.rs` (title and Sound), sng-roguelite
`run_menu_frame` in `crates/game-app/src/lib.rs` (all three, and panels
opened from its run-summary screen as well).

## Input

Every screen takes mouse, touch, keyboard and gamepad:

- **Pointer:** a button acts on press then release over it, so a finger
  that slides off cancels. Sliders follow a drag. A finger drag scrolls the
  achievements list 1:1.
- **Keys:** arrows move focus (and adjust a focused slider by 5% of its
  range); Enter or Space confirms; Escape goes back. On the Sound panel
  Space also closes, as sng-roguelite's did.
- **Gamepad:** d-pad or left stick move focus, with `NavRepeat`'s
  hold-to-repeat; South confirms; East goes back.
- **Wheel:** a notched wheel glides a row per notch; a trackpad's precise
  pixel delta scrolls 1:1 at once.

A screen starts **disarmed**: keys and pad buttons do nothing until it has
seen them all released and the stick centred. The press that opened a
screen, still held on its first frame, cannot also act on it. Pointer
input needs no arming, because a button acts only on a release that
followed a press on it.

The highlighted control follows the input in use: the focused one after
keys or a pad, the one under the pointer after the mouse moves or a finger
lands. A pressed control is always highlighted. The title menu opens with
its default entry highlighted, so Space or Enter starting at once is
visible.

Escape on the title menu is left to the game: the games quit on it on
desktop. Phones have no Quit entry; the platform closes apps.

## Layout

Everything is laid out inside the safe area (`HostFrame::insets`), so
nothing sits under a notch or system bar. Title menu buttons are at most
340 points wide and 44 to 56 tall. On a short screen (a phone held
sideways) the title and gaps shrink so four buttons still fit; a test
checks a 390 x 844 phone both ways up with notch insets, and a desktop
window. Panels are centred, at most 560 (Sound) or 760 x 560
(Achievements) points, with a 40-point close button in the top-right
corner.

## Frame demand

The screens are static unless something moves. `is_animating()` is true
while a scroll glides or a stick is held (repeats are due);
`menu_frame_demand(animating, needs_ticks)` then asks for 16 ms frames.
Otherwise it returns `IdleUntil(100 ms)` when the game needs ticks (music
to advance, a gamepad to poll: hosts do not wake on pad input) or `Idle`.
Input always wakes the menu at once. sng-mahjong's menu, with no music and
no pad, idles at 0% CPU on macOS.

## History

sng-roguelite had its own title screen ("Press Space / Tap to start") and
its own Sound and Achievements screens, reachable only from its run-summary
screen. A title-screen Achievements button was tried in August 2026 and
removed: that screen started on a tap anywhere, so the first touch left it
before the button could be used. The menu has no tap-anywhere gesture.
The Sound and Achievements code moved here from sng-roguelite, with its
input adapters, release-before-arm rule and scroll behaviour.
