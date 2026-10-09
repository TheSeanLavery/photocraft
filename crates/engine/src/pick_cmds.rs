//! Move tool Auto-Select: pick the topmost visible, not fully locked layer with pixels at a canvas
//! point (Photoshop's options bar "Auto-Select: Layer | Group", ⌘-click with the Move tool, and the
//! canvas right-click layer list).

use photocraft_doc::{Document, LayerContent, LayerId};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

/// Layers with visible pixels at (x, y), topmost first (hidden layers and hidden groups skipped).
pub fn layers_at(doc: &Document, x: i32, y: i32) -> Vec<LayerId> {
    if !doc.bounds().contains(x, y) {
        return Vec::new();
    }
    let rows = doc.walk();
    let mut out = Vec::new();
    for (path, _, l) in rows.iter().rev() {
        // The layer and every enclosing group must be visible.
        if !(1..path.len()).all(|n| {
            doc.layer_at(&path[..n]).is_some_and(|a| {
                a.visible
                    && a.opacity > 0.0
                    && a.fill_opacity > 0.0
                    && !matches!(&a.content, LayerContent::Group(g) if g.artboard.as_ref().is_some_and(|b| !b.rect.contains(x, y)))
                    && photocraft_compose::mask_alpha_at(doc, a, x, y) > 0.0
            })
        }) {
            continue;
        }
        // An artboard clips its layers to the board (#1531): pixels past its edge aren't shown,
        // so they can't be picked. The board itself is hit anywhere on it, below its layers
        // (the walk lists them first), as a Photoshop click on an empty spot selects the board.
        let board = path.first().and_then(|i| doc.layers.get(*i)).and_then(|t| t.artboard());
        if board.is_some_and(|a| !a.rect.contains(x, y)) {
            continue;
        }
        if l.artboard().is_some() {
            out.push(l.id);
            continue;
        }
        if matches!(l.content, LayerContent::Group(_) | LayerContent::Adjustment(_)) {
            continue;
        }
        if l.visible && l.opacity > 0.0 && l.fill_opacity > 0.0 && photocraft_compose::content_alpha_at(doc, l, x, y) > 0.0 {
            out.push(l.id);
        }
    }
    out
}

/// The outermost group containing `id` (the layer itself when it isn't in a group). An artboard
/// is not a group here: Group mode stops at the outermost group on the board.
fn top_group(doc: &Document, id: LayerId) -> LayerId {
    let Some((path, _, _)) = doc.walk().into_iter().find(|(_, _, l)| l.id == id) else { return id };
    let depth = if path.get(..1).and_then(|p| doc.layer_at(p)).is_some_and(|t| t.artboard().is_some()) { 2 } else { 1 };
    path.get(..depth).and_then(|p| doc.layer_at(p)).map_or(id, |l| l.id)
}

fn pick(s: &mut Session, p: &Value) -> Result<Value> {
    let x = p.get("x").and_then(Value::as_f64).ok_or_else(|| EngineError::BadParams { cmd: "layer.pickAt".into(), msg: "missing `x`".into() })?.floor() as i32;
    let y = p.get("y").and_then(Value::as_f64).ok_or_else(|| EngineError::BadParams { cmd: "layer.pickAt".into(), msg: "missing `y`".into() })?.floor() as i32;
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let hits = layers_at(&doc, x, y);
    if p.get("list").and_then(Value::as_bool).unwrap_or(false) {
        let names: Vec<Value> = hits.iter().filter_map(|id| doc.layer(*id)).map(|l| json!({"layer": l.id.0, "name": l.name})).collect();
        return Ok(json!({ "layers": names }));
    }
    // Like Photoshop, Auto-Select clicks through a fully locked layer (its own Lock All or a
    // locked group's) to the layer under it (#1641). The right-click list above still shows it.
    let Some(&hit) = hits.iter().find(|id| !doc.effective_locks(**id).all) else { return Ok(json!({ "layer": null })) };
    let target = if p.get("target").and_then(Value::as_str) == Some("group") { top_group(&doc, hit) } else { hit };
    if p.get("select").and_then(Value::as_bool).unwrap_or(true) {
        let mode = p.get("mode").and_then(Value::as_str).unwrap_or("replace");
        // Like Photoshop, a plain click on one of several selected layers keeps them all
        // selected, so the drag that follows moves the whole selection.
        let keeps = mode == "replace" && s.active().is_some_and(|st| st.is_layer_selected(target) && st.selected_layers().len() > 1);
        if !keeps {
            s.execute("layer.select", json!({"layer": target.0, "mode": mode}))?;
        }
    }
    Ok(json!({ "layer": target.0 }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "layer.pickAt",
        label: "Auto-Select Layer",
        menu: &[],
        shortcut: None,
        params: r##"{"x":px,"y":px,"target":"layer|group"="layer","select":bool=true,"mode":"replace|toggle|add"="replace","list":bool=false (return every layer with pixels there, topmost first)} → {layer} | {layers}"##,
        enabled: has_doc,
        journal: false,
        run: pick,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{Color, Fill, Layer, comps::Artboard, vector::VectorMask};
    use photocraft_geom::Rect;

    #[test]
    fn picks_topmost_layer_with_pixels() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 40})).unwrap();
        let bg = s.active().unwrap().doc.layers[0].id;
        s.execute("layer.new.layer", json!({"name": "A"})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        let a = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.new.layer", json!({"name": "B"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        // Empty B is skipped; A wins inside its square, the Background elsewhere.
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap()["layer"], a.0);
        assert_eq!(s.active().unwrap().active_layer, Some(a));
        assert_eq!(s.execute("layer.pickAt", json!({"x": 30, "y": 30, "select": false})).unwrap()["layer"], bg.0);
        let list = s.execute("layer.pickAt", json!({"x": 5, "y": 5, "list": true})).unwrap();
        assert_eq!(list["layers"].as_array().unwrap().len(), 2);
        // Hidden layers are ignored.
        s.execute("layer.setProps", json!({"layer": a.0, "visible": false})).unwrap();
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5, "select": false})).unwrap()["layer"], bg.0);
    }

    /// Two filled squares: A at (0..10), B at (20..30), both in their own layers.
    fn two_squares() -> (Session, LayerId, LayerId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 40})).unwrap();
        let mut ids = Vec::new();
        for (name, x) in [("A", 0), ("B", 20)] {
            s.execute("layer.new.layer", json!({"name": name})).unwrap();
            s.execute("select.rect", json!({"x": x, "y": x, "width": 10, "height": 10})).unwrap();
            s.execute("edit.fill", json!({"color": "#00ff00"})).unwrap();
            ids.push(s.active().unwrap().active_layer.unwrap());
        }
        s.execute("select.deselect", json!({})).unwrap();
        (s, ids[0], ids[1])
    }

    #[test]
    fn group_target_selects_the_outermost_group() {
        let (mut s, a, _) = two_squares();
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        let inner = s.execute("layer.new.groupFromLayers", json!({"name": "Inner"})).unwrap()["layer"].as_u64().unwrap();
        let outer = s.execute("layer.new.groupFromLayers", json!({"name": "Outer"})).unwrap()["layer"].as_u64().unwrap();
        assert_ne!(inner, outer);
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5, "target": "group"})).unwrap()["layer"], outer);
        assert_eq!(s.active().unwrap().active_layer, Some(LayerId(outer)));
        // Layer mode reaches through the groups to the pixels.
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5, "target": "layer"})).unwrap()["layer"], a.0);
        // Hiding the outer group hides its layers from the pick.
        s.execute("layer.setProps", json!({"layer": outer, "visible": false})).unwrap();
        let bg = s.active().unwrap().doc.layers[0].id;
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5, "select": false})).unwrap()["layer"], bg.0);
    }

    #[test]
    fn a_click_on_a_selected_layer_keeps_the_multi_selection() {
        let (mut s, a, b) = two_squares();
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        s.execute("layer.select", json!({"layer": b.0, "mode": "add"})).unwrap();
        assert_eq!(s.active().unwrap().selected_layers().len(), 2);
        // Clicking A (selected) keeps both so the drag moves both.
        s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap();
        let st = s.active().unwrap();
        assert!(st.is_layer_selected(a) && st.is_layer_selected(b));
        // Clicking the Background (not selected) replaces the selection.
        s.execute("layer.pickAt", json!({"x": 35, "y": 5})).unwrap();
        assert_eq!(s.active().unwrap().selected_layers().len(), 1);
        assert!(!s.active().unwrap().is_layer_selected(a));
        // Shift-click (add) builds a selection up again.
        s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap();
        s.execute("layer.pickAt", json!({"x": 25, "y": 25, "mode": "add"})).unwrap();
        let st = s.active().unwrap();
        assert!(st.is_layer_selected(a) && st.is_layer_selected(b));
    }

    #[test]
    fn transparent_fill_and_vector_mask_do_not_steal_the_pick() {
        let (mut s, a, _) = two_squares();
        let top = Layer::new("Transparent fill", LayerContent::Fill(Fill::Solid(Color::rgba(1.0, 0.0, 0.0, 0.0))));
        let id = top.id;
        std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).layers.push(top);
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap()["layer"], a.0);

        let top = std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).layer_mut(id).unwrap();
        top.content = LayerContent::Fill(Fill::Solid(Color::rgb(1.0, 0.0, 0.0)));
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap()["layer"], id.0);

        let hidden = photocraft_doc::vector::Path { inverted: true, ..Default::default() };
        std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).layer_mut(id).unwrap().vector_mask = Some(VectorMask::new(hidden));
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap()["layer"], a.0);
    }

    #[test]
    fn group_opacity_and_artboard_clip_limit_child_hits() {
        let (mut s, a, _) = two_squares();
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        let group = LayerId(s.execute("layer.new.groupFromLayers", json!({"name": "Group"})).unwrap()["layer"].as_u64().unwrap());
        std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).layer_mut(group).unwrap().opacity = 0.0;
        let bg = s.active().unwrap().doc.layers[0].id;
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap()["layer"], bg.0);
        std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).layer_mut(group).unwrap().opacity = 1.0;
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5, "target": "group"})).unwrap()["layer"], group.0);
        let layer = std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).layer_mut(group).unwrap();
        if let LayerContent::Group(g) = &mut layer.content {
            g.artboard = Some(Artboard::new(Rect::new(10, 10, 30, 30)));
        }
        assert_eq!(s.execute("layer.pickAt", json!({"x": 5, "y": 5})).unwrap()["layer"], bg.0);
    }

    #[test]
    fn hostile_params_are_errors_or_misses_not_panics() {
        let (mut s, _, _) = two_squares();
        assert!(s.execute("layer.pickAt", json!({})).is_err());
        assert!(s.execute("layer.pickAt", json!({"x": "5", "y": 5})).is_err());
        for (x, y) in [(-1e12, 5.0), (1e12, 1e12), (-0.5, -0.5), (40.0, 40.0)] {
            assert_eq!(s.execute("layer.pickAt", json!({"x": x, "y": y})).unwrap()["layer"], Value::Null, "({x}, {y})");
        }
        let mut empty = Session::new();
        assert!(empty.execute("layer.pickAt", json!({"x": 1, "y": 1})).is_err());
    }
}
