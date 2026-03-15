use crate::{component::Component, geometry::Rect, paint::PaintOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapModel {
    pub bounds: Rect,
    pub image_key: String,
}

impl BitmapModel {
    pub fn new(image_key: impl Into<String>, bounds: Rect) -> Self {
        Self {
            bounds,
            image_key: image_key.into(),
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn set_image_key(&mut self, image_key: impl Into<String>) {
        self.image_key = image_key.into();
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::BlitImage {
            rect: self.bounds,
            image_key: self.image_key.clone(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub id: i32,
    pub model: BitmapModel,
}

impl Bitmap {
    pub fn new(id: i32, image_key: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            model: BitmapModel::new(image_key, bounds),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.model.paint(scene);
    }
}

impl Component for Bitmap {
    fn bounds(&self) -> Rect {
        self.model.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.model.set_bounds(rect);
    }

    fn id(&self) -> i32 {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Bitmap, BitmapModel};
    use crate::{component::Component, geometry::Rect, paint::PaintOp};

    #[test]
    fn bitmap_model_paints_blit_image_op() {
        let model = BitmapModel::new(
            "scene/title.png",
            Rect {
                x: 10,
                y: 20,
                width: 200,
                height: 120,
            },
        );

        let mut ops = Vec::new();
        model.paint(&mut ops);

        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PaintOp::BlitImage { image_key, .. } if image_key == "scene/title.png"
        ));
    }

    #[test]
    fn bitmap_component_updates_bounds() {
        let mut bitmap = Bitmap::new(
            4,
            "scene/title.png",
            Rect {
                x: 0,
                y: 0,
                width: 32,
                height: 32,
            },
        );

        bitmap.set_bounds(Rect {
            x: 5,
            y: 6,
            width: 64,
            height: 48,
        });

        assert_eq!(
            bitmap.bounds(),
            Rect {
                x: 5,
                y: 6,
                width: 64,
                height: 48,
            }
        );
    }

    #[test]
    fn bitmap_model_updates_key_and_bounds_in_paint_output() {
        let mut model = BitmapModel::new(
            "scene/title.png",
            Rect {
                x: 10,
                y: 20,
                width: 200,
                height: 120,
            },
        );
        model.set_image_key("scene/updated.png");
        model.set_bounds(Rect {
            x: 30,
            y: 40,
            width: 300,
            height: 180,
        });

        let mut ops = Vec::new();
        model.paint(&mut ops);

        assert_eq!(
            ops,
            vec![PaintOp::BlitImage {
                rect: Rect {
                    x: 30,
                    y: 40,
                    width: 300,
                    height: 180,
                },
                image_key: "scene/updated.png".to_string(),
            }]
        );
    }

    #[test]
    fn bitmap_component_paint_delegates_to_model() {
        let bitmap = Bitmap::new(
            9,
            "scene/logo.png",
            Rect {
                x: 2,
                y: 4,
                width: 16,
                height: 18,
            },
        );

        let mut ops = Vec::new();
        bitmap.paint(&mut ops);

        assert_eq!(
            ops,
            vec![PaintOp::BlitImage {
                rect: Rect {
                    x: 2,
                    y: 4,
                    width: 16,
                    height: 18,
                },
                image_key: "scene/logo.png".to_string(),
            }]
        );
    }
}
