use std::collections::HashSet;

use uuid::Uuid;

use crate::models::{LayoutNode, PaneCreatedBy, PaneLaunchState, StackItem, WorkspaceTab};

pub fn new_workspace_tab(title: String) -> WorkspaceTab {
    let root = new_stack_node(1, PaneCreatedBy::User, None);
    let active_pane_id = first_stack_id(&root);

    WorkspaceTab {
        id: Uuid::new_v4().to_string(),
        title,
        root,
        next_pane_ordinal: 2,
        active_pane_id,
    }
}

pub fn new_stack_node(
    pane_ordinal: u32,
    created_by: PaneCreatedBy,
    source_pane_id: Option<String>,
) -> LayoutNode {
    let item = empty_stack_item();

    LayoutNode::Stack {
        id: Uuid::new_v4().to_string(),
        pane_ordinal,
        pane_label: pane_label_for(pane_ordinal),
        created_by: created_by.clone(),
        launch_state: default_launch_state(&created_by),
        source_pane_id,
        active_item_id: item.id.clone(),
        items: vec![item],
    }
}

pub fn normalize_tab(tab: &mut WorkspaceTab) -> bool {
    let mut changed = false;
    let mut seen_ordinals = HashSet::new();
    let mut next_assign = 1;
    let mut max_ordinal = 0;

    changed |= normalize_node(
        &mut tab.root,
        &mut seen_ordinals,
        &mut next_assign,
        &mut max_ordinal,
    );

    let expected_active_pane_id = match &tab.active_pane_id {
        Some(active_pane_id) if stack_exists(&tab.root, active_pane_id) => Some(active_pane_id.clone()),
        _ => first_stack_id(&tab.root),
    };

    if tab.active_pane_id != expected_active_pane_id {
        tab.active_pane_id = expected_active_pane_id;
        changed = true;
    }

    let expected_next_pane_ordinal = max_ordinal.max(1) + 1;
    if tab.next_pane_ordinal < expected_next_pane_ordinal {
        tab.next_pane_ordinal = expected_next_pane_ordinal;
        changed = true;
    }

    changed
}

pub fn reset_tab_layout(tab: &mut WorkspaceTab) {
    let root = new_stack_node(1, PaneCreatedBy::User, None);
    let active_pane_id = first_stack_id(&root);

    tab.root = root;
    tab.next_pane_ordinal = 2;
    tab.active_pane_id = active_pane_id;
}

pub fn split_stack_node(
    node: &mut LayoutNode,
    target_id: &str,
    direction: &str,
    next_pane_ordinal: &mut u32,
    created_by: PaneCreatedBy,
) -> bool {
    match node {
        LayoutNode::Stack { id, .. } if id == target_id => {
            let source_pane_id = id.clone();
            let current = node.clone();
            let new_child = new_stack_node(
                allocate_pane_ordinal(next_pane_ordinal),
                created_by,
                Some(source_pane_id),
            );

            *node = LayoutNode::Split {
                id: Uuid::new_v4().to_string(),
                direction: direction.to_string(),
                sizes: default_sizes_for_child_count(direction, 2),
                children: vec![current, new_child],
            };
            true
        }
        LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|child| split_stack_node(child, target_id, direction, next_pane_ordinal, created_by.clone())),
        _ => false,
    }
}

pub fn add_session_to_stack(
    node: &mut LayoutNode,
    target_id: &str,
    session_id: &str,
    title: &str,
) -> bool {
    match node {
        LayoutNode::Stack {
            id,
            active_item_id,
            items,
            launch_state,
            ..
        } if id == target_id => {
            *launch_state = PaneLaunchState::Launched;
            if items.len() <= 1 {
                let item_id = items
                    .first()
                    .map(|item| item.id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                items.clear();
                items.push(StackItem {
                    id: item_id.clone(),
                    kind: "terminal".to_string(),
                    session_id: Some(session_id.to_string()),
                    title: title.to_string(),
                });
                *active_item_id = item_id;
                return true;
            }

            let item = StackItem {
                id: Uuid::new_v4().to_string(),
                kind: "terminal".to_string(),
                session_id: Some(session_id.to_string()),
                title: title.to_string(),
            };
            *active_item_id = item.id.clone();
            items.push(item);
            true
        }
        LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|child| add_session_to_stack(child, target_id, session_id, title)),
        _ => false,
    }
}

pub fn set_active_stack_item(node: &mut LayoutNode, target_id: &str, item_id: &str) -> bool {
    match node {
        LayoutNode::Stack {
            id,
            active_item_id,
            items,
            ..
        } if id == target_id && items.iter().any(|item| item.id == item_id) => {
            *active_item_id = item_id.to_string();
            true
        }
        LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|child| set_active_stack_item(child, target_id, item_id)),
        _ => false,
    }
}

pub fn stack_exists(node: &LayoutNode, target_id: &str) -> bool {
    match node {
        LayoutNode::Stack { id, .. } => id == target_id,
        LayoutNode::Split { children, .. } => {
            children.iter().any(|child| stack_exists(child, target_id))
        }
    }
}

pub fn first_stack_id(node: &LayoutNode) -> Option<String> {
    match node {
        LayoutNode::Stack { id, .. } => Some(id.clone()),
        LayoutNode::Split { children, .. } => children.iter().find_map(first_stack_id),
    }
}

pub fn collect_session_ids(node: &LayoutNode, session_ids: &mut Vec<String>) {
    match node {
        LayoutNode::Stack { items, .. } => {
            for item in items {
                if let Some(session_id) = &item.session_id {
                    session_ids.push(session_id.clone());
                }
            }
        }
        LayoutNode::Split { children, .. } => {
            for child in children {
                collect_session_ids(child, session_ids);
            }
        }
    }
}

pub fn close_session_in_layout(
    node: &mut LayoutNode,
    target_stack_id: &str,
    session_id: &str,
) -> bool {
    let mutation = remove_session(node, target_stack_id, session_id);
    match mutation {
        LayoutMutation::Unchanged => false,
        LayoutMutation::Updated => true,
        LayoutMutation::Replace(next) => {
            *node = next;
            true
        }
        LayoutMutation::RemoveNode => {
            clear_root_stack(node);
            true
        }
    }
}

pub fn close_stack_node(node: &mut LayoutNode, target_stack_id: &str) -> Option<Vec<String>> {
    let mut session_ids = Vec::new();
    let mutation = remove_stack(node, target_stack_id, &mut session_ids);
    match mutation {
        LayoutMutation::Unchanged => None,
        LayoutMutation::Updated => Some(session_ids),
        LayoutMutation::Replace(next) => {
            *node = next;
            Some(session_ids)
        }
        LayoutMutation::RemoveNode => {
            clear_root_stack(node);
            Some(session_ids)
        }
    }
}

enum LayoutMutation {
    Unchanged,
    Updated,
    Replace(LayoutNode),
    RemoveNode,
}

fn normalize_node(
    node: &mut LayoutNode,
    seen_ordinals: &mut HashSet<u32>,
    next_assign: &mut u32,
    max_ordinal: &mut u32,
) -> bool {
    match node {
        LayoutNode::Stack {
            pane_ordinal,
            pane_label,
            created_by,
            launch_state,
            active_item_id,
            items,
            ..
        } => {
            let mut changed = false;
            let ordinal = if *pane_ordinal == 0 || seen_ordinals.contains(pane_ordinal) {
                changed = true;
                assign_next_ordinal(seen_ordinals, next_assign)
            } else {
                let ordinal = *pane_ordinal;
                seen_ordinals.insert(ordinal);
                *next_assign = (*next_assign).max(ordinal + 1);
                ordinal
            };

            if *pane_ordinal != ordinal {
                *pane_ordinal = ordinal;
            }

            let expected_label = pane_label_for(ordinal);
            if *pane_label != expected_label {
                *pane_label = expected_label;
                changed = true;
            }

            let expected_launch_state = default_launch_state(created_by);
            if *launch_state != expected_launch_state && *created_by == PaneCreatedBy::Ai {
                *launch_state = expected_launch_state;
                changed = true;
            }

            if items.iter().any(|item| item.session_id.is_some())
                && *launch_state != PaneLaunchState::Launched
            {
                *launch_state = PaneLaunchState::Launched;
                changed = true;
            }

            if items.is_empty() {
                let item = empty_stack_item();
                *active_item_id = item.id.clone();
                items.push(item);
                changed = true;
            } else if !items.iter().any(|item| item.id == *active_item_id) {
                *active_item_id = items[0].id.clone();
                changed = true;
            }

            *max_ordinal = (*max_ordinal).max(ordinal);
            changed
        }
        LayoutNode::Split {
            direction,
            sizes,
            children,
            ..
        } => {
            let mut changed = false;

            for child in children.iter_mut() {
                changed |= normalize_node(child, seen_ordinals, next_assign, max_ordinal);
            }

            let expected_sizes = default_sizes_for_child_count(direction, children.len());
            if sizes.len() != children.len() || sizes.iter().all(|size| *size == 0) {
                *sizes = expected_sizes;
                changed = true;
            }

            changed
        }
    }
}

fn assign_next_ordinal(seen_ordinals: &mut HashSet<u32>, next_assign: &mut u32) -> u32 {
    let mut candidate = (*next_assign).max(1);
    while seen_ordinals.contains(&candidate) {
        candidate += 1;
    }
    seen_ordinals.insert(candidate);
    *next_assign = candidate + 1;
    candidate
}

fn allocate_pane_ordinal(next_pane_ordinal: &mut u32) -> u32 {
    let pane_ordinal = (*next_pane_ordinal).max(1);
    *next_pane_ordinal = pane_ordinal + 1;
    pane_ordinal
}

fn pane_label_for(pane_ordinal: u32) -> String {
    format!("P{pane_ordinal}")
}

fn empty_stack_item() -> StackItem {
    StackItem {
        id: Uuid::new_v4().to_string(),
        kind: "terminal".to_string(),
        session_id: None,
        title: "Empty".to_string(),
    }
}

fn clear_root_stack(node: &mut LayoutNode) {
    match node {
        LayoutNode::Stack {
            active_item_id,
            items,
            ..
        } => {
            let item = empty_stack_item();
            *active_item_id = item.id.clone();
            items.clear();
            items.push(item);
        }
        _ => {
            *node = new_stack_node(1, PaneCreatedBy::User, None);
        }
    }
}

fn default_launch_state(created_by: &PaneCreatedBy) -> PaneLaunchState {
    match created_by {
        PaneCreatedBy::User => PaneLaunchState::Unlaunched,
        PaneCreatedBy::Ai => PaneLaunchState::Launched,
    }
}

fn default_split_sizes(direction: &str) -> Vec<u16> {
    match direction {
        "vertical" => vec![60, 40],
        "horizontal" => vec![55, 45],
        _ => vec![50, 50],
    }
}

fn default_sizes_for_child_count(direction: &str, child_count: usize) -> Vec<u16> {
    match child_count {
        0 => Vec::new(),
        1 => vec![100],
        2 => default_split_sizes(direction),
        _ => {
            let base = 100 / child_count as u16;
            let remainder = 100 % child_count as u16;
            (0..child_count)
                .map(|index| if index == 0 { base + remainder } else { base })
                .collect()
        }
    }
}

fn remove_session(
    node: &mut LayoutNode,
    target_stack_id: &str,
    session_id: &str,
) -> LayoutMutation {
    match node {
        LayoutNode::Stack {
            id,
            active_item_id,
            items,
            ..
        } if id == target_stack_id => {
            let original_len = items.len();
            items.retain(|item| item.session_id.as_deref() != Some(session_id));

            if items.len() == original_len {
                return LayoutMutation::Unchanged;
            }

            if items.is_empty() {
                return LayoutMutation::RemoveNode;
            }

            if !items.iter().any(|item| item.id == *active_item_id) {
                *active_item_id = items[0].id.clone();
            }

            LayoutMutation::Updated
        }
        LayoutNode::Split {
            direction,
            children,
            sizes,
            ..
        } => {
            let mut changed = false;
            let mut index = 0;

            while index < children.len() {
                match remove_session(&mut children[index], target_stack_id, session_id) {
                    LayoutMutation::Unchanged => {
                        index += 1;
                    }
                    LayoutMutation::Updated => {
                        changed = true;
                        index += 1;
                    }
                    LayoutMutation::Replace(next) => {
                        children[index] = next;
                        changed = true;
                        index += 1;
                    }
                    LayoutMutation::RemoveNode => {
                        children.remove(index);
                        changed = true;
                    }
                }
            }

            if !changed {
                return LayoutMutation::Unchanged;
            }

            if children.is_empty() {
                return LayoutMutation::RemoveNode;
            }

            if children.len() == 1 {
                return LayoutMutation::Replace(children[0].clone());
            }

            *sizes = default_sizes_for_child_count(direction, children.len());

            LayoutMutation::Updated
        }
        _ => LayoutMutation::Unchanged,
    }
}

fn remove_stack(
    node: &mut LayoutNode,
    target_stack_id: &str,
    session_ids: &mut Vec<String>,
) -> LayoutMutation {
    match node {
        LayoutNode::Stack { id, .. } if id == target_stack_id => {
            collect_session_ids(node, session_ids);
            LayoutMutation::RemoveNode
        }
        LayoutNode::Split {
            direction,
            children,
            sizes,
            ..
        } => {
            let mut changed = false;
            let mut index = 0;

            while index < children.len() {
                match remove_stack(&mut children[index], target_stack_id, session_ids) {
                    LayoutMutation::Unchanged => {
                        index += 1;
                    }
                    LayoutMutation::Updated => {
                        changed = true;
                        index += 1;
                    }
                    LayoutMutation::Replace(next) => {
                        children[index] = next;
                        changed = true;
                        index += 1;
                    }
                    LayoutMutation::RemoveNode => {
                        children.remove(index);
                        changed = true;
                    }
                }
            }

            if !changed {
                return LayoutMutation::Unchanged;
            }

            if children.is_empty() {
                return LayoutMutation::RemoveNode;
            }

            if children.len() == 1 {
                return LayoutMutation::Replace(children[0].clone());
            }

            *sizes = default_sizes_for_child_count(direction, children.len());

            LayoutMutation::Updated
        }
        _ => LayoutMutation::Unchanged,
    }
}
