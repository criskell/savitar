use crate::{BlockId, Function};

#[derive(Debug)]
pub struct Dominance {
    immediate: Vec<Option<BlockId>>,
    order: Vec<BlockId>,
    position: Vec<Option<usize>>,
}

impl Dominance {
    #[must_use]
    pub fn of(function: &Function) -> Self {
        let count = function.block_count();
        let mut immediate: Vec<Option<BlockId>> = vec![None; count];
        let mut position: Vec<Option<usize>> = vec![None; count];
        let mut order: Vec<BlockId> = Vec::with_capacity(count);

        let Some(entry) = function.entry() else {
            return Self {
                immediate,
                order,
                position,
            };
        };

        let mut postorder = Vec::with_capacity(count);
        let mut seen = vec![false; count];
        let mut stack: Vec<(BlockId, usize)> = vec![(entry, 0)];
        seen[entry.index() as usize] = true;
        while let Some((block, next)) = stack.pop() {
            let successors = function.block(block).terminator.successors();
            if next < successors.len() {
                stack.push((block, next + 1));
                let successor = successors[next];
                let index = successor.index() as usize;
                if index < count && !seen[index] {
                    seen[index] = true;
                    stack.push((successor, 0));
                }
            } else {
                postorder.push(block);
            }
        }
        postorder.reverse();
        order = postorder;
        for (slot, block) in order.iter().enumerate() {
            position[block.index() as usize] = Some(slot);
        }

        let mut predecessors: Vec<Vec<BlockId>> = vec![Vec::new(); count];
        for (id, block) in function.blocks() {
            if position[id.index() as usize].is_none() {
                continue;
            }
            for successor in block.terminator.successors() {
                if (successor.index() as usize) < count {
                    predecessors[successor.index() as usize].push(id);
                }
            }
        }

        immediate[entry.index() as usize] = Some(entry);
        let mut changed = true;
        while changed {
            changed = false;
            for &block in &order {
                if block == entry {
                    continue;
                }
                let mut candidate: Option<BlockId> = None;
                for &predecessor in &predecessors[block.index() as usize] {
                    if immediate[predecessor.index() as usize].is_none() {
                        continue;
                    }
                    candidate = Some(match candidate {
                        None => predecessor,
                        Some(held) => {
                            meet(&immediate, &position, held, predecessor).unwrap_or(held)
                        }
                    });
                }
                if candidate != immediate[block.index() as usize] {
                    immediate[block.index() as usize] = candidate;
                    changed = true;
                }
            }
        }

        Self {
            immediate,
            order,
            position,
        }
    }

    #[must_use]
    pub fn reachable(&self, block: BlockId) -> bool {
        self.position
            .get(block.index() as usize)
            .copied()
            .flatten()
            .is_some()
    }

    #[must_use]
    pub fn reachable_blocks(&self) -> &[BlockId] {
        &self.order
    }

    #[must_use]
    pub fn immediate_dominator(&self, block: BlockId) -> Option<BlockId> {
        self.immediate
            .get(block.index() as usize)
            .copied()
            .flatten()
    }

    #[must_use]
    pub fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        if !self.reachable(dominator) || !self.reachable(block) {
            return false;
        }
        let mut walk = block;
        loop {
            if walk == dominator {
                return true;
            }
            match self.immediate_dominator(walk) {
                Some(next) if next != walk => walk = next,
                _ => return false,
            }
        }
    }
}

fn meet(
    immediate: &[Option<BlockId>],
    position: &[Option<usize>],
    mut left: BlockId,
    mut right: BlockId,
) -> Option<BlockId> {
    loop {
        let left_slot = position[left.index() as usize]?;
        let right_slot = position[right.index() as usize]?;
        if left_slot == right_slot {
            return Some(left);
        }
        if left_slot > right_slot {
            let next = immediate[left.index() as usize]?;
            if next == left {
                return Some(left);
            }
            left = next;
        } else {
            let next = immediate[right.index() as usize]?;
            if next == right {
                return Some(right);
            }
            right = next;
        }
    }
}
