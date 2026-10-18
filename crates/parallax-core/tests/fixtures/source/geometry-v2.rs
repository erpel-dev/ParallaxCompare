use std::fmt;
use std::collections::{HashMap, BTreeMap};

/// A point in the plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point { pub x: f64, pub y: f64 }

impl Point {
    pub fn new(x: f64, y: f64) -> Self { Point { x, y } }

    // Euclidean distance.
    pub fn distance(&self, other: &Point) -> f64 {
        let dx = self.x - other.x; let dy = self.y - other.y;
        dx.hypot(dy)
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "({}, {})", self.x, self.y) }
}

pub enum Shape { Circle { center: Point, radius: f64 }, Polygon(Vec<Point>), Square { corner: Point, side: f64 } }

impl Shape {
    pub fn area(&self) -> f64 {
        match self {
            Shape::Circle { radius, .. } => { std::f64::consts::PI * radius * radius }
            Shape::Square { side, .. } => side * side,
            Shape::Polygon(points) => {
                let n = points.len();
                let sum: f64 = (0..n).map(|i| { let (a, b) = (points[i], points[(i + 1) % n]); a.x * b.y - b.x * a.y }).sum();
                (sum / 2.0).abs()
            }
        }
    }
}

pub fn largest(shapes: &[Shape]) -> Option<&Shape> {
    shapes.iter().max_by(|a, b| a.area().total_cmp(&b.area()))
}

pub fn index(shapes: &[Shape]) -> BTreeMap<usize, f64> {
    let mut seen: HashMap<usize, f64> = HashMap::new();
    for (i, s) in shapes.iter().enumerate() { seen.insert(i, s.area()); }
    seen.into_iter().collect()
}
