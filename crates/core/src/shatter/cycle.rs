//! Directed cycle detection used to grow the ILP's lazy constraints.
//!
//! An iterative depth-first search that returns one residual cycle as a closed
//! vertex sequence, or nothing when the graph is acyclic.

use super::EdgeRef;

/// Finds a directed cycle in the graph on `vertex_count` vertices with `edges`,
/// returning it as a vertex sequence whose first and last entries coincide, or
/// `None` when the graph is acyclic. Plain iterative DFS with a recursion stack.
pub(super) fn find_cycle(vertex_count: u32, edges: &[EdgeRef]) -> Option<Vec<u32>> {
    let count = vertex_count as usize;
    let mut successors = vec![Vec::new(); count];
    for edge in edges {
        if let Some(list) = successors.get_mut(edge.source as usize) {
            list.push(edge.target);
        }
    }

    // 0 = unvisited, 1 = on the active stack, 2 = fully explored.
    let mut color = vec![0_u8; count];

    for root in 0..count {
        if color.get(root).copied().unwrap_or(2) != 0 {
            continue;
        }
        if let Some(cycle) = dfs_cycle(root, &successors, &mut color) {
            return Some(cycle);
        }
    }
    None
}

/// One iterative DFS from `root`, detecting a back edge to a vertex still on the
/// active path and reconstructing the cycle from the path stack.
fn dfs_cycle(root: usize, successors: &[Vec<u32>], color: &mut [u8]) -> Option<Vec<u32>> {
    let mut path: Vec<usize> = Vec::new();
    let mut cursor: Vec<usize> = Vec::new();
    path.push(root);
    cursor.push(0);
    if let Some(slot) = color.get_mut(root) {
        *slot = 1;
    }

    while let Some(&vertex) = path.last() {
        let index = cursor.len().saturating_sub(1);
        let next = cursor
            .get(index)
            .copied()
            .and_then(|position| successors.get(vertex).and_then(|list| list.get(position)))
            .copied();

        let Some(target) = next else {
            // exhausted `vertex`: mark it explored and backtrack.
            if let Some(slot) = color.get_mut(vertex) {
                *slot = 2;
            }
            path.pop();
            cursor.pop();
            continue;
        };

        if let Some(slot) = cursor.get_mut(index) {
            *slot += 1;
        }
        let target_index = target as usize;
        match color.get(target_index).copied().unwrap_or(2) {
            0 => {
                path.push(target_index);
                cursor.push(0);
                if let Some(slot) = color.get_mut(target_index) {
                    *slot = 1;
                }
            }
            1 => return Some(reconstruct_cycle(&path, target_index)),
            _ => {}
        }
    }
    None
}

/// Reconstructs a cycle from the active `path` once a back edge to `target` is
/// found: the slice from `target`'s first occurrence to the end, closed by
/// repeating `target`.
fn reconstruct_cycle(path: &[usize], target: usize) -> Vec<u32> {
    let start = path
        .iter()
        .position(|&vertex| vertex == target)
        .unwrap_or(0);
    let mut cycle: Vec<u32> = path
        .iter()
        .skip(start)
        .filter_map(|&vertex| u32::try_from(vertex).ok())
        .collect();
    if let Some(&first) = cycle.first() {
        cycle.push(first);
    }
    cycle
}
