use crate::op_tree::{OpSetMetadata, OpTreeNode};
use crate::query::{binary_search_by, QueryResult, TreeQuery};
use crate::types::{Key, Op, HEAD};
use std::cmp::Ordering;
use std::fmt::Debug;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SeekOp {
    /// the op we are looking for
    op: Op,
    /// The position to insert at
    pub pos: usize,
    /// The indices of ops that this op overwrites
    pub succ: Vec<usize>,
    /// whether a position has been found
    found: bool,
}

impl SeekOp {
    pub fn new(op: &Op) -> Self {
        SeekOp {
            op: op.clone(),
            succ: vec![],
            pos: 0,
            found: false,
        }
    }

    fn lesser_insert(&self, op: &Op, m: &OpSetMetadata) -> bool {
        op.insert && m.lamport_cmp(op.id, self.op.id) == Ordering::Less
    }

    fn greater_opid(&self, op: &Op, m: &OpSetMetadata) -> bool {
        m.lamport_cmp(op.id, self.op.id) == Ordering::Greater
    }

    fn is_target_insert(&self, op: &Op) -> bool {
        if !op.insert {
            return false;
        }
        if self.op.insert {
            op.elemid() == self.op.key.elemid()
        } else {
            op.elemid() == self.op.elemid()
        }
    }
}

impl<const B: usize> TreeQuery<B> for SeekOp {
    fn query_node_with_metadata(
        &mut self,
        child: &OpTreeNode<B>,
        m: &OpSetMetadata,
    ) -> QueryResult {
        if self.found {
            return QueryResult::Descend;
        }
        match self.op.key {
            Key::Seq(HEAD) => {
                while self.pos < child.len() {
                    let op = child.get(self.pos).unwrap();
                    if self.op.overwrites(op) {
                        self.succ.push(self.pos);
                    }
                    if op.insert && m.lamport_cmp(op.id, self.op.id) == Ordering::Less {
                        break;
                    }
                    self.pos += 1;
                }
                QueryResult::Finish
            }
            Key::Seq(e) => {
                if child.index.ops.contains(&e.0) {
                    QueryResult::Descend
                } else {
                    self.pos += child.len();
                    QueryResult::Next
                }
            }
            Key::Map(_) => {
                self.pos = binary_search_by(child, |op| m.key_cmp(&op.key, &self.op.key));
                while self.pos < child.len() {
                    let op = child.get(self.pos).unwrap();
                    if op.key != self.op.key {
                        break;
                    }
                    if self.op.overwrites(op) {
                        self.succ.push(self.pos);
                    }
                    if m.lamport_cmp(op.id, self.op.id) == Ordering::Greater {
                        break;
                    }
                    self.pos += 1;
                }
                QueryResult::Finish
            }
        }
    }

    fn query_element_with_metadata(&mut self, e: &Op, m: &OpSetMetadata) -> QueryResult {
        if !self.found {
            if self.is_target_insert(e) {
                self.found = true;
                if self.op.overwrites(e) {
                    self.succ.push(self.pos);
                }
            }
            self.pos += 1;
            QueryResult::Next
        } else {
            // we have already found the target
            if self.op.overwrites(e) {
                self.succ.push(self.pos);
            }
            if self.op.insert {
                if self.lesser_insert(e, m) {
                    QueryResult::Finish
                } else {
                    self.pos += 1;
                    QueryResult::Next
                }
            } else if e.insert || self.greater_opid(e, m) {
                QueryResult::Finish
            } else {
                self.pos += 1;
                QueryResult::Next
            }
        }
    }
}
