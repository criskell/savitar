mod dominance;
mod verify;

use savitar_diagnostics::Span;
use savitar_types::TypeId;

pub use dominance::Dominance;
pub use verify::{Malformation, verify};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(u32);

impl ValueId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(u32);

impl BlockId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constant {
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    StrictEqual,
    StrictNotEqual,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
    TypeOf,
    IsNullish,
    ToText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub name: String,
    pub value: ValueId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rvalue {
    Parameter(u32),
    Global(String),
    Constant(Constant),
    Widen(ValueId),
    Narrow(ValueId),
    Await(ValueId),
    Concat(Vec<ValueId>),
    Unary {
        op: UnaryOp,
        operand: ValueId,
    },
    Binary {
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
    },
    ReadField {
        object: ValueId,
        name: String,
    },
    ReadElement {
        array: ValueId,
        index: ValueId,
    },
    Slice {
        array: ValueId,
        from: u32,
    },
    Rest {
        object: ValueId,
        taken: Vec<String>,
    },
    NewObject {
        fields: Vec<(String, ValueId)>,
    },
    NewArray {
        elements: Vec<ValueId>,
    },
    Call {
        callee: ValueId,
        arguments: Vec<ValueId>,
    },
    New {
        callee: ValueId,
        arguments: Vec<ValueId>,
    },
    Closure {
        function: String,
        captured: Vec<ValueId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Assign {
        target: ValueId,
        value: Rvalue,
        span: Span,
    },
    WriteField {
        object: ValueId,
        name: String,
        value: ValueId,
        span: Span,
    },
    WriteElement {
        array: ValueId,
        index: ValueId,
        value: ValueId,
        span: Span,
    },
    ConstructBase {
        base_class: String,
        object: ValueId,
        arguments: Vec<ValueId>,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Jump {
        target: BlockId,
        arguments: Vec<ValueId>,
    },
    Branch {
        condition: ValueId,
        then_block: BlockId,
        then_arguments: Vec<ValueId>,
        else_block: BlockId,
        else_arguments: Vec<ValueId>,
    },
    Return(Option<ValueId>),
    Unreachable,
}

impl Terminator {
    #[must_use]
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Self::Jump { target, .. } => vec![*target],
            Self::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            Self::Return(_) | Self::Unreachable => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Block {
    pub parameters: Vec<ValueId>,
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Debug)]
pub struct Function {
    name: String,
    parameters: Vec<ValueId>,
    captures: Vec<Capture>,
    blocks: Vec<Block>,
    value_types: Vec<TypeId>,
}

impl Function {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            parameters: Vec::new(),
            captures: Vec::new(),
            blocks: Vec::new(),
            value_types: Vec::new(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fresh_value(&mut self, ty: TypeId) -> ValueId {
        let id = ValueId(u32::try_from(self.value_types.len()).expect("value overflow"));
        self.value_types.push(ty);
        id
    }

    pub fn declare_parameter(&mut self, ty: TypeId) -> ValueId {
        let id = self.fresh_value(ty);
        self.parameters.push(id);
        id
    }

    #[must_use]
    pub fn parameters(&self) -> &[ValueId] {
        &self.parameters
    }

    pub fn declare_capture(&mut self, name: impl Into<String>, ty: TypeId) -> ValueId {
        let id = self.fresh_value(ty);
        self.captures.push(Capture {
            name: name.into(),
            value: id,
        });
        id
    }

    #[must_use]
    pub fn captures(&self) -> &[Capture] {
        &self.captures
    }

    #[must_use]
    pub fn type_of(&self, value: ValueId) -> TypeId {
        self.value_types[value.0 as usize]
    }

    #[must_use]
    pub fn value_count(&self) -> usize {
        self.value_types.len()
    }

    pub fn add_block(&mut self, block: Block) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("block overflow"));
        self.blocks.push(block);
        id
    }

    #[must_use]
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut Block {
        &mut self.blocks[id.0 as usize]
    }

    #[must_use]
    pub fn has_block(&self, id: BlockId) -> bool {
        (id.0 as usize) < self.blocks.len()
    }

    pub fn blocks(&self) -> impl Iterator<Item = (BlockId, &Block)> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (BlockId(u32::try_from(index).unwrap_or(u32::MAX)), block))
    }

    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    #[must_use]
    pub fn entry(&self) -> Option<BlockId> {
        (!self.blocks.is_empty()).then_some(BlockId(0))
    }
}
