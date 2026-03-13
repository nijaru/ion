/// A position in the terminal grid (zero-indexed, col/row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub x: u16,
    pub y: u16,
}

impl Position {
    pub fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// A rectangular region of the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(&self, pos: Position) -> bool {
        pos.x >= self.x
            && pos.x < self.x.saturating_add(self.width)
            && pos.y >= self.y
            && pos.y < self.y.saturating_add(self.height)
    }

    pub fn intersection(&self, other: Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self
            .x
            .saturating_add(self.width)
            .min(other.x.saturating_add(other.width));
        let bottom = self
            .y
            .saturating_add(self.height)
            .min(other.y.saturating_add(other.height));
        if x < right && y < bottom {
            Some(Rect::new(x, y, right - x, bottom - y))
        } else {
            None
        }
    }

    /// Shrink by `margin` on all sides.
    pub fn inner(&self, margin: u16) -> Rect {
        let double = margin.saturating_mul(2);
        if self.width <= double || self.height <= double {
            return Rect::default();
        }
        Rect::new(
            self.x.saturating_add(margin),
            self.y.saturating_add(margin),
            self.width - double,
            self.height - double,
        )
    }

    /// Clamp to fit within `bounds`.
    pub fn clamp(&self, bounds: Rect) -> Rect {
        let x = self.x.max(bounds.x);
        let y = self.y.max(bounds.y);
        let right = self
            .x
            .saturating_add(self.width)
            .min(bounds.x.saturating_add(bounds.width));
        let bottom = self
            .y
            .saturating_add(self.height)
            .min(bounds.y.saturating_add(bounds.height));
        Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }
}

/// A terminal size (width × height in cells).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl Size {
    pub fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_area() {
        assert_eq!(Rect::new(0, 0, 5, 3).area(), 15);
        assert_eq!(Rect::new(0, 0, 0, 3).area(), 0);
    }

    #[test]
    fn rect_is_empty() {
        assert!(Rect::new(0, 0, 0, 5).is_empty());
        assert!(Rect::new(0, 0, 5, 0).is_empty());
        assert!(!Rect::new(0, 0, 1, 1).is_empty());
    }

    #[test]
    fn rect_contains() {
        let r = Rect::new(1, 1, 4, 4);
        assert!(r.contains(Position::new(1, 1)));
        assert!(r.contains(Position::new(4, 4)));
        assert!(!r.contains(Position::new(0, 0)));
        assert!(!r.contains(Position::new(5, 5)));
    }

    #[test]
    fn rect_intersection() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(3, 3, 5, 5);
        assert_eq!(a.intersection(b), Some(Rect::new(3, 3, 2, 2)));
    }

    #[test]
    fn rect_no_intersection() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(5, 0, 5, 5); // touching but not overlapping
        assert!(a.intersection(b).is_none());
    }

    #[test]
    fn rect_inner() {
        let r = Rect::new(0, 0, 10, 6);
        assert_eq!(r.inner(1), Rect::new(1, 1, 8, 4));
        assert_eq!(r.inner(0), r);
        // margin too large → empty
        assert_eq!(r.inner(5), Rect::default());
    }

    #[test]
    fn rect_clamp() {
        let bounds = Rect::new(0, 0, 80, 24);
        // Partially outside → clamp
        let r = Rect::new(75, 20, 10, 10);
        let c = r.clamp(bounds);
        assert_eq!(c.x, 75);
        assert_eq!(c.y, 20);
        assert_eq!(c.width, 5);
        assert_eq!(c.height, 4);
    }
}
