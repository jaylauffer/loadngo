//! Directional focus navigation across a set of widgets.
//!
//! Every focusable model in this crate (`ButtonModel`, `SliderModel`,
//! `CheckboxModel`, `StepperModel`, `TextAreaModel`) already carries a
//! `focused: bool` driven by [`UiEvent::FocusChanged`], and already paints
//! that state distinctly. What was missing was anything that decides *which*
//! of several widgets holds focus, and moves it when a player pushes a
//! direction — until now every caller hand-rolled that per screen.
//!
//! [`FocusRing`] is that missing piece, and deliberately owns nothing else:
//! it stores an ordered set of candidate rects, tracks which one is focused,
//! and answers "given a direction, who loses focus and who gains it?" The
//! caller still owns the widgets themselves and emits the resulting
//! `FocusChanged` events, so this composes with existing widgets without
//! changing a single one of them.
//!
//! Deliberately input-source agnostic: it speaks [`NavDirection`], not
//! gamepad buttons or key codes. Translating a d-pad, a thumbstick, or arrow
//! keys into a direction (including hold-to-repeat timing) belongs a layer
//! up, in `loadngo-touch`, which is where the host's `InputSnapshot` is
//! visible — this crate sits below that and stays pure.

use crate::geometry::Rect;
use crate::widget::WidgetId;

/// A direction a player pushed, already normalized away from whichever
/// device produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDirection {
    Up,
    Down,
    Left,
    Right,
}

/// One navigable widget: its identity, and where it currently sits on
/// screen. Bounds drive *spatial* navigation (see [`FocusRing::navigate`]),
/// so they should be refreshed whenever layout changes — the same
/// per-frame `set_bounds` discipline callers already follow for widgets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusEntry {
    pub id: WidgetId,
    pub bounds: Rect,
}

impl FocusEntry {
    #[must_use]
    pub const fn new(id: WidgetId, bounds: Rect) -> Self {
        Self { id, bounds }
    }

    fn center(&self) -> (f32, f32) {
        (
            self.bounds.x + self.bounds.width * 0.5,
            self.bounds.y + self.bounds.height * 0.5,
        )
    }
}

/// The result of a focus move: who lost it, who gained it. Callers turn
/// this into `UiEvent::FocusChanged(false)` / `FocusChanged(true)` on the
/// two widgets involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusMove {
    pub lost: Option<WidgetId>,
    pub gained: WidgetId,
}

/// Tracks which of a set of widgets holds focus, and moves that focus in
/// response to directional input.
///
/// Navigation is **spatial**, not declaration-ordered: pushing right moves
/// to the nearest entry actually to the right on screen. A row of buttons
/// therefore behaves correctly no matter what order the caller registered
/// them in, and a grid works without the caller describing its shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FocusRing {
    entries: Vec<FocusEntry>,
    focused: Option<usize>,
    armed: bool,
}

impl FocusRing {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the navigable set, preserving which widget was focused
    /// (by id) when it's still present. Safe to call every frame with
    /// freshly laid-out rects.
    pub fn set_entries(&mut self, entries: Vec<FocusEntry>) {
        let focused_id = self.focused_id();
        self.entries = entries;
        self.focused = focused_id.and_then(|id| self.index_of(id));
    }

    #[must_use]
    pub fn entries(&self) -> &[FocusEntry] {
        &self.entries
    }

    #[must_use]
    pub fn focused_id(&self) -> Option<WidgetId> {
        self.focused.and_then(|index| self.entries.get(index)).map(|entry| entry.id)
    }

    #[must_use]
    pub fn is_focused(&self, id: WidgetId) -> bool {
        self.focused_id() == Some(id)
    }

    fn index_of(&self, id: WidgetId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }

    /// Whether directional/confirm input is currently accepted. A ring
    /// starts **disarmed**: a screen that appears while the player still
    /// holds a button from the previous screen must not act on that stale
    /// press. Callers arm it once they observe the relevant controls
    /// released — the same release-before-arm discipline `sng-roguelite`
    /// proved per-screen before this type existed.
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// Arms the ring, focusing the first entry if nothing is focused yet so
    /// the player always has a visible starting selection.
    pub fn arm(&mut self) -> Option<FocusMove> {
        self.armed = true;
        if self.focused.is_none() {
            return self.focus_first();
        }
        None
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Focuses the first entry outright, regardless of arming.
    pub fn focus_first(&mut self) -> Option<FocusMove> {
        let gained = self.entries.first()?.id;
        let lost = self.focused_id().filter(|id| *id != gained);
        self.focused = Some(0);
        Some(FocusMove { lost, gained })
    }

    /// Focuses a specific widget by id — for a screen that wants to open
    /// with a particular default selection rather than its first entry.
    /// Returns `None` if `id` isn't in the ring, or already had focus.
    pub fn focus(&mut self, id: WidgetId) -> Option<FocusMove> {
        let index = self.index_of(id)?;
        let lost = self.focused_id();
        if lost == Some(id) {
            return None;
        }
        self.focused = Some(index);
        Some(FocusMove { lost, gained: id })
    }

    pub fn clear_focus(&mut self) -> Option<WidgetId> {
        let lost = self.focused_id();
        self.focused = None;
        lost
    }

    /// Moves focus one step in `direction`, returning who lost and gained it.
    ///
    /// Returns `None` (no move) when the ring is disarmed, empty, or there
    /// is no entry in that direction — focus deliberately does **not** wrap
    /// around, so holding a direction settles at the edge instead of
    /// cycling endlessly past it.
    ///
    /// With nothing focused yet, any direction focuses the first entry, so
    /// a player's first input always produces a visible selection.
    pub fn navigate(&mut self, direction: NavDirection) -> Option<FocusMove> {
        if !self.armed || self.entries.is_empty() {
            return None;
        }
        let Some(current_index) = self.focused else {
            return self.focus_first();
        };
        let lost = self.entries.get(current_index)?.id;
        let best = self.nearest_in_direction(current_index, direction)?;
        let gained = self.entries.get(best)?.id;
        self.focused = Some(best);
        Some(FocusMove {
            lost: Some(lost),
            gained,
        })
    }

    /// Nearest entry whose center lies in `direction` from the entry at
    /// `from_index`. Distance along the travel axis dominates; off-axis
    /// drift is a tie-breaker, so a grid steps to the neighbour a player
    /// expects rather than to whatever happens to be closest as the crow
    /// flies.
    fn nearest_in_direction(&self, from_index: usize, direction: NavDirection) -> Option<usize> {
        let (from_x, from_y) = self.entries.get(from_index)?.center();
        self.entries
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != from_index)
            .filter_map(|(index, entry)| {
                let (x, y) = entry.center();
                let (along, across) = match direction {
                    NavDirection::Left => (from_x - x, (y - from_y).abs()),
                    NavDirection::Right => (x - from_x, (y - from_y).abs()),
                    NavDirection::Up => (from_y - y, (x - from_x).abs()),
                    NavDirection::Down => (y - from_y, (x - from_x).abs()),
                };
                (along > f32::EPSILON).then_some((index, along, across))
            })
            .min_by(|a, b| {
                let cost_a = a.1 + a.2 * 2.0;
                let cost_b = b.1 + b.2 * 2.0;
                cost_a.partial_cmp(&cost_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::{FocusEntry, FocusRing, NavDirection};
    use crate::geometry::Rect;
    use crate::widget::WidgetId;

    fn rect(x: f32, y: f32) -> Rect {
        Rect {
            x,
            y,
            width: 100.0,
            height: 40.0,
        }
    }

    /// Three buttons in a row, deliberately registered out of visual order
    /// to prove navigation is spatial rather than declaration-ordered.
    fn shuffled_row() -> FocusRing {
        let mut ring = FocusRing::new();
        ring.set_entries(vec![
            FocusEntry::new(WidgetId(2), rect(240.0, 0.0)),
            FocusEntry::new(WidgetId(0), rect(0.0, 0.0)),
            FocusEntry::new(WidgetId(1), rect(120.0, 0.0)),
        ]);
        ring
    }

    #[test]
    fn a_disarmed_ring_ignores_navigation() {
        let mut ring = shuffled_row();
        assert!(!ring.is_armed());
        assert_eq!(ring.navigate(NavDirection::Right), None);
        assert_eq!(ring.focused_id(), None);
    }

    #[test]
    fn arming_focuses_the_first_entry() {
        let mut ring = shuffled_row();
        let moved = ring.arm().expect("arming focuses something");
        assert_eq!(moved.lost, None);
        assert_eq!(moved.gained, WidgetId(2));
        assert_eq!(ring.focused_id(), Some(WidgetId(2)));
    }

    #[test]
    fn navigation_follows_screen_position_not_registration_order() {
        let mut ring = shuffled_row();
        ring.set_entries(vec![
            FocusEntry::new(WidgetId(2), rect(240.0, 0.0)),
            FocusEntry::new(WidgetId(0), rect(0.0, 0.0)),
            FocusEntry::new(WidgetId(1), rect(120.0, 0.0)),
        ]);
        ring.arm();
        // Start at the leftmost button regardless of where it was declared.
        ring.focus(WidgetId(0));

        let moved = ring.navigate(NavDirection::Right).expect("moves right");
        assert_eq!(moved.lost, Some(WidgetId(0)));
        assert_eq!(moved.gained, WidgetId(1));

        let moved = ring.navigate(NavDirection::Right).expect("moves right again");
        assert_eq!(moved.gained, WidgetId(2));
    }

    #[test]
    fn focus_stops_at_the_edge_instead_of_wrapping() {
        let mut ring = shuffled_row();
        ring.arm();
        ring.focus(WidgetId(2));
        assert_eq!(ring.navigate(NavDirection::Right), None);
        assert_eq!(ring.focused_id(), Some(WidgetId(2)));
    }

    #[test]
    fn a_grid_steps_to_the_neighbour_directly_across() {
        let mut ring = FocusRing::new();
        ring.set_entries(vec![
            FocusEntry::new(WidgetId(0), rect(0.0, 0.0)),
            FocusEntry::new(WidgetId(1), rect(120.0, 0.0)),
            FocusEntry::new(WidgetId(2), rect(0.0, 80.0)),
            FocusEntry::new(WidgetId(3), rect(120.0, 80.0)),
        ]);
        ring.arm();
        ring.focus(WidgetId(0));

        assert_eq!(
            ring.navigate(NavDirection::Down).map(|m| m.gained),
            Some(WidgetId(2))
        );
        assert_eq!(
            ring.navigate(NavDirection::Right).map(|m| m.gained),
            Some(WidgetId(3))
        );
        assert_eq!(
            ring.navigate(NavDirection::Up).map(|m| m.gained),
            Some(WidgetId(1))
        );
    }

    #[test]
    fn relayout_preserves_the_focused_widget_by_id() {
        let mut ring = shuffled_row();
        ring.arm();
        ring.focus(WidgetId(1));
        // Same widgets, new positions (e.g. a window resize).
        ring.set_entries(vec![
            FocusEntry::new(WidgetId(0), rect(0.0, 200.0)),
            FocusEntry::new(WidgetId(1), rect(120.0, 200.0)),
            FocusEntry::new(WidgetId(2), rect(240.0, 200.0)),
        ]);
        assert_eq!(ring.focused_id(), Some(WidgetId(1)));
    }

    #[test]
    fn focus_is_dropped_when_its_widget_disappears() {
        let mut ring = shuffled_row();
        ring.arm();
        ring.focus(WidgetId(1));
        ring.set_entries(vec![FocusEntry::new(WidgetId(0), rect(0.0, 0.0))]);
        assert_eq!(ring.focused_id(), None);
    }

    #[test]
    fn first_direction_with_nothing_focused_selects_something_visible() {
        let mut ring = shuffled_row();
        ring.arm();
        ring.clear_focus();
        let moved = ring.navigate(NavDirection::Left).expect("selects an entry");
        assert_eq!(moved.gained, WidgetId(2));
    }
}
