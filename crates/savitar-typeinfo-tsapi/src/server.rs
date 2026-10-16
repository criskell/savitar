use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use savitar_diagnostics::Span;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{Field, IndexSignature, Parameter, TypeId, TypeKind, TypeTable};
use serde_json::{Value, json};

use crate::flags::{ObjectFlags, SignatureFlags, SymbolFlags, TypeFlags};
use crate::translate::{Classified, call_signature_kind, classify, optional_parameters};

const MAX_TYPE_DEPTH: u32 = 32;
const MAX_DEFERRED_UNROLLING: u32 = 2;

#[derive(Debug)]
pub enum TsApiError {
    Spawn(std::io::Error),
    Transport(String),
    Protocol(String),
}

impl std::fmt::Display for TsApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not start the type provider: {error}"),
            Self::Transport(message) => write!(f, "transport: {message}"),
            Self::Protocol(message) => write!(f, "protocol: {message}"),
        }
    }
}

impl std::error::Error for TsApiError {}

struct Connection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct TsApi {
    connection: RefCell<Connection>,
    next_id: Cell<u64>,
    unrolled: Cell<u32>,
    snapshot: RefCell<Value>,
    project: RefCell<Value>,
    cache: RefCell<HashMap<u32, TypeId>>,
    resolving: RefCell<HashMap<u32, (TypeId, bool)>>,
}

struct FixedTuple {
    arity: usize,
    readonly: bool,
}

fn property_named<'a>(properties: &'a [Value], wanted: &str) -> Option<&'a Value> {
    properties
        .iter()
        .find(|property| property.get("name").and_then(Value::as_str) == Some(wanted))
}

fn refusal_reached_through_member(member: &str, answer: Answer) -> Answer {
    match answer {
        Answer::Unsupported(reason) => Answer::Unsupported(format!("`{member}`: {reason}")),
        other => other,
    }
}

fn type_flags_of(response: &Value) -> TypeFlags {
    response
        .get("flags")
        .and_then(Value::as_u64)
        .map(|bits| TypeFlags(u32::try_from(bits).unwrap_or(0)))
        .unwrap_or_default()
}

fn file_uri(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    format!("file://{}", absolute.display())
}

impl TsApi {
    pub fn start(binary: &OsStr, cwd: &Path, open_files: &[&Path]) -> Result<Self, TsApiError> {
        let mut child = Command::new(binary)
            .arg("--api")
            .arg("--async")
            .arg("--cwd")
            .arg(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(TsApiError::Spawn)?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
        let api = Self {
            connection: RefCell::new(Connection {
                child,
                stdin,
                stdout,
            }),
            next_id: Cell::new(0),
            unrolled: Cell::new(0),
            snapshot: RefCell::new(Value::Null),
            project: RefCell::new(Value::Null),
            cache: RefCell::new(HashMap::new()),
            resolving: RefCell::new(HashMap::new()),
        };

        let uris: Vec<Value> = open_files
            .iter()
            .map(|path| json!({ "uri": file_uri(path) }))
            .collect();
        let created = api.request("createSnapshot", json!({ "openFiles": uris }))?;

        let snapshot = created
            .get("snapshot")
            .cloned()
            .ok_or_else(|| TsApiError::Protocol("createSnapshot returned no snapshot".into()))?;
        let project = created
            .get("projects")
            .and_then(|projects| projects.get(0))
            .and_then(|project| project.get("id"))
            .cloned()
            .ok_or_else(|| TsApiError::Protocol("createSnapshot returned no project".into()))?;

        *api.snapshot.borrow_mut() = snapshot;
        *api.project.borrow_mut() = project;
        Ok(api)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, TsApiError> {
        self.next_id.set(self.next_id.get() + 1);
        let id = self.next_id.get();
        let body =
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();

        let mut connection = self.connection.borrow_mut();
        write!(connection.stdin, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|()| connection.stdin.write_all(body.as_bytes()))
            .and_then(|()| connection.stdin.flush())
            .map_err(|error| TsApiError::Transport(error.to_string()))?;

        loop {
            let message = read_message(&mut connection.stdout)?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(TsApiError::Protocol(text.to_string()));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| TsApiError::Protocol("response carried no result".into()));
        }
    }

    fn type_response(&self, file: &str, position: u32) -> Result<Value, TsApiError> {
        self.request(
            "getTypeAtPosition",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "file": { "uri": file_uri(Path::new(file)) },
                "position": position,
            }),
        )
    }

    fn enter_resolution(&self, table: &mut TypeTable, id: u32) -> Result<TypeId, TypeId> {
        if let Some(entry) = self.resolving.borrow_mut().get_mut(&id) {
            entry.1 = true;
            return Err(entry.0);
        }
        let slot = table.reserve();
        self.resolving.borrow_mut().insert(id, (slot, false));
        Ok(slot)
    }

    fn leave_resolution(
        &self,
        table: &mut TypeTable,
        id: u32,
        slot: TypeId,
        answer: Answer,
    ) -> Answer {
        let used = self
            .resolving
            .borrow_mut()
            .remove(&id)
            .is_some_and(|entry| entry.1);
        match answer {
            Answer::Known(interned) if used => {
                let kind = table.kind(interned).clone();
                table.fill(slot, kind);
                Answer::Known(slot)
            }
            other => other,
        }
    }

    fn union_members(&self, object_id: u32) -> Result<Vec<Value>, TsApiError> {
        let members = self.request(
            "getTypesOfType",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "objectId": object_id,
            }),
        )?;
        members
            .as_array()
            .cloned()
            .ok_or_else(|| TsApiError::Protocol("getTypesOfType returned no array".into()))
    }

    fn intern_intersection(&self, table: &mut TypeTable, object_id: u32, depth: u32) -> Answer {
        let slot = match self.enter_resolution(table, object_id) {
            Ok(slot) => slot,
            Err(reserved) => return Answer::Known(reserved),
        };
        let answer = self.merge_intersection(table, object_id, depth);
        self.leave_resolution(table, object_id, slot, answer)
    }

    fn merge_intersection(&self, table: &mut TypeTable, object_id: u32, depth: u32) -> Answer {
        let members = match self.union_members(object_id) {
            Ok(members) => members,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        if members.is_empty() {
            return Answer::Unsupported("an intersection with no members".into());
        }

        let mut fields: Vec<Field> = Vec::new();
        let mut indexes: Vec<IndexSignature> = Vec::new();
        for member in &members {
            let interned = match self.intern_response(table, member, depth + 1) {
                Answer::Known(interned) => interned,
                other => return other,
            };
            let TypeKind::Object {
                fields: member_fields,
                indexes: member_indexes,
            } = table.kind(interned).clone()
            else {
                return Answer::Unsupported(
                    "an intersection of something other than object shapes".into(),
                );
            };
            for field in member_fields {
                if let Some(existing) = fields.iter().find(|held| held.name == field.name) {
                    if *existing != field {
                        return Answer::Unsupported(format!(
                            "the intersection declares `{}` twice with different types",
                            field.name
                        ));
                    }
                    continue;
                }
                fields.push(field);
            }
            indexes.extend(member_indexes);
        }

        Answer::Known(table.object_with_indexes(fields, indexes))
    }

    fn intern_union(&self, table: &mut TypeTable, object_id: u32, depth: u32) -> Answer {
        let slot = match self.enter_resolution(table, object_id) {
            Ok(slot) => slot,
            Err(reserved) => return Answer::Known(reserved),
        };
        let answer = self.intern_union_members(table, object_id, depth);
        self.leave_resolution(table, object_id, slot, answer)
    }

    fn intern_union_members(&self, table: &mut TypeTable, object_id: u32, depth: u32) -> Answer {
        let members = match self.union_members(object_id) {
            Ok(members) => members,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        if members.is_empty() {
            return Answer::Unsupported("a union with no members".into());
        }

        let mut interned = Vec::with_capacity(members.len());
        for member in &members {
            match self.intern_response(table, member, depth + 1) {
                Answer::Known(id) => interned.push(id),
                other => return other,
            }
        }
        Answer::Known(table.union(interned))
    }

    fn symbol_name_of_type(&self, object_id: u32) -> Option<String> {
        let symbol = self
            .request(
                "getSymbolOfType",
                json!({
                    "snapshot": *self.snapshot.borrow(),
                    "project": *self.project.borrow(),
                    "objectId": object_id,
                }),
            )
            .ok()?;
        symbol
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn type_arguments(&self, type_id: u32) -> Result<Vec<Value>, TsApiError> {
        let arguments = self.request(
            "getTypeArguments",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "type": type_id,
            }),
        )?;
        arguments
            .as_array()
            .cloned()
            .ok_or_else(|| TsApiError::Protocol("getTypeArguments returned no array".into()))
    }

    fn checker_call(&self, method: &str, field: &str, id: u32) -> Result<Value, TsApiError> {
        self.request(
            method,
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                field: id,
            }),
        )
    }

    fn index_signatures(
        &self,
        table: &mut TypeTable,
        type_id: u32,
        depth: u32,
    ) -> Result<Vec<IndexSignature>, Answer> {
        let infos = self
            .checker_call("getIndexInfosOfType", "type", type_id)
            .map_err(|error| Answer::Unsupported(error.to_string()))?;
        let Some(infos) = infos.as_array() else {
            return Err(Answer::Unsupported(
                "getIndexInfosOfType returned no array".into(),
            ));
        };

        let mut signatures = Vec::with_capacity(infos.len());
        for info in infos {
            let Some(key) = info.get("keyType") else {
                return Err(Answer::Unsupported("an index signature with no key".into()));
            };
            let Some(value) = info.get("valueType") else {
                return Err(Answer::Unsupported(
                    "an index signature with no value".into(),
                ));
            };
            let Answer::Known(key) = self.intern_response(table, key, depth + 1) else {
                return Err(Answer::Unsupported(
                    "an index signature key that is not modelled yet".into(),
                ));
            };
            let Answer::Known(value) = self.intern_response(table, value, depth + 1) else {
                return Err(Answer::Unsupported(
                    "an index signature value that is not modelled yet".into(),
                ));
            };
            let readonly = info
                .get("isReadonly")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            signatures.push(IndexSignature {
                key,
                value,
                readonly,
            });
        }
        Ok(signatures)
    }

    fn operand_of(&self, table: &mut TypeTable, method: &str, id: u32, depth: u32) -> Answer {
        match self.checker_call(method, "objectId", id) {
            Ok(operand) => self.intern_response(table, &operand, depth + 1),
            Err(error) => Answer::Unsupported(format!(
                "the checker would not hand over the operand of a deferred type ({error})"
            )),
        }
    }

    fn named_type(&self, id: u32) -> String {
        match self.request(
            "typeToString",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "type": id,
            }),
        ) {
            Ok(Value::String(printed)) => printed,
            _ => "a deferred type the checker would not print".to_string(),
        }
    }

    fn stopped_unrolling(&self, table: &mut TypeTable, id: u32) -> Answer {
        Answer::Known(table.deferred(self.named_type(id)))
    }

    fn intern_indexed_access(&self, table: &mut TypeTable, id: u32, depth: u32) -> Answer {
        if self.unrolled.get() >= MAX_DEFERRED_UNROLLING {
            return self.stopped_unrolling(table, id);
        }
        self.unrolled.set(self.unrolled.get() + 1);
        let answer = self.unroll_indexed_access(table, id, depth);
        self.unrolled.set(self.unrolled.get() - 1);
        answer
    }

    fn unroll_indexed_access(&self, table: &mut TypeTable, id: u32, depth: u32) -> Answer {
        let object = match self.operand_of(table, "getObjectTypeOfType", id, depth) {
            Answer::Known(object) => object,
            other => return refusal_reached_through_member("the shape `T[K]` indexes", other),
        };
        let index = match self.operand_of(table, "getIndexTypeOfType", id, depth) {
            Answer::Known(index) => index,
            other => return refusal_reached_through_member("the key of `T[K]`", other),
        };
        Answer::Known(table.indexed_access(object, index))
    }

    fn intern_conditional(&self, table: &mut TypeTable, id: u32, depth: u32) -> Answer {
        if self.unrolled.get() >= MAX_DEFERRED_UNROLLING {
            return self.stopped_unrolling(table, id);
        }
        self.unrolled.set(self.unrolled.get() + 1);
        let answer = self.unroll_conditional(table, id, depth);
        self.unrolled.set(self.unrolled.get() - 1);
        answer
    }

    fn unroll_conditional(&self, table: &mut TypeTable, id: u32, depth: u32) -> Answer {
        let mut operands = Vec::with_capacity(4);
        for method in [
            "getCheckTypeOfType",
            "getExtendsTypeOfType",
            "getTrueTypeOfConditionalType",
            "getFalseTypeOfConditionalType",
        ] {
            match self.operand_of(table, method, id, depth) {
                Answer::Known(operand) => operands.push(operand),
                other => return refusal_reached_through_member("a conditional type", other),
            }
        }
        Answer::Known(table.conditional(operands[0], operands[1], operands[2], operands[3]))
    }

    fn symbol_is_readonly(&self, reference: &Value) -> Result<bool, TsApiError> {
        let answer = self.request(
            "isReadonlySymbol",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "symbol": reference,
            }),
        )?;
        answer
            .as_bool()
            .ok_or_else(|| TsApiError::Protocol("isReadonlySymbol returned no boolean".into()))
    }

    fn symbol_type(&self, reference: &Value) -> Result<Value, TsApiError> {
        self.request(
            "getNonMissingTypeOfSymbol",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "symbol": reference,
            }),
        )
    }

    fn intern_signature(&self, table: &mut TypeTable, signature: &Value, depth: u32) -> Answer {
        let Some(signature_id) = signature.get("id").and_then(Value::as_u64) else {
            return Answer::Unsupported("a call signature with no id".into());
        };
        let signature_id = u32::try_from(signature_id).unwrap_or(u32::MAX);

        let mut type_parameters = Vec::new();
        for declared in signature
            .get("typeParameters")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
        {
            let Some(declared) = declared.as_u64() else {
                return Answer::Unsupported("a type parameter with no id".into());
            };
            let declared = u32::try_from(declared).unwrap_or(u32::MAX);
            let known = self.cache.borrow().get(&declared).copied();
            let interned = match known {
                Some(interned) => interned,
                None => {
                    let fresh = table.fresh_type_parameter();
                    self.cache.borrow_mut().insert(declared, fresh);
                    fresh
                }
            };
            type_parameters.push(interned);
        }

        let parameters =
            match self.checker_call("getParametersOfSignature", "objectId", signature_id) {
                Ok(parameters) => parameters,
                Err(error) => return Answer::Unsupported(error.to_string()),
            };
        let Some(parameters) = parameters.as_array() else {
            return Answer::Unsupported("getParametersOfSignature returned no array".into());
        };

        let printed = self.printed_signature(signature_id);
        let marked = optional_parameters(&printed);
        let signature_flags = signature
            .get("flags")
            .and_then(Value::as_u64)
            .map(|bits| SignatureFlags(u32::try_from(bits).unwrap_or(0)))
            .unwrap_or_default();
        let last = parameters.len().saturating_sub(1);

        let mut interned = Vec::with_capacity(parameters.len());
        for (position, parameter) in parameters.iter().enumerate() {
            let Some(reference) = parameter.get("reference") else {
                return Answer::Unsupported("a parameter with no symbol reference".into());
            };
            let parameter_type = match self.symbol_type(reference) {
                Ok(parameter_type) => parameter_type,
                Err(error) => return Answer::Unsupported(error.to_string()),
            };
            let rest = position == last && signature_flags.has(SignatureFlags::HAS_REST_PARAMETER);
            let optional = marked.get(position).copied().unwrap_or(false);
            match self.intern_response(table, &parameter_type, depth + 1) {
                Answer::Known(type_id) => interned.push(Parameter {
                    type_id,
                    rest,
                    optional,
                }),
                other => return other,
            }
        }

        let returns = match self.checker_call("getReturnTypeOfSignature", "objectId", signature_id)
        {
            Ok(returns) => returns,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        match self.intern_response(table, &returns, depth + 1) {
            Answer::Known(returns) => {
                Answer::Known(table.generic_function(type_parameters, interned, returns))
            }
            other => other,
        }
    }

    fn printed_signature(&self, signature_id: u32) -> String {
        let declaration = self.request(
            "signatureToSignatureDeclaration",
            json!({
                "snapshot": *self.snapshot.borrow(),
                "project": *self.project.borrow(),
                "signature": signature_id,
                "kind": call_signature_kind(),
            }),
        );
        let Ok(declaration) = declaration else {
            return String::new();
        };
        let Some(data) = declaration.get("data").cloned() else {
            return String::new();
        };
        match self.request("printNode", json!({ "data": data })) {
            Ok(Value::String(printed)) => printed,
            _ => String::new(),
        }
    }

    fn call_signatures(&self, type_id: u32) -> Result<Vec<Value>, TsApiError> {
        let signatures = self.checker_call("getSignaturesOfType", "type", type_id)?;
        signatures
            .as_array()
            .cloned()
            .ok_or_else(|| TsApiError::Protocol("getSignaturesOfType returned no array".into()))
    }

    fn intern_fields(&self, table: &mut TypeTable, type_id: u32, depth: u32) -> Answer {
        let indexes = match self.index_signatures(table, type_id, depth) {
            Ok(indexes) => indexes,
            Err(answer) => return answer,
        };

        let signatures = match self.call_signatures(type_id) {
            Ok(signatures) => signatures,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };

        let properties = match self.checker_call("getPropertiesOfType", "type", type_id) {
            Ok(properties) => properties,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        let Some(properties) = properties.as_array() else {
            return Answer::Unsupported("getPropertiesOfType returned no array".into());
        };

        if let [signature] = signatures.as_slice() {
            if !properties.is_empty() || !indexes.is_empty() {
                return Answer::Unsupported(
                    "a callable object with properties is not modelled yet".into(),
                );
            }
            return self.intern_signature(table, signature, depth);
        }
        if signatures.len() > 1 {
            if !properties.is_empty() || !indexes.is_empty() {
                return Answer::Unsupported(
                    "a callable object with properties is not modelled yet".into(),
                );
            }
            let mut forms = Vec::with_capacity(signatures.len());
            for signature in &signatures {
                match self.intern_signature(table, signature, depth) {
                    Answer::Known(form) => forms.push(form),
                    other => return other,
                }
            }
            return Answer::Known(table.overloaded(forms));
        }

        let mut fields = Vec::with_capacity(properties.len());
        for property in properties {
            let Some(name) = property.get("name").and_then(Value::as_str) else {
                return Answer::Unsupported("a property with no name".into());
            };
            let symbol_flags = property
                .get("flags")
                .and_then(Value::as_u64)
                .map(|bits| SymbolFlags(u32::try_from(bits).unwrap_or(0)))
                .unwrap_or_default();
            let method = symbol_flags.has(SymbolFlags::METHOD);
            if !method && !symbol_flags.has(SymbolFlags::PROPERTY) {
                return Answer::Unsupported(format!("`{name}` is neither a property nor a method"));
            }

            let Some(reference) = property.get("reference") else {
                return Answer::Unsupported(format!("`{name}` carries no symbol reference"));
            };
            let field_type = match self.symbol_type(reference) {
                Ok(field_type) => field_type,
                Err(error) => return Answer::Unsupported(error.to_string()),
            };
            let readonly = match self.symbol_is_readonly(reference) {
                Ok(readonly) => readonly,
                Err(error) => return Answer::Unsupported(error.to_string()),
            };

            match self.intern_response(table, &field_type, depth + 1) {
                Answer::Known(type_id) => fields.push(Field {
                    name: name.to_string(),
                    type_id,
                    optional: symbol_flags.has(SymbolFlags::OPTIONAL),
                    method,
                    readonly,
                }),
                other => return refusal_reached_through_member(name, other),
            }
        }

        Answer::Known(table.object_with_indexes(fields, indexes))
    }

    fn fixed_tuple_shape(&self, type_id: u32) -> Result<FixedTuple, Answer> {
        let properties = self
            .checker_call("getPropertiesOfType", "type", type_id)
            .map_err(|error| Answer::Unsupported(error.to_string()))?;
        let Some(properties) = properties.as_array() else {
            return Err(Answer::Unsupported(
                "getPropertiesOfType returned no array".into(),
            ));
        };

        let Some(length) = property_named(properties, "length") else {
            return Err(Answer::Unsupported(
                "a tuple whose `length` property the API does not report".into(),
            ));
        };
        let Some(reference) = length.get("reference") else {
            return Err(Answer::Unsupported(
                "a tuple whose `length` carries no symbol reference".into(),
            ));
        };
        let length = self
            .symbol_type(reference)
            .map_err(|error| Answer::Unsupported(error.to_string()))?;
        let flags = type_flags_of(&length);

        if flags.has(TypeFlags::UNION) {
            return Err(Answer::Unsupported(
                "a tuple with an optional element: `length` is a union of arities, not one literal"
                    .into(),
            ));
        }
        if flags.has(TypeFlags::NUMBER) {
            return Err(Answer::Unsupported(
                "a tuple with a rest or variadic element: `length` is `number`, not a literal"
                    .into(),
            ));
        }
        if !flags.has(TypeFlags::NUMBER_LITERAL) {
            return Err(Answer::Unsupported(
                "a tuple whose `length` is neither a numeric literal nor `number`".into(),
            ));
        }

        let Some(arity) = length.get("value").and_then(Value::as_u64) else {
            return Err(Answer::Unsupported(
                "a tuple whose `length` literal carries no whole value".into(),
            ));
        };

        if property_named(properties, "concat").is_none() {
            return Err(Answer::Unsupported(
                "a tuple carrying no array members, so the absence of `push` does not prove it is readonly"
                    .into(),
            ));
        }

        Ok(FixedTuple {
            arity: usize::try_from(arity).unwrap_or(usize::MAX),
            readonly: property_named(properties, "push").is_none(),
        })
    }

    fn intern_tuple(&self, table: &mut TypeTable, id: u32, depth: u32) -> Answer {
        let slot = match self.enter_resolution(table, id) {
            Ok(slot) => slot,
            Err(reserved) => return Answer::Known(reserved),
        };
        let answer = self.intern_tuple_elements(table, id, depth);
        self.leave_resolution(table, id, slot, answer)
    }

    fn intern_tuple_elements(&self, table: &mut TypeTable, id: u32, depth: u32) -> Answer {
        let shape = match self.fixed_tuple_shape(id) {
            Ok(shape) => shape,
            Err(answer) => return answer,
        };

        let arguments = match self.type_arguments(id) {
            Ok(arguments) => arguments,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        if arguments.len() < shape.arity {
            return Answer::Unsupported(format!(
                "a tuple of arity {} with only {} type arguments",
                shape.arity,
                arguments.len()
            ));
        }

        let mut elements = Vec::with_capacity(shape.arity);
        for argument in &arguments[..shape.arity] {
            match self.intern_response(table, argument, depth + 1) {
                Answer::Known(element) => {
                    elements.push(element);
                }
                other => return other,
            }
        }

        Answer::Known(table.tuple(elements, shape.readonly))
    }

    fn intern_wrapper(
        &self,
        table: &mut TypeTable,
        id: u32,
        depth: u32,
        wrap: fn(TypeId) -> TypeKind,
    ) -> Answer {
        let arguments = match self.type_arguments(id) {
            Ok(arguments) => arguments,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        let [only] = arguments.as_slice() else {
            return Answer::Unsupported(format!(
                "a single-argument generic with {} type arguments",
                arguments.len()
            ));
        };
        match self.intern_response(table, only, depth + 1) {
            Answer::Known(inner) => Answer::Known(table.intern(wrap(inner))),
            other => other,
        }
    }

    fn intern_object(
        &self,
        table: &mut TypeTable,
        response: &Value,
        id: u32,
        depth: u32,
    ) -> Answer {
        if response
            .get("isTupleType")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return self.intern_tuple(table, id, depth);
        }

        let object_flags = response
            .get("objectFlags")
            .and_then(Value::as_u64)
            .map(|bits| ObjectFlags(u32::try_from(bits).unwrap_or(0)))
            .unwrap_or_default();

        if !object_flags.has(ObjectFlags::REFERENCE) {
            if !object_flags.has(ObjectFlags(
                ObjectFlags::ANONYMOUS.0
                    | ObjectFlags::INTERFACE.0
                    | ObjectFlags::CLASS.0
                    | ObjectFlags::MAPPED.0,
            )) {
                return Answer::Unsupported("object shapes are not resolved yet".into());
            }
            let slot = match self.enter_resolution(table, id) {
                Ok(slot) => slot,
                Err(reserved) => return Answer::Known(reserved),
            };
            let answer = self.intern_fields(table, id, depth);
            return self.leave_resolution(table, id, slot, answer);
        }

        let name = self.symbol_name_of_type(id);
        if name.as_deref() == Some("Promise") {
            return self.intern_wrapper(table, id, depth, TypeKind::Promise);
        }
        let readonly = match name.as_deref() {
            Some("Array") => false,
            Some("ReadonlyArray") => true,
            _ => {
                let slot = match self.enter_resolution(table, id) {
                    Ok(slot) => slot,
                    Err(reserved) => return Answer::Known(reserved),
                };
                let answer = self.intern_fields(table, id, depth);
                return self.leave_resolution(table, id, slot, answer);
            }
        };

        let arguments = match self.type_arguments(id) {
            Ok(arguments) => arguments,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        let [element] = arguments.as_slice() else {
            return Answer::Unsupported(format!(
                "an array with {} type arguments",
                arguments.len()
            ));
        };

        match self.intern_response(table, element, depth + 1) {
            Answer::Known(element) => Answer::Known(table.intern(if readonly {
                TypeKind::ReadonlyArray(element)
            } else {
                TypeKind::Array(element)
            })),
            other => other,
        }
    }

    fn intern_response(&self, table: &mut TypeTable, response: &Value, depth: u32) -> Answer {
        if depth > MAX_TYPE_DEPTH {
            return Answer::Unsupported(format!(
                "the type nests deeper than {MAX_TYPE_DEPTH} levels"
            ));
        }
        if response.get("intrinsicName").and_then(Value::as_str) == Some("error") {
            return Answer::OutOfScope;
        }
        let Some(id) = response.get("id").and_then(Value::as_u64) else {
            return Answer::Unsupported("the checker answered without a type id".into());
        };
        let id = u32::try_from(id).unwrap_or(u32::MAX);
        if let Some(&cached) = self.cache.borrow().get(&id) {
            return Answer::Known(cached);
        }

        let flags = type_flags_of(response);
        let value = response.get("value");

        let interned = match classify(flags) {
            Classified::Primitive(kind) => table.intern(kind),
            Classified::StringLiteral => match value.and_then(Value::as_str) {
                Some(text) => table.intern(TypeKind::StringLiteral(text.to_string())),
                None => return Answer::Unsupported("a string literal with no value".into()),
            },
            Classified::NumberLiteral => match value.and_then(Value::as_f64) {
                Some(number) => table.number_literal(number),
                None => return Answer::Unsupported("a number literal with no value".into()),
            },
            Classified::BooleanLiteral => match value.and_then(Value::as_bool) {
                Some(flag) => table.intern(TypeKind::BooleanLiteral(flag)),
                None => return Answer::Unsupported("a boolean literal with no value".into()),
            },
            Classified::TypeParameter => table.fresh_type_parameter(),
            Classified::IndexedAccess => {
                let answer = self.intern_indexed_access(table, id, depth);
                if let Answer::Known(interned) = answer {
                    self.cache.borrow_mut().insert(id, interned);
                }
                return answer;
            }
            Classified::Conditional => {
                let answer = self.intern_conditional(table, id, depth);
                if let Answer::Known(interned) = answer {
                    self.cache.borrow_mut().insert(id, interned);
                }
                return answer;
            }
            Classified::Intersection => {
                let answer = self.intern_intersection(table, id, depth);
                if let Answer::Known(interned) = answer {
                    self.cache.borrow_mut().insert(id, interned);
                }
                return answer;
            }
            Classified::Union => {
                let answer = self.intern_union(table, id, depth);
                if let Answer::Known(interned) = answer {
                    self.cache.borrow_mut().insert(id, interned);
                }
                return answer;
            }
            Classified::Object => {
                let answer = self.intern_object(table, response, id, depth);
                if let Answer::Known(interned) = answer {
                    self.cache.borrow_mut().insert(id, interned);
                }
                return answer;
            }
            Classified::Unsupported(reason) => return Answer::Unsupported(reason.to_string()),
        };

        self.cache.borrow_mut().insert(id, interned);
        Answer::Known(interned)
    }
}

impl TypeProvider for TsApi {
    fn name(&self) -> &'static str {
        "tsapi"
    }

    fn type_at(&self, table: &mut TypeTable, file: &str, span: Span) -> Answer {
        match self.type_response(file, span.start) {
            Ok(response) => self.intern_response(table, &response, 0),
            Err(error) => Answer::Unsupported(error.to_string()),
        }
    }
}

impl Drop for TsApi {
    fn drop(&mut self) {
        let mut connection = self.connection.borrow_mut();
        let _ = connection.stdin.flush();
        let _ = connection.child.kill();
        let _ = connection.child.wait();
    }
}

fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value, TsApiError> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = stdout
            .read_line(&mut line)
            .map_err(|error| TsApiError::Transport(error.to_string()))?;
        if read == 0 {
            return Err(TsApiError::Transport("the provider closed stdout".into()));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            length = rest.trim().parse().ok();
        }
    }

    let length =
        length.ok_or_else(|| TsApiError::Protocol("a message without Content-Length".into()))?;
    let mut body = vec![0u8; length];
    stdout
        .read_exact(&mut body)
        .map_err(|error| TsApiError::Transport(error.to_string()))?;
    serde_json::from_slice(&body).map_err(|error| TsApiError::Protocol(error.to_string()))
}
