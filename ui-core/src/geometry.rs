use serde::{Deserialize, Serialize};

pub type Scalar = f32;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: Scalar,
    pub y: Scalar,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Size {
    pub width: Scalar,
    pub height: Scalar,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: Scalar,
    pub y: Scalar,
    pub width: Scalar,
    pub height: Scalar,
}

impl Rect {
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    pub fn right(self) -> Scalar {
        self.x + self.width
    }

    pub fn bottom(self) -> Scalar {
        self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Insets {
    pub left: Scalar,
    pub top: Scalar,
    pub right: Scalar,
    pub bottom: Scalar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[cfg(test)]
mod tests {
    use super::{Point, Rect};

    #[test]
    fn rect_contains_uses_half_open_edges() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        };

        assert!(rect.contains(Point { x: 10.0, y: 20.0 }));
        assert!(rect.contains(Point { x: 39.0, y: 59.0 }));
        assert!(!rect.contains(Point { x: 40.0, y: 59.0 }));
        assert!(!rect.contains(Point { x: 39.0, y: 60.0 }));
    }

    #[test]
    fn rect_right_and_bottom_use_logical_extents() {
        let rect = Rect {
            x: 12.5,
            y: 7.25,
            width: 33.5,
            height: 18.75,
        };

        assert_eq!(rect.right(), 46.0);
        assert_eq!(rect.bottom(), 26.0);
    }

    #[test]
    fn rect_contains_fractional_points_without_integer_rounding() {
        let rect = Rect {
            x: 0.25,
            y: 0.5,
            width: 10.5,
            height: 4.25,
        };

        assert!(rect.contains(Point { x: 0.25, y: 0.5 }));
        assert!(rect.contains(Point { x: 10.74, y: 4.74 }));
        assert!(!rect.contains(Point { x: 10.75, y: 4.74 }));
        assert!(!rect.contains(Point { x: 10.74, y: 4.75 }));
    }
}
