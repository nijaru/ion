use std::collections::HashMap;

use taffy::{
    AvailableSpace, NodeId, TaffyTree,
    geometry::{Rect as TaffyRect, Size as TaffySize},
    style::{
        AlignItems, AlignSelf, Dimension as TaffyDimension, Display as TaffyDisplay, FlexDirection,
        JustifyContent, LengthPercentage, LengthPercentageAuto, Style,
    },
};

use crate::{
    geometry::{Rect, Size},
    widgets::{Element, WidgetId},
};

// ── Layout style types ────────────────────────────────────────────────────────

/// Flex layout direction.
#[derive(Debug, Clone, Copy, Default)]
pub enum Direction {
    #[default]
    Row,
    Column,
    RowReverse,
    ColumnReverse,
}

/// A sizing value for layout constraints.
#[derive(Debug, Clone, Copy, Default)]
pub enum Dimension {
    /// Let the layout engine decide (content-sized or flex-driven).
    #[default]
    Auto,
    /// Fixed number of terminal cells.
    Cells(u16),
    /// Percentage of the parent container (0.0–100.0).
    Percent(f32),
}

/// A size expressed in two dimensions.
#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutSize {
    pub width: Dimension,
    pub height: Dimension,
}

impl LayoutSize {
    pub fn cells(width: u16, height: u16) -> Self {
        Self {
            width: Dimension::Cells(width),
            height: Dimension::Cells(height),
        }
    }
}

/// Cross-axis alignment (align-items / align-self in CSS).
#[derive(Debug, Clone, Copy, Default)]
pub enum Align {
    #[default]
    Stretch,
    Start,
    Center,
    End,
    Baseline,
}

/// Main-axis alignment (justify-content in CSS).
#[derive(Debug, Clone, Copy, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Per-side cell counts for padding or margin.
#[derive(Debug, Clone, Copy, Default)]
pub struct Edges {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Edges {
    pub fn all(v: u16) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    pub fn horizontal(h: u16) -> Self {
        Self {
            top: 0,
            right: h,
            bottom: 0,
            left: h,
        }
    }

    pub fn vertical(v: u16) -> Self {
        Self {
            top: v,
            right: 0,
            bottom: v,
            left: 0,
        }
    }

    pub fn symmetric(h: u16, v: u16) -> Self {
        Self {
            top: v,
            right: h,
            bottom: v,
            left: h,
        }
    }
}

/// Layout constraints for a widget node.
///
/// These map directly to CSS Flexbox properties. Taffy computes the actual
/// pixel (cell) positions from these constraints plus the tree hierarchy.
#[derive(Debug, Clone)]
pub struct LayoutStyle {
    pub direction: Direction,
    /// Fixed or percentage size. `Auto` = size by content or flex.
    pub size: LayoutSize,
    pub min_size: LayoutSize,
    pub max_size: LayoutSize,
    /// How much this item should grow relative to siblings. Default `1.0`.
    pub flex_grow: f32,
    /// How much this item should shrink relative to siblings. Default `1.0`.
    pub flex_shrink: f32,
    pub align_self: Option<Align>,
    pub align_items: Option<Align>,
    pub justify_content: Option<Justify>,
    /// Column and row gap between children, in cells.
    pub gap: (u16, u16),
    /// Inner spacing that pushes children away from the edges.
    pub padding: Edges,
    /// Outer spacing that separates this element from siblings.
    pub margin: Edges,
}

impl LayoutStyle {
    /// Override default (flex_grow=1) to create a fixed-size style.
    pub fn fixed(width: u16, height: u16) -> Self {
        Self {
            size: LayoutSize::cells(width, height),
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Default::default()
        }
    }

    /// Column flex container style.
    pub fn column() -> Self {
        Self {
            direction: Direction::Column,
            ..Default::default()
        }
    }
}

// Use flex_grow=1 as the default so elements fill their container.
// This is overridden at the root level (forced to terminal size) and can
// be overridden per-element via Element builder methods.
impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            direction: Direction::Row,
            size: LayoutSize::default(),
            min_size: LayoutSize::default(),
            max_size: LayoutSize::default(),
            flex_grow: 1.0,
            flex_shrink: 1.0,
            align_self: None,
            align_items: None,
            justify_content: None,
            gap: (0, 0),
            padding: Edges::all(0),
            margin: Edges::all(0),
        }
    }
}

// ── Layout result ─────────────────────────────────────────────────────────────

/// The output of a layout pass: maps each WidgetId to its computed screen Rect.
pub struct Layout {
    rects: HashMap<WidgetId, Rect>,
}

impl Layout {
    /// Get the computed rect for a widget. Returns `Rect::default()` (0x0 at
    /// origin) if the widget was not part of the layout tree.
    pub fn get(&self, id: WidgetId) -> Rect {
        self.rects.get(&id).copied().unwrap_or_default()
    }
}

// ── Layout computation ────────────────────────────────────────────────────────

/// Run a Taffy flexbox layout pass over an element tree.
///
/// The root element is forced to `available` size; all other elements use
/// their [`LayoutStyle`] constraints. Returns a mapping of widget IDs to
/// their computed terminal [`Rect`].
pub fn compute_layout(root: &Element, available: Size) -> Layout {
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let mut id_map: HashMap<NodeId, WidgetId> = HashMap::new();

    // Root always fills the terminal — override any user-set size.
    let mut root_style = to_taffy_style(&root.layout_style);
    root_style.size = TaffySize {
        width: TaffyDimension::length(available.width as f32),
        height: TaffyDimension::length(available.height as f32),
    };

    let root_node = build_node_with_style(root, root_style, &mut tree, &mut id_map);

    tree.compute_layout(
        root_node,
        TaffySize {
            width: AvailableSpace::Definite(available.width as f32),
            height: AvailableSpace::Definite(available.height as f32),
        },
    )
    .expect("Taffy layout computation failed");

    let mut rects = HashMap::new();
    collect_layout(&tree, root_node, &id_map, 0.0, 0.0, &mut rects);

    Layout { rects }
}

fn build_taffy_node(
    element: &Element,
    tree: &mut TaffyTree<()>,
    id_map: &mut HashMap<NodeId, WidgetId>,
) -> NodeId {
    let style = to_taffy_style(&element.layout_style);
    build_node_with_style(element, style, tree, id_map)
}

fn build_node_with_style(
    element: &Element,
    style: Style,
    tree: &mut TaffyTree<()>,
    id_map: &mut HashMap<NodeId, WidgetId>,
) -> NodeId {
    let children: Vec<NodeId> = element
        .children
        .iter()
        .map(|child| build_taffy_node(child, tree, id_map))
        .collect();

    let node = if children.is_empty() {
        tree.new_leaf(style).expect("Taffy new_leaf failed")
    } else {
        tree.new_with_children(style, &children)
            .expect("Taffy new_with_children failed")
    };

    id_map.insert(node, element.id);
    node
}

fn collect_layout(
    tree: &TaffyTree<()>,
    node: NodeId,
    id_map: &HashMap<NodeId, WidgetId>,
    parent_x: f32,
    parent_y: f32,
    rects: &mut HashMap<WidgetId, Rect>,
) {
    let tl = tree.layout(node).expect("Taffy layout() failed");
    let abs_x = parent_x + tl.location.x;
    let abs_y = parent_y + tl.location.y;

    if let Some(&widget_id) = id_map.get(&node) {
        rects.insert(
            widget_id,
            Rect::new(
                abs_x.round() as u16,
                abs_y.round() as u16,
                tl.size.width.round() as u16,
                tl.size.height.round() as u16,
            ),
        );
    }

    let children = tree.children(node).expect("Taffy children() failed");
    for child in children {
        collect_layout(tree, child, id_map, abs_x, abs_y, rects);
    }
}

// ── Type conversions ──────────────────────────────────────────────────────────

fn to_taffy_style(s: &LayoutStyle) -> Style {
    Style {
        display: TaffyDisplay::Flex,
        flex_direction: to_flex_dir(s.direction),
        size: TaffySize {
            width: to_dim(s.size.width),
            height: to_dim(s.size.height),
        },
        min_size: TaffySize {
            width: to_dim(s.min_size.width),
            height: to_dim(s.min_size.height),
        },
        max_size: TaffySize {
            width: to_dim(s.max_size.width),
            height: to_dim(s.max_size.height),
        },
        flex_grow: s.flex_grow,
        flex_shrink: s.flex_shrink,
        align_items: s.align_items.map(to_align_items),
        align_self: s.align_self.map(to_align_self),
        justify_content: s.justify_content.map(to_justify),
        gap: TaffySize {
            width: LengthPercentage::length(s.gap.0 as f32),
            height: LengthPercentage::length(s.gap.1 as f32),
        },
        padding: TaffyRect {
            top: LengthPercentage::length(s.padding.top as f32),
            right: LengthPercentage::length(s.padding.right as f32),
            bottom: LengthPercentage::length(s.padding.bottom as f32),
            left: LengthPercentage::length(s.padding.left as f32),
        },
        margin: TaffyRect {
            top: LengthPercentageAuto::length(s.margin.top as f32),
            right: LengthPercentageAuto::length(s.margin.right as f32),
            bottom: LengthPercentageAuto::length(s.margin.bottom as f32),
            left: LengthPercentageAuto::length(s.margin.left as f32),
        },
        ..Style::default()
    }
}

fn to_dim(d: Dimension) -> TaffyDimension {
    match d {
        Dimension::Auto => TaffyDimension::auto(),
        Dimension::Cells(n) => TaffyDimension::length(n as f32),
        Dimension::Percent(p) => TaffyDimension::percent(p / 100.0),
    }
}

fn to_flex_dir(d: Direction) -> FlexDirection {
    match d {
        Direction::Row => FlexDirection::Row,
        Direction::Column => FlexDirection::Column,
        Direction::RowReverse => FlexDirection::RowReverse,
        Direction::ColumnReverse => FlexDirection::ColumnReverse,
    }
}

fn to_align_items(a: Align) -> AlignItems {
    match a {
        Align::Start => AlignItems::FlexStart,
        Align::Center => AlignItems::Center,
        Align::End => AlignItems::FlexEnd,
        Align::Stretch => AlignItems::Stretch,
        Align::Baseline => AlignItems::Baseline,
    }
}

fn to_align_self(a: Align) -> AlignSelf {
    match a {
        Align::Start => AlignSelf::FlexStart,
        Align::Center => AlignSelf::Center,
        Align::End => AlignSelf::FlexEnd,
        Align::Stretch => AlignSelf::Stretch,
        Align::Baseline => AlignSelf::Baseline,
    }
}

fn to_justify(j: Justify) -> JustifyContent {
    match j {
        Justify::Start => JustifyContent::FlexStart,
        Justify::Center => JustifyContent::Center,
        Justify::End => JustifyContent::FlexEnd,
        Justify::SpaceBetween => JustifyContent::SpaceBetween,
        Justify::SpaceAround => JustifyContent::SpaceAround,
        Justify::SpaceEvenly => JustifyContent::SpaceEvenly,
    }
}
