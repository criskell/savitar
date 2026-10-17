use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use savitar_diagnostics::Span;
use savitar_ir::{Function, Rvalue, Statement, ValueId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opaque {
    AParameter,
    ACapturedName(String),
    AValueABranchCarriesIn,
    AGlobalName(String),
    AnImportedName { specifier: String, name: String },
    AFieldOfAValue(String),
    AnElementOfAValue,
    AnAwaitedValue,
    AResultOfAnotherCall,
    AValueBuiltInPlace,
    AValueThatDefinesItself,
    AValueNothingInThisBodyDefines,
    ABodyThisRunDoesNotHold(String),
}

impl fmt::Display for Opaque {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AParameter => write!(formatter, "a value that arrives as a parameter"),
            Self::ACapturedName(name) => write!(formatter, "the captured name `{name}`"),
            Self::AValueABranchCarriesIn => write!(formatter, "a value a branch carries in"),
            Self::AGlobalName(name) => write!(formatter, "the global name `{name}`"),
            Self::AnImportedName { specifier, name } => {
                write!(formatter, "`{name}` imported from `{specifier}`")
            }
            Self::AFieldOfAValue(name) => write!(formatter, "the field `{name}` of a value"),
            Self::AnElementOfAValue => write!(formatter, "an element of a value"),
            Self::AnAwaitedValue => write!(formatter, "an awaited value"),
            Self::AResultOfAnotherCall => write!(formatter, "the result of another call"),
            Self::AValueBuiltInPlace => write!(formatter, "a value built in place"),
            Self::AValueThatDefinesItself => {
                write!(formatter, "a value whose definition loops back to itself")
            }
            Self::AValueNothingInThisBodyDefines => {
                write!(formatter, "a value nothing in this body defines")
            }
            Self::ABodyThisRunDoesNotHold(name) => {
                write!(formatter, "`{name}`, whose body this run does not hold")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callee {
    Named(String),
    NotAName(Opaque),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub callee: Callee,
    pub at: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleTouch {
    ANameOfThisModuleIsRead(String),
    ANameOfAnotherModuleIsRead { specifier: String, name: String },
    ANameOfThisModuleIsBound(String),
}

impl fmt::Display for ModuleTouch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ANameOfThisModuleIsRead(name) => {
                write!(formatter, "it reads the module name `{name}`")
            }
            Self::ANameOfAnotherModuleIsRead { specifier, name } => {
                write!(formatter, "it reads `{name}` from `{specifier}`")
            }
            Self::ANameOfThisModuleIsBound(name) => {
                write!(formatter, "it binds the module name `{name}`")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    NoModuleNameIsTouched,
    AModuleNameIsTouched {
        touch: ModuleTouch,
        at: Span,
        through: Vec<String>,
    },
    ACalleeIsNotKnown {
        callee: Opaque,
        at: Span,
        through: Vec<String>,
    },
}

impl Reach {
    #[must_use]
    pub const fn proves_no_module_name_is_touched(&self) -> bool {
        matches!(self, Self::NoModuleNameIsTouched)
    }

    #[must_use]
    pub fn path(&self) -> &[String] {
        match self {
            Self::NoModuleNameIsTouched => &[],
            Self::AModuleNameIsTouched { through, .. }
            | Self::ACalleeIsNotKnown { through, .. } => through,
        }
    }
}

#[must_use]
pub fn said_as_a_path(through: &[String]) -> String {
    through
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<String>>()
        .join(" -> ")
}

#[derive(Debug, Default)]
pub struct CallGraph<'a> {
    bodies: BTreeMap<&'a str, &'a Function>,
}

impl<'a> CallGraph<'a> {
    #[must_use]
    pub fn over(bodies: impl IntoIterator<Item = &'a Function>) -> Self {
        let held: Vec<&'a Function> = bodies.into_iter().collect();
        let mut bodies: BTreeMap<&'a str, &'a Function> = BTreeMap::new();
        for body in &held {
            bodies.insert(body.name(), body);
        }
        for body in &held {
            if let Some(class) = body.the_class_it_constructs() {
                bodies.entry(class).or_insert(body);
            }
        }
        Self { bodies }
    }

    #[must_use]
    pub fn holds(&self, name: &str) -> bool {
        self.bodies.contains_key(name)
    }

    #[must_use]
    pub fn calls_made_by(&self, name: &str) -> Vec<CallSite> {
        self.bodies
            .get(name)
            .map(|body| calls_in(body))
            .unwrap_or_default()
    }

    #[must_use]
    pub fn what_a_call_reaches(&self, site: &CallSite) -> Reach {
        let entry = match &site.callee {
            Callee::NotAName(opaque) => {
                return Reach::ACalleeIsNotKnown {
                    callee: opaque.clone(),
                    at: site.at,
                    through: Vec::new(),
                };
            }
            Callee::Named(name) => name.clone(),
        };

        let mut queue: VecDeque<(String, Vec<String>, Span)> = VecDeque::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut not_known: Option<Reach> = None;
        queue.push_back((entry.clone(), vec![entry], site.at));

        while let Some((name, through, reached_at)) = queue.pop_front() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some(body) = self.bodies.get(name.as_str()) else {
                if not_known.is_none() {
                    not_known = Some(Reach::ACalleeIsNotKnown {
                        callee: Opaque::ABodyThisRunDoesNotHold(name),
                        at: reached_at,
                        through,
                    });
                }
                continue;
            };
            if let Some((touch, at)) = first_module_name_touched(body) {
                return Reach::AModuleNameIsTouched { touch, at, through };
            }
            for onwards in calls_in(body) {
                match onwards.callee {
                    Callee::Named(callee) => {
                        let mut deeper = through.clone();
                        deeper.push(callee.clone());
                        queue.push_back((callee, deeper, onwards.at));
                    }
                    Callee::NotAName(opaque) => {
                        if not_known.is_none() {
                            not_known = Some(Reach::ACalleeIsNotKnown {
                                callee: opaque,
                                at: onwards.at,
                                through: through.clone(),
                            });
                        }
                    }
                }
            }
        }

        not_known.unwrap_or(Reach::NoModuleNameIsTouched)
    }
}

#[must_use]
pub fn calls_in(function: &Function) -> Vec<CallSite> {
    let defined = definitions_of(function);
    let mut found = Vec::new();
    for (_, block) in function.blocks() {
        for statement in &block.statements {
            match statement {
                Statement::Assign {
                    value: Rvalue::Call { callee, .. } | Rvalue::New { callee, .. },
                    span,
                    ..
                } => found.push(CallSite {
                    callee: callee_of(function, &defined, *callee),
                    at: *span,
                }),
                Statement::ConstructBase {
                    base_class, span, ..
                } => found.push(CallSite {
                    callee: Callee::Named(base_class.clone()),
                    at: *span,
                }),
                _ => {}
            }
        }
    }
    found
}

fn definitions_of(function: &Function) -> BTreeMap<ValueId, &Rvalue> {
    let mut defined = BTreeMap::new();
    for (_, block) in function.blocks() {
        for statement in &block.statements {
            if let Statement::Assign { target, value, .. } = statement {
                defined.insert(*target, value);
            }
        }
    }
    defined
}

fn callee_of(function: &Function, defined: &BTreeMap<ValueId, &Rvalue>, from: ValueId) -> Callee {
    let mut value = from;
    let mut followed: BTreeSet<ValueId> = BTreeSet::new();
    loop {
        if !followed.insert(value) {
            return Callee::NotAName(Opaque::AValueThatDefinesItself);
        }
        let Some(rvalue) = defined.get(&value) else {
            return Callee::NotAName(where_it_arrives_from(function, value));
        };
        match rvalue {
            Rvalue::ModuleBinding(name) => return Callee::Named(name.clone()),
            Rvalue::Closure { function: name, .. } => return Callee::Named(name.clone()),
            Rvalue::Widen(inner) | Rvalue::Narrow(inner) => value = *inner,
            Rvalue::Parameter(_) => return Callee::NotAName(Opaque::AParameter),
            Rvalue::Global(name) => {
                return Callee::NotAName(Opaque::AGlobalName(name.clone()));
            }
            Rvalue::Imported { specifier, name } => {
                return Callee::NotAName(Opaque::AnImportedName {
                    specifier: specifier.clone(),
                    name: name.clone(),
                });
            }
            Rvalue::ReadField { name, .. } => {
                return Callee::NotAName(Opaque::AFieldOfAValue(name.clone()));
            }
            Rvalue::ReadElement { .. } => return Callee::NotAName(Opaque::AnElementOfAValue),
            Rvalue::Await(_) => return Callee::NotAName(Opaque::AnAwaitedValue),
            Rvalue::Call { .. } | Rvalue::New { .. } => {
                return Callee::NotAName(Opaque::AResultOfAnotherCall);
            }
            Rvalue::Constant(_)
            | Rvalue::Concat(_)
            | Rvalue::Unary { .. }
            | Rvalue::Binary { .. }
            | Rvalue::Slice { .. }
            | Rvalue::Rest { .. }
            | Rvalue::NewObject { .. }
            | Rvalue::NewArray { .. } => return Callee::NotAName(Opaque::AValueBuiltInPlace),
        }
    }
}

fn where_it_arrives_from(function: &Function, value: ValueId) -> Opaque {
    if function.parameters().contains(&value) {
        return Opaque::AParameter;
    }
    if let Some(capture) = function
        .captures()
        .iter()
        .find(|capture| capture.value == value)
    {
        return Opaque::ACapturedName(capture.name.clone());
    }
    if function
        .blocks()
        .any(|(_, block)| block.parameters.contains(&value))
    {
        return Opaque::AValueABranchCarriesIn;
    }
    Opaque::AValueNothingInThisBodyDefines
}

fn first_module_name_touched(function: &Function) -> Option<(ModuleTouch, Span)> {
    for (_, block) in function.blocks() {
        for statement in &block.statements {
            match statement {
                Statement::Assign {
                    value: Rvalue::ModuleBinding(name),
                    span,
                    ..
                } => {
                    return Some((ModuleTouch::ANameOfThisModuleIsRead(name.clone()), *span));
                }
                Statement::Assign {
                    value: Rvalue::Imported { specifier, name },
                    span,
                    ..
                } => {
                    return Some((
                        ModuleTouch::ANameOfAnotherModuleIsRead {
                            specifier: specifier.clone(),
                            name: name.clone(),
                        },
                        *span,
                    ));
                }
                Statement::BindModule { name, span, .. } => {
                    return Some((ModuleTouch::ANameOfThisModuleIsBound(name.clone()), *span));
                }
                _ => {}
            }
        }
    }
    None
}
