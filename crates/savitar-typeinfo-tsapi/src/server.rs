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

use crate::flags::{ObjectFlags, SymbolFlags, TypeFlags};
use crate::translate::{Classified, classify};

const MAX_TYPE_DEPTH: u32 = 32;

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
    snapshot: RefCell<Value>,
    project: RefCell<Value>,
    cache: RefCell<HashMap<u32, TypeId>>,
    resolving: RefCell<HashMap<u32, (TypeId, bool)>>,
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
            signatures.push(IndexSignature { key, value });
        }
        Ok(signatures)
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

        let parameters =
            match self.checker_call("getParametersOfSignature", "objectId", signature_id) {
                Ok(parameters) => parameters,
                Err(error) => return Answer::Unsupported(error.to_string()),
            };
        let Some(parameters) = parameters.as_array() else {
            return Answer::Unsupported("getParametersOfSignature returned no array".into());
        };

        let mut interned = Vec::with_capacity(parameters.len());
        for parameter in parameters {
            let Some(reference) = parameter.get("reference") else {
                return Answer::Unsupported("a parameter with no symbol reference".into());
            };
            let parameter_type = match self.symbol_type(reference) {
                Ok(parameter_type) => parameter_type,
                Err(error) => return Answer::Unsupported(error.to_string()),
            };
            match self.intern_response(table, &parameter_type, depth + 1) {
                Answer::Known(type_id) => {
                    if table.admits_undefined(type_id) {
                        return Answer::Unsupported(
                            "the API does not say whether a parameter that admits `undefined` is optional"
                                .into(),
                        );
                    }
                    interned.push(Parameter { type_id });
                }
                other => return other,
            }
        }

        let returns = match self.checker_call("getReturnTypeOfSignature", "objectId", signature_id)
        {
            Ok(returns) => returns,
            Err(error) => return Answer::Unsupported(error.to_string()),
        };
        match self.intern_response(table, &returns, depth + 1) {
            Answer::Known(returns) => Answer::Known(table.function(interned, returns)),
            other => other,
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
            return Answer::Unsupported(format!(
                "an overloaded signature with {} forms is not modelled yet",
                signatures.len()
            ));
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

            match self.intern_response(table, &field_type, depth + 1) {
                Answer::Known(type_id) => fields.push(Field {
                    name: name.to_string(),
                    type_id,
                    optional: symbol_flags.has(SymbolFlags::OPTIONAL),
                    method,
                }),
                other => return other,
            }
        }

        Answer::Known(table.object_with_indexes(fields, indexes))
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
            return Answer::Unsupported(
                "the API omits a tuple's element flags, so a rest tuple is indistinguishable from a fixed one"
                    .into(),
            );
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
        let Some(id) = response.get("id").and_then(Value::as_u64) else {
            return Answer::Unsupported("the checker answered without a type id".into());
        };
        let id = u32::try_from(id).unwrap_or(u32::MAX);
        if let Some(&cached) = self.cache.borrow().get(&id) {
            return Answer::Known(cached);
        }

        let flags = response
            .get("flags")
            .and_then(Value::as_u64)
            .map(|bits| TypeFlags(u32::try_from(bits).unwrap_or(0)))
            .unwrap_or_default();
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
