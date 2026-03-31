use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Bounds, Pixels, Point};

use crate::workspace::layout::SplitDirection;

/// State captured at the start of a divider drag.
pub struct DragState {
    pub project_id: String,
    pub split_path: Vec<usize>,
    pub left_index: usize,
    pub right_index: usize,
    pub direction: SplitDirection,
    pub container_bounds: Bounds<Pixels>,
    pub initial_mouse: Point<Pixels>,
    pub initial_sizes: Vec<f32>,
}

/// Shared drag state for the active resize operation.
pub type ActiveDrag = Rc<RefCell<Option<DragState>>>;

/// Compute new sizes based on mouse position during drag.
/// Returns the full updated sizes vector, or None if computation fails.
pub fn compute_resize(drag: &DragState, mouse_pos: Point<Pixels>) -> Option<Vec<f32>> {
    let container_size_px: f32 = match drag.direction {
        SplitDirection::Horizontal => f32::from(drag.container_bounds.size.height),
        SplitDirection::Vertical => f32::from(drag.container_bounds.size.width),
    };

    if container_size_px <= 0.0 {
        return None;
    }

    let delta_px: f32 = match drag.direction {
        SplitDirection::Horizontal => f32::from(mouse_pos.y) - f32::from(drag.initial_mouse.y),
        SplitDirection::Vertical => f32::from(mouse_pos.x) - f32::from(drag.initial_mouse.x),
    };

    let total_sizes: f32 = drag.initial_sizes.iter().sum();
    if total_sizes <= 0.0 {
        return None;
    }

    let delta_pct = delta_px / container_size_px * total_sizes;

    let combined = drag.initial_sizes[drag.left_index] + drag.initial_sizes[drag.right_index];
    let min_size = total_sizes * 0.05;

    let left_new =
        (drag.initial_sizes[drag.left_index] + delta_pct).clamp(min_size, combined - min_size);
    let right_new = combined - left_new;

    let mut new_sizes = drag.initial_sizes.clone();
    new_sizes[drag.left_index] = left_new;
    new_sizes[drag.right_index] = right_new;

    Some(new_sizes)
}

#[cfg(test)]
mod tests {
    // Explicit imports to avoid pulling in GPUI types that blow the
    // gpui_macros proc-macro stack during test compilation.
    use super::{compute_resize, DragState, SplitDirection};
    use gpui::{point, px, size, Bounds};

    fn make_drag(
        direction: SplitDirection,
        container_w: f32,
        container_h: f32,
        mouse_x: f32,
        mouse_y: f32,
        sizes: Vec<f32>,
    ) -> DragState {
        DragState {
            project_id: "p".into(),
            split_path: vec![],
            left_index: 0,
            right_index: 1,
            direction,
            container_bounds: Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(container_w), px(container_h)),
            ),
            initial_mouse: point(px(mouse_x), px(mouse_y)),
            initial_sizes: sizes,
        }
    }

    #[test]
    fn test_normal_drag_preserves_total() {
        let drag = make_drag(
            SplitDirection::Vertical,
            400.0,
            300.0,
            200.0,
            150.0,
            vec![50.0, 50.0],
        );
        let result = compute_resize(&drag, point(px(250.0), px(150.0))).unwrap();
        let total_before: f32 = drag.initial_sizes.iter().sum();
        let total_after: f32 = result.iter().sum();
        assert!((total_before - total_after).abs() < 0.001);
    }

    #[test]
    fn test_min_size_clamping() {
        let drag = make_drag(
            SplitDirection::Vertical,
            400.0,
            300.0,
            200.0,
            150.0,
            vec![50.0, 50.0],
        );
        // Drag far right. Should clamp at 5%.
        let result = compute_resize(&drag, point(px(600.0), px(150.0))).unwrap();
        let total: f32 = result.iter().sum();
        let min = total * 0.05;
        assert!(result[1] >= min - 0.001);
    }

    #[test]
    fn test_zero_container_returns_none() {
        let drag = make_drag(
            SplitDirection::Vertical,
            0.0,
            0.0,
            0.0,
            0.0,
            vec![50.0, 50.0],
        );
        assert!(compute_resize(&drag, point(px(10.0), px(0.0))).is_none());
    }

    #[test]
    fn test_horizontal_direction_uses_y() {
        let drag = make_drag(
            SplitDirection::Horizontal,
            400.0,
            300.0,
            200.0,
            100.0,
            vec![50.0, 50.0],
        );
        // Move mouse down by 30px in a 300px container with total_sizes=100
        let result = compute_resize(&drag, point(px(200.0), px(130.0))).unwrap();
        // delta_pct = 30/300 * 100 = 10
        assert!((result[0] - 60.0).abs() < 0.01);
        assert!((result[1] - 40.0).abs() < 0.01);
    }
}
