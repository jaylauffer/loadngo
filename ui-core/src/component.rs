use std::any::Any;

use crate::geometry::{Point, Rect};

/// Platform-agnostic widget contract.
pub trait Component: Any {
    fn bounds(&self) -> Rect;
    fn set_bounds(&mut self, rect: Rect);

    fn focus_changed(&mut self, _gained: bool) {}
    fn mouse_entered(&mut self) {}
    fn mouse_exited(&mut self) {}

    fn drag_over(&mut self, _pt: Point) -> bool {
        false
    }

    fn drop_files(&mut self, _files: &[String], _pt: Point) -> bool {
        false
    }

    fn id(&self) -> i32 {
        0
    }

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn hit_test(&self, pt: Point) -> bool {
        self.bounds().contains(pt)
    }
}
