#![allow(dead_code)]
use crate::{ObjType, OpType, ScalarValue};
use nonzero_ext::nonzero;
use smol_str::SmolStr;

#[derive(Eq, PartialEq, Hash, Debug, Clone, Copy)]
pub(crate) struct ActorId(pub usize);

#[derive(Eq, PartialEq, Debug, Hash, Clone, Copy)]
pub(crate) struct OpId(pub u64, pub ActorId);

#[derive(Eq, PartialEq, Debug, Hash, Clone, Copy)]
pub(crate) enum ObjectId {
    Id(OpId),
    Root,
}

#[derive(PartialEq, Eq, Debug, Hash, Clone, Copy)]
pub(crate) enum ElementId {
    Head,
    Id(OpId),
}

#[derive(PartialEq, Eq, Debug, Hash, Clone)]
pub(crate) enum Key {
    Map(SmolStr),
    Seq(ElementId),
}

#[derive(PartialEq, Debug, Clone)]
pub(crate) struct InternalOp {
    pub action: InternalOpType,
    pub obj: ObjectId,
    pub key: Key,
    pub pred: Vec<OpId>,
    pub insert: bool,
}

impl InternalOp {
    pub fn obj_type(&self) -> Option<ObjType> {
        match self.action {
            InternalOpType::Make(objtype) => Some(objtype),
            _ => None,
        }
    }

    pub fn is_inc(&self) -> bool {
        matches!(self.action, InternalOpType::Inc(_))
    }
}

#[derive(PartialEq, Debug, Clone)]
pub(crate) enum InternalOpType {
    Make(ObjType),
    Del,
    Inc(i64),
    Set(ScalarValue),
}

impl Key {
    pub fn as_element_id(&self) -> Option<ElementId> {
        match self {
            Key::Map(_) => None,
            Key::Seq(eid) => Some(*eid),
        }
    }

    pub fn to_opid(&self) -> Option<OpId> {
        match self.as_element_id()? {
            ElementId::Id(id) => Some(id),
            ElementId::Head => None,
        }
    }
}

impl From<OpId> for ObjectId {
    fn from(id: OpId) -> ObjectId {
        ObjectId::Id(id)
    }
}

impl From<OpId> for ElementId {
    fn from(id: OpId) -> ElementId {
        ElementId::Id(id)
    }
}

impl From<OpId> for Key {
    fn from(id: OpId) -> Key {
        Key::Seq(ElementId::Id(id))
    }
}

impl From<&InternalOpType> for OpType {
    fn from(i: &InternalOpType) -> OpType {
        match i {
            InternalOpType::Del => OpType::Del(nonzero!(1_u32)),
            InternalOpType::Make(ot) => OpType::Make(*ot),
            InternalOpType::Set(v) => OpType::Set(v.clone()),
            InternalOpType::Inc(i) => OpType::Inc(*i),
        }
    }
}

impl From<InternalOpType> for OpType {
    fn from(i: InternalOpType) -> OpType {
        match i {
            InternalOpType::Del => OpType::Del(nonzero!(1_u32)),
            InternalOpType::Make(ot) => OpType::Make(ot),
            InternalOpType::Set(v) => OpType::Set(v),
            InternalOpType::Inc(i) => OpType::Inc(i),
        }
    }
}
