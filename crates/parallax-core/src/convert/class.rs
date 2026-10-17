//! Java class files: the API as an outline, and the bytecode of every method.
//!
//! Both are written for diffing rather than for completeness (javap does that):
//! constant-pool numbers are resolved to what they stand for, jump targets become
//! labels, locals are named from the debug info, members are sorted, and line
//! numbers are left out unless asked for. So recompiling the class, or adding a
//! constant or an instruction elsewhere, does not change unrelated lines.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use cafebabe::attributes::{
    Annotation, AnnotationElementValue, AttributeData, AttributeInfo, BootstrapMethodEntry, CodeData,
    LocalVariableEntry, ModuleData,
};
use cafebabe::bytecode::{Opcode, PrimitiveArrayType};
use cafebabe::constant_pool::{
    BootstrapArgument, LiteralConstant, Loadable, MemberRef, MethodHandle, ObjectArrayType, ReferenceKind,
};
use cafebabe::{ClassAccessFlags, ClassFile, FieldInfo, MethodInfo};

use crate::{Error, Result};

/// Whether the data is a class file: Mach-O universal binaries share the magic
/// number but have a small architecture count where the class version is.
pub fn is_class(data: &[u8]) -> bool {
    data.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]) && data.len() >= 8 && u16::from_be_bytes([data[6], data[7]]) >= 45
}

/// The class header, fields and methods, with a checksum of each method's code.
pub fn outline(data: &[u8]) -> Result<String> {
    Ok(Class::parse(data, Detail { debug: true, lines: false })?.outline())
}

/// The instructions of every method; `lines` adds the source line numbers.
pub fn bytecode(data: &[u8], lines: bool) -> Result<String> {
    Ok(Class::parse(data, Detail { debug: true, lines })?.bytecode())
}

/// Whether two class files have the same API and code, debug info aside (line
/// numbers, local variable names, source file name) and however the constant pool
/// is laid out. `None` when either is not a readable class file.
pub fn same_code(left: &[u8], right: &[u8]) -> Option<bool> {
    let canonical = |data| {
        let class = Class::parse(data, Detail { debug: false, lines: false }).ok()?;
        Some(class.outline() + &class.bytecode())
    };
    Some(canonical(left)? == canonical(right)?)
}

#[derive(Clone, Copy)]
struct Detail {
    /// Use debug info: local variable names, the source file.
    debug: bool,
    /// Mark where source lines start in the bytecode.
    lines: bool,
}

struct Class<'a> {
    file: ClassFile<'a>,
    names: Names,
    detail: Detail,
    /// `lambda$name$N` methods renumbered per enclosing method.
    lambdas: HashMap<String, String>,
}

// ---- Names and types ----

/// How class names are shown: classes of `java.lang` and of the class's own
/// package without the package, nested classes with a dot as in Java source.
#[derive(Default)]
struct Names {
    package: String,
    /// Internal names of member classes → their source names (`a/Outer$Inner` → `a/Outer.Inner`).
    nested: HashMap<String, String>,
}

impl Names {
    /// `java/util/List` → `java.util.List`; classes of `java.lang` lose the package.
    fn class_name(&self, internal: &str) -> String {
        if internal.starts_with('[') {
            return self.descriptor_type(internal);
        }
        let source = self.nested.get(internal).map_or(internal, String::as_str);
        let (package, simple) = source.rsplit_once('/').unwrap_or(("", source));
        if package == "java/lang" || package == self.package { simple.to_string() } else { source.replace('/', ".") }
    }

    /// The type at the start of a descriptor, and the rest.
    fn parse_type<'d>(&self, desc: &'d str) -> Option<(String, &'d str)> {
        let rest = desc.trim_start_matches('[');
        let dims = "[]".repeat(desc.len() - rest.len());
        let base = match rest.as_bytes().first()? {
            b'B' => "byte",
            b'C' => "char",
            b'D' => "double",
            b'F' => "float",
            b'I' => "int",
            b'J' => "long",
            b'S' => "short",
            b'Z' => "boolean",
            b'V' => "void",
            b'L' => {
                let end = rest.find(';')?;
                return Some((self.class_name(&rest[1..end]) + &dims, &rest[end + 1..]));
            }
            _ => return None,
        };
        Some((format!("{base}{dims}"), &rest[1..]))
    }

    fn descriptor_type(&self, desc: &str) -> String {
        self.parse_type(desc).map_or_else(|| desc.to_string(), |(t, _)| t)
    }

    /// Parameter types and return type of a method descriptor.
    fn method_types(&self, desc: &str) -> Option<(Vec<String>, String)> {
        let mut rest = desc.strip_prefix('(')?;
        let mut params = Vec::new();
        while !rest.starts_with(')') {
            let (t, r) = self.parse_type(rest)?;
            params.push(t);
            rest = r;
        }
        let (ret, r) = self.parse_type(&rest[1..])?;
        r.is_empty().then_some((params, ret))
    }
}

/// Number of local variable slots a parameter takes.
fn slots(desc_type: &str) -> u16 {
    if desc_type == "long" || desc_type == "double" { 2 } else { 1 }
}

/// A parser for generic signatures (JVMS 4.7.9.1).
struct Signature<'s> {
    s: &'s str,
    pos: usize,
    n: &'s Names,
}

impl<'s> Signature<'s> {
    fn new(n: &'s Names, s: &'s str) -> Self {
        Signature { s, pos: 0, n }
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.pos).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.pos += 1;
        }
        found
    }

    fn until(&mut self, stops: &[u8]) -> Option<&'s str> {
        let start = self.pos;
        let len = self.s.as_bytes()[start..].iter().position(|b| stops.contains(b))?;
        self.pos += len;
        Some(&self.s[start..start + len])
    }

    fn type_params(&mut self) -> Option<String> {
        if !self.eat(b'<') {
            return Some(String::new());
        }
        let mut params = Vec::new();
        while !self.eat(b'>') {
            let name = self.until(b":")?;
            let mut bounds = Vec::new();
            // The class bound may be empty; interface bounds follow, each after ':'.
            while self.eat(b':') {
                if matches!(self.peek(), Some(b'L' | b'T' | b'[')) {
                    bounds.push(self.reference()?);
                }
            }
            bounds.retain(|b| b != "Object");
            params.push(if bounds.is_empty() {
                name.to_string()
            } else {
                format!("{name} extends {}", bounds.join(" & "))
            });
        }
        Some(format!("<{}>", params.join(", ")))
    }

    fn java_type(&mut self) -> Option<String> {
        match self.peek()? {
            b'L' | b'T' | b'[' => self.reference(),
            _ => {
                let (t, _) = self.n.parse_type(&self.s[self.pos..self.pos + 1])?;
                self.pos += 1;
                Some(t)
            }
        }
    }

    fn reference(&mut self) -> Option<String> {
        match self.peek()? {
            b'[' => {
                self.pos += 1;
                Some(self.java_type()? + "[]")
            }
            b'T' => {
                self.pos += 1;
                let name = self.until(b";")?;
                self.pos += 1;
                Some(name.to_string())
            }
            b'L' => {
                self.pos += 1;
                let mut out = self.n.class_name(self.until(b"<.;")?);
                loop {
                    if self.peek() == Some(b'<') {
                        out += &self.type_args()?;
                    }
                    if self.eat(b'.') {
                        out.push('.');
                        out += self.until(b"<.;")?;
                    } else {
                        break;
                    }
                }
                self.eat(b';').then_some(out)
            }
            _ => None,
        }
    }

    fn type_args(&mut self) -> Option<String> {
        self.eat(b'<');
        let mut args = Vec::new();
        while !self.eat(b'>') {
            args.push(match self.peek()? {
                b'*' => {
                    self.pos += 1;
                    "?".to_string()
                }
                b'+' => {
                    self.pos += 1;
                    format!("? extends {}", self.reference()?)
                }
                b'-' => {
                    self.pos += 1;
                    format!("? super {}", self.reference()?)
                }
                _ => self.reference()?,
            });
        }
        Some(format!("<{}>", args.join(", ")))
    }

    fn done(&self) -> bool {
        self.pos == self.s.len()
    }
}

struct MethodSignature {
    type_params: String,
    params: Vec<String>,
    ret: String,
    throws: Vec<String>,
}

fn method_signature(n: &Names, sig: &str) -> Option<MethodSignature> {
    let mut p = Signature::new(n, sig);
    let type_params = p.type_params()?;
    if !p.eat(b'(') {
        return None;
    }
    let mut params = Vec::new();
    while !p.eat(b')') {
        params.push(p.java_type()?);
    }
    let ret = p.java_type()?;
    let mut throws = Vec::new();
    while p.eat(b'^') {
        throws.push(p.reference()?);
    }
    p.done().then_some(MethodSignature { type_params, params, ret, throws })
}

/// Type parameters, superclass and interfaces of a class signature.
fn class_signature(n: &Names, sig: &str) -> Option<(String, String, Vec<String>)> {
    let mut p = Signature::new(n, sig);
    let type_params = p.type_params()?;
    let superclass = p.reference()?;
    let mut interfaces = Vec::new();
    while !p.done() {
        interfaces.push(p.reference()?);
    }
    Some((type_params, superclass, interfaces))
}

fn field_signature(n: &Names, sig: &str) -> Option<String> {
    let mut p = Signature::new(n, sig);
    let t = p.reference()?;
    p.done().then_some(t)
}

// ---- Values ----

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn float(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        format!("{v:?}")
    }
}

fn literal(c: &LiteralConstant) -> String {
    match c {
        LiteralConstant::Integer(v) => v.to_string(),
        LiteralConstant::Float(v) => float(f64::from(*v)) + "f",
        LiteralConstant::Long(v) => format!("{v}L"),
        LiteralConstant::Double(v) => float(*v),
        LiteralConstant::String(s) => quote(s),
        LiteralConstant::StringBytes(b) => quote(&String::from_utf8_lossy(b)),
    }
}

/// A constant field value as Java writes it: `true` rather than `1` for booleans.
fn constant_value(c: &LiteralConstant, ty: &str) -> String {
    match (c, ty) {
        (LiteralConstant::Integer(v), "boolean") => (*v != 0).to_string(),
        (LiteralConstant::Integer(v), "char") => char::from_u32(*v as u32)
            .map_or_else(|| v.to_string(), |ch| format!("'{}'", quote(&ch.to_string()).trim_matches('"'))),
        _ => literal(c),
    }
}

fn annotation(n: &Names, a: &Annotation) -> String {
    let ty = n.descriptor_type(&a.type_descriptor.to_string());
    match a.elements.as_slice() {
        [] => format!("@{ty}"),
        [only] if only.name == "value" => format!("@{ty}({})", element_value(n, &only.value)),
        elements => {
            let list: Vec<String> =
                elements.iter().map(|e| format!("{} = {}", e.name, element_value(n, &e.value))).collect();
            format!("@{ty}({})", list.join(", "))
        }
    }
}

fn element_value(n: &Names, v: &AnnotationElementValue) -> String {
    match v {
        AnnotationElementValue::ByteConstant(n)
        | AnnotationElementValue::ShortConstant(n)
        | AnnotationElementValue::IntConstant(n) => n.to_string(),
        AnnotationElementValue::CharConstant(c) => constant_value(&LiteralConstant::Integer(*c), "char"),
        AnnotationElementValue::BooleanConstant(n) => (*n != 0).to_string(),
        AnnotationElementValue::LongConstant(n) => format!("{n}L"),
        AnnotationElementValue::FloatConstant(n) => float(f64::from(*n)) + "f",
        AnnotationElementValue::DoubleConstant(n) => float(*n),
        AnnotationElementValue::StringConstant(s) => quote(s),
        AnnotationElementValue::EnumConstant { type_name, const_name } => {
            format!("{}.{const_name}", n.descriptor_type(&type_name.to_string()))
        }
        AnnotationElementValue::ClassLiteral { class_name } => format!("{}.class", n.descriptor_type(class_name)),
        AnnotationElementValue::AnnotationValue(a) => annotation(n, a),
        AnnotationElementValue::ArrayValue(items) => {
            format!("{{{}}}", items.iter().map(|v| element_value(n, v)).collect::<Vec<_>>().join(", "))
        }
    }
}

fn annotations(n: &Names, attributes: &[AttributeInfo]) -> Vec<String> {
    attributes
        .iter()
        .flat_map(|a| match &a.data {
            AttributeData::RuntimeVisibleAnnotations(list) | AttributeData::RuntimeInvisibleAnnotations(list) => {
                list.iter().map(|a| annotation(n, a)).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

/// Per parameter, its annotations, when the attribute covers every parameter.
fn parameter_annotations(n: &Names, attributes: &[AttributeInfo], count: usize) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new(); count];
    for a in attributes {
        if let AttributeData::RuntimeVisibleParameterAnnotations(list)
        | AttributeData::RuntimeInvisibleParameterAnnotations(list) = &a.data
            && list.len() == count
        {
            for (slot, p) in out.iter_mut().zip(list) {
                slot.extend(p.annotations.iter().map(|a| annotation(n, a)));
            }
        }
    }
    out
}

fn signature_attr<'b>(attributes: &'b [AttributeInfo]) -> Option<&'b str> {
    attributes.iter().find_map(|a| match &a.data {
        AttributeData::Signature(s) => Some(s.as_ref()),
        _ => None,
    })
}

fn modifiers(bits: u16, table: &[(u16, &str)]) -> String {
    table.iter().filter(|(bit, _)| bits & bit != 0).map(|(_, name)| format!("{name} ")).collect()
}

const CLASS_MODIFIERS: &[(u16, &str)] =
    &[(0x0001, "public"), (0x0400, "abstract"), (0x0010, "final"), (0x1000, "synthetic")];
const FIELD_MODIFIERS: &[(u16, &str)] = &[
    (0x0001, "public"),
    (0x0004, "protected"),
    (0x0002, "private"),
    (0x0008, "static"),
    (0x0010, "final"),
    (0x0080, "transient"),
    (0x0040, "volatile"),
    (0x1000, "synthetic"),
];
const METHOD_MODIFIERS: &[(u16, &str)] = &[
    (0x0001, "public"),
    (0x0004, "protected"),
    (0x0002, "private"),
    (0x0400, "abstract"),
    (0x0008, "static"),
    (0x0010, "final"),
    (0x0020, "synchronized"),
    (0x0100, "native"),
    (0x0800, "strictfp"),
    (0x1000, "synthetic"),
    (0x0040, "bridge"),
];
const INNER_MODIFIERS: &[(u16, &str)] = &[
    (0x0001, "public"),
    (0x0004, "protected"),
    (0x0002, "private"),
    (0x0400, "abstract"),
    (0x0008, "static"),
    (0x0010, "final"),
    (0x1000, "synthetic"),
];

fn java_version(major: u16, minor: u16) -> String {
    let release = match major {
        45 => "1.1".to_string(),
        46..=48 => format!("1.{}", major - 44),
        49.. => (major - 44).to_string(),
        _ => "?".to_string(),
    };
    let preview = if minor == 0xFFFF && major >= 56 { ", preview features" } else { "" };
    format!("{major}.{minor} (Java {release}{preview})")
}

fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

/// A lambda method's name and number: `lambda$run$3` → (`run`, 3).
fn lambda_parts(name: &str) -> Option<(&str, u32)> {
    let rest = name.strip_prefix("lambda$")?;
    let (method, n) = rest.rsplit_once('$')?;
    Some((method, n.parse().ok()?))
}

// ---- The class ----

impl<'a> Class<'a> {
    fn parse(data: &'a [u8], detail: Detail) -> Result<Self> {
        let file = cafebabe::parse_class(data).map_err(|e| Error::Unsupported(format!("class file: {e}")))?;
        // javac numbers lambdas per class, so a lambda added to one method renumbers
        // those of every later method. Per enclosing method, only its own count.
        let mut by_method: HashMap<&str, Vec<u32>> = HashMap::new();
        for m in &file.methods {
            if let Some((method, n)) = lambda_parts(&m.name) {
                by_method.entry(method).or_default().push(n);
            }
        }
        let mut lambdas = HashMap::new();
        for (method, mut numbers) in by_method {
            numbers.sort_unstable();
            for (i, n) in numbers.into_iter().enumerate() {
                lambdas.insert(format!("lambda${method}${n}"), format!("lambda${method}${i}"));
            }
        }
        let this = file.this_class.to_string();
        let package = this.rsplit_once('/').map_or("", |(p, _)| p).to_string();
        let mut nested = HashMap::new();
        for a in &file.attributes {
            if let AttributeData::InnerClasses(list) = &a.data {
                for e in list {
                    if let (Some(outer), Some(name)) = (&e.outer_class_info, &e.inner_name) {
                        nested.insert(e.inner_class_info.to_string(), format!("{outer}.{name}"));
                    }
                }
            }
        }
        // Resolve nesting over several levels: a/A$B$C → a/A.B.C.
        let mut names = Names { package, nested: HashMap::new() };
        for inner in nested.keys() {
            let mut name = nested[inner].clone();
            while let Some((outer, rest)) = name.rsplit_once('.').map(|(o, r)| (o.to_string(), r.to_string()))
                && let Some(resolved) = nested.get(&outer).filter(|r| !r.ends_with(&format!(".{rest}")))
            {
                name = format!("{resolved}.{rest}");
                if name.len() > 4 * inner.len() {
                    break;
                }
            }
            names.nested.insert(inner.clone(), name);
        }
        Ok(Class { file, names, detail, lambdas })
    }

    fn this(&self) -> &str {
        &self.file.this_class
    }

    fn attr<'s, T>(&'s self, f: impl Fn(&'s AttributeData<'a>) -> Option<T>) -> Option<T> {
        self.file.attributes.iter().find_map(|a| f(&a.data))
    }

    fn bootstrap_methods(&self) -> &[BootstrapMethodEntry<'a>] {
        self.file
            .attributes
            .iter()
            .find_map(|a| match &a.data {
                AttributeData::BootstrapMethods(list) => Some(list.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }

    /// A member name, with lambdas of this class renumbered.
    fn member_name<'n>(&'n self, owner: &str, name: &'n str) -> &'n str {
        if owner == self.this() { self.lambdas.get(name).map_or(name, String::as_str) } else { name }
    }

    fn simple_name(&self) -> String {
        let n = &self.names;
        let name = n.class_name(self.this());
        let simple = name.rsplit(['.', '$']).next().unwrap_or(&name);
        simple.to_string()
    }

    // ---- Outline ----

    fn outline(&self) -> String {
        let n = &self.names;
        let mut out = String::new();
        let f = &self.file;
        let _ = writeln!(out, "=== Class ===");
        line(&mut out, "Version", java_version(f.major_version, f.minor_version));
        if self.detail.debug
            && let Some(source) = self.attr(|d| match d {
                AttributeData::SourceFile(s) => Some(s.to_string()),
                _ => None,
            })
        {
            line(&mut out, "Source file", source);
        }
        if let Some(module) = self.attr(|d| match d {
            AttributeData::Module(m) => Some(m),
            _ => None,
        }) {
            module_outline(n, module, &mut out);
            return out;
        }
        for a in annotations(n, &f.attributes) {
            let _ = writeln!(out, "{a}");
        }
        let _ = writeln!(out, "{}", self.declaration());
        self.class_attributes(&mut out);

        let mut fields: Vec<&FieldInfo> = f.fields.iter().collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        let _ = writeln!(out, "\n=== Fields ({}) ===", fields.len());
        for field in fields {
            for a in annotations(n, &field.attributes) {
                let _ = writeln!(out, "{a}");
            }
            let _ = writeln!(out, "{}", self.field(field));
        }

        let methods = self.sorted_methods();
        let _ = writeln!(out, "\n=== Methods ({}) ===", methods.len());
        for m in methods {
            for a in annotations(n, &m.attributes) {
                let _ = writeln!(out, "{a}");
            }
            let mut text = self.method_header(m);
            if let Some(code) = code_of(m) {
                let lines = self.code_lines(code, Detail { debug: false, lines: false });
                let count = code.bytecode.as_ref().map_or(0, |b| b.opcodes.len());
                let _ = write!(
                    text,
                    "  // code {:08x}, {count} instructions",
                    crc32fast::hash(lines.join("\n").as_bytes())
                );
            }
            let _ = writeln!(out, "{text}");
        }
        out
    }

    fn sorted_methods(&self) -> Vec<&MethodInfo<'a>> {
        let mut methods: Vec<&MethodInfo> = self.file.methods.iter().collect();
        methods.sort_by_cached_key(|m| {
            let group = match m.name.as_ref() {
                "<clinit>" => 0,
                "<init>" => 1,
                _ => 2,
            };
            (group, self.member_name(self.this(), &m.name).to_string(), m.descriptor.to_string())
        });
        methods
    }

    fn declaration(&self) -> String {
        let n = &self.names;
        let f = &self.file;
        let flags = f.access_flags;
        let is_record = self.attr(|d| matches!(d, AttributeData::Record(_)).then_some(())).is_some();
        let (kind, mut bits) = if flags.contains(ClassAccessFlags::ANNOTATION) {
            ("@interface", flags.bits() & !0x0400)
        } else if flags.contains(ClassAccessFlags::INTERFACE) {
            ("interface", flags.bits() & !0x0400)
        } else if flags.contains(ClassAccessFlags::ENUM) {
            ("enum", flags.bits())
        } else if is_record {
            ("record", flags.bits() & !0x0010) // Records are always final.
        } else {
            ("class", flags.bits())
        };
        bits &= !0x0020; // ACC_SUPER says nothing about the class.
        let erased_super = f.super_class.as_ref().map(|s| n.class_name(s));
        let erased_interfaces: Vec<String> = f.interfaces.iter().map(|i| n.class_name(i)).collect();
        let (type_params, superclass, interfaces) = signature_attr(&f.attributes)
            .and_then(|sig| class_signature(n, sig))
            .filter(|(_, _, i)| i.len() == erased_interfaces.len())
            .map_or((String::new(), erased_super, erased_interfaces), |(t, s, i)| (t, Some(s), i));
        let sealed =
            self.attr(|d| matches!(d, AttributeData::PermittedSubclasses(_)).then_some("sealed ")).unwrap_or("");
        let mut out =
            format!("{}{sealed}{kind} {}{type_params}", modifiers(bits, CLASS_MODIFIERS), n.class_name(self.this()));
        let implied_super = match kind {
            "enum" => "Enum",
            "record" => "Record",
            _ => "Object",
        };
        if let Some(s) = superclass.filter(|s| s != "Object" && !s.starts_with(implied_super)) {
            let _ = write!(out, " extends {s}");
        }
        // Annotation types implicitly extend java.lang.annotation.Annotation.
        let interfaces: Vec<String> = if kind == "@interface" {
            {
                let annotation = n.class_name("java/lang/annotation/Annotation");
                interfaces.into_iter().filter(|i| *i != annotation).collect()
            }
        } else {
            interfaces
        };
        if !interfaces.is_empty() {
            let word = if kind.contains("interface") { "extends" } else { "implements" };
            let _ = write!(out, " {word} {}", interfaces.join(", "));
        }
        out
    }

    fn class_attributes(&self, out: &mut String) {
        let n = &self.names;
        let names =
            |list: &[std::borrow::Cow<str>]| list.iter().map(|x| n.class_name(x)).collect::<Vec<_>>().join(", ");
        for a in &self.file.attributes {
            match &a.data {
                AttributeData::PermittedSubclasses(list) => line(out, "Permitted", names(list)),
                AttributeData::NestHost(host) => line(out, "Nest host", n.class_name(host)),
                AttributeData::NestMembers(list) => line(out, "Nest members", names(list)),
                AttributeData::EnclosingMethod { class_name: owner, method } => {
                    let method = method.as_ref().map(|m| format!(".{}", m.name)).unwrap_or_default();
                    line(out, "Enclosed in", format!("{}{method}", n.class_name(owner)));
                }
                AttributeData::Record(components) => {
                    let list: Vec<String> = components
                        .iter()
                        .map(|c| {
                            let ty = signature_attr(&c.attributes)
                                .and_then(|sig| field_signature(n, sig))
                                .unwrap_or_else(|| n.descriptor_type(&c.descriptor.to_string()));
                            format!("{ty} {}", c.name)
                        })
                        .collect();
                    line(out, "Components", list.join(", "));
                }
                AttributeData::InnerClasses(list) => {
                    // Only the classes nested in this one; the attribute also lists
                    // every nested class this class refers to, such as Map.Entry.
                    for entry in list.iter().filter(|e| e.outer_class_info.as_deref() == Some(self.this())) {
                        let flags = modifiers(entry.access_flags.bits(), INNER_MODIFIERS);
                        line(out, "Inner class", format!("{flags}{}", n.class_name(&entry.inner_class_info)));
                    }
                }
                _ => {}
            }
        }
    }

    fn field(&self, field: &FieldInfo) -> String {
        let n = &self.names;
        let erased = n.descriptor_type(&field.descriptor.to_string());
        let ty =
            signature_attr(&field.attributes).and_then(|sig| field_signature(n, sig)).unwrap_or_else(|| erased.clone());
        let mut out = format!("{}{ty} {}", modifiers(field.access_flags.bits(), FIELD_MODIFIERS), field.name);
        for a in &field.attributes {
            if let AttributeData::ConstantValue(c) = &a.data {
                let _ = write!(out, " = {}", constant_value(c, &erased));
            }
        }
        if field.access_flags.bits() & 0x4000 != 0 {
            out.push_str("  // enum constant");
        }
        out
    }

    /// Parameter names: from MethodParameters (compiled with `-parameters`), else
    /// with debug info from the local variables that start at the first instruction.
    fn parameter_names(&self, m: &MethodInfo, types: &[String]) -> Vec<Option<String>> {
        let declared: Vec<Option<String>> = m
            .attributes
            .iter()
            .find_map(|a| match &a.data {
                AttributeData::MethodParameters(list) if list.len() == types.len() => {
                    Some(list.iter().map(|p| p.name.as_ref().map(|n| n.to_string())).collect())
                }
                _ => None,
            })
            .unwrap_or_else(|| vec![None; types.len()]);
        let locals = match code_of(m) {
            Some(code) if self.detail.debug => local_table(code),
            _ => Vec::new(),
        };
        let mut slot = if m.access_flags.bits() & 0x0008 != 0 { 0 } else { 1 };
        types
            .iter()
            .zip(declared)
            .map(|(t, declared)| {
                let name = declared
                    .or_else(|| locals.iter().find(|l| l.index == slot && l.start_pc == 0).map(|l| l.name.to_string()));
                slot += slots(t);
                name
            })
            .collect()
    }

    fn method_header(&self, m: &MethodInfo) -> String {
        let n = &self.names;
        let desc = m.descriptor.to_string();
        let (erased_params, erased_ret) = n.method_types(&desc).unwrap_or_default();
        let generic = signature_attr(&m.attributes).and_then(|sig| method_signature(n, sig));
        // Signatures leave out synthetic parameters (the outer instance of an inner
        // class, an enum's name and ordinal), so they are used only when they line up.
        let (type_params, params, ret, mut throws) = match generic {
            Some(g) if g.params.len() == erased_params.len() => (g.type_params, g.params, g.ret, g.throws),
            Some(g) => (g.type_params, erased_params.clone(), g.ret, g.throws),
            None => (String::new(), erased_params.clone(), erased_ret, Vec::new()),
        };
        if throws.is_empty() {
            throws = m
                .attributes
                .iter()
                .find_map(|a| match &a.data {
                    AttributeData::Exceptions(list) => Some(list.iter().map(|e| n.class_name(e)).collect()),
                    _ => None,
                })
                .unwrap_or_default();
        }
        let names = self.parameter_names(m, &erased_params);
        let annotations = parameter_annotations(n, &m.attributes, params.len());
        let varargs = m.access_flags.bits() & 0x0080 != 0;
        let list: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut t = t.clone();
                if varargs && i + 1 == params.len() && t.ends_with("[]") {
                    t.truncate(t.len() - 2);
                    t.push_str("...");
                }
                let mut p = String::new();
                for a in &annotations[i] {
                    let _ = write!(p, "{a} ");
                }
                p.push_str(&t);
                if let Some(Some(name)) = names.get(i) {
                    let _ = write!(p, " {name}");
                }
                p
            })
            .collect();
        let mods = modifiers(m.access_flags.bits(), METHOD_MODIFIERS);
        let type_params = if type_params.is_empty() { type_params } else { type_params + " " };
        let mut out = match m.name.as_ref() {
            "<clinit>" => "static {}".to_string(),
            "<init>" => format!("{mods}{type_params}{}({})", self.simple_name(), list.join(", ")),
            name => format!("{mods}{type_params}{ret} {}({})", self.member_name(self.this(), name), list.join(", ")),
        };
        if !throws.is_empty() {
            let _ = write!(out, " throws {}", throws.join(", "));
        }
        for a in &m.attributes {
            if let AttributeData::AnnotationDefault(v) = &a.data {
                let _ = write!(out, " default {}", element_value(n, v));
            }
        }
        out
    }

    // ---- Bytecode ----

    fn bytecode(&self) -> String {
        let mut out = String::new();
        for m in self.sorted_methods() {
            let Some(code) = code_of(m) else { continue };
            let _ = writeln!(out, "=== {} ===", self.method_header(m));
            for l in self.code_lines(code, self.detail) {
                let _ = writeln!(out, "{l}");
            }
            out.push('\n');
        }
        if out.is_empty() {
            out.push_str("(no method has code)\n");
        }
        out
    }

    fn code_lines(&self, code: &CodeData, detail: Detail) -> Vec<String> {
        let n = &self.names;
        let Some(bytecode) = &code.bytecode else {
            return vec!["! the bytecode could not be read".into()];
        };
        let ops = &bytecode.opcodes;
        let target = |pc: usize, jump: i32| (pc as i64 + i64::from(jump)) as usize;

        let mut targets = BTreeSet::new();
        for (pc, op) in ops {
            if let Some(j) = jump(op) {
                targets.insert(target(*pc, j));
            }
            match op {
                Opcode::Tableswitch(t) => {
                    targets.insert(target(*pc, t.default));
                    targets.extend(t.jumps.iter().map(|j| target(*pc, *j)));
                }
                Opcode::Lookupswitch(t) => {
                    targets.insert(target(*pc, t.default));
                    targets.extend(t.match_offsets.iter().map(|(_, j)| target(*pc, *j)));
                }
                _ => {}
            }
        }
        for e in &code.exception_table {
            targets.extend([usize::from(e.start_pc), usize::from(e.end_pc), usize::from(e.handler_pc)]);
        }
        let labels: HashMap<usize, String> =
            targets.iter().enumerate().map(|(i, pc)| (*pc, format!("L{}", i + 1))).collect();
        let label = |pc: usize| labels.get(&pc).cloned().unwrap_or_else(|| format!("@{pc}"));

        let locals = if detail.debug { local_table(code) } else { Vec::new() };
        let mut line_starts: HashMap<usize, u16> = HashMap::new();
        if detail.lines {
            for a in &code.attributes {
                if let AttributeData::LineNumberTable(entries) = &a.data {
                    for e in entries {
                        line_starts.insert(usize::from(e.start_pc), e.line_number);
                    }
                }
            }
        }

        let mut out = Vec::with_capacity(ops.len() + labels.len());
        let mut last_line = None;
        for (i, (pc, op)) in ops.iter().enumerate() {
            if let Some(l) = labels.get(pc) {
                out.push(format!("  {l}:"));
            }
            if let Some(&n) = line_starts.get(pc)
                && last_line != Some(n)
            {
                out.push(format!("    // line {n}"));
                last_line = Some(n);
            }
            let next = ops.get(i + 1).map_or(code.code.len(), |(p, _)| *p);
            let local = |slot: u16, store: bool| {
                // A stored variable's scope starts after the store.
                let at = if store { next } else { *pc };
                locals
                    .iter()
                    .find(|l| {
                        l.index == slot
                            && usize::from(l.start_pc) <= at
                            && at <= usize::from(l.start_pc) + usize::from(l.length)
                    })
                    .map_or_else(|| slot.to_string(), |l| l.name.to_string())
            };
            match op {
                Opcode::Tableswitch(t) => {
                    out.push("    tableswitch".into());
                    for (k, j) in t.jumps.iter().enumerate() {
                        out.push(format!("      {}: {}", i64::from(t.low) + k as i64, label(target(*pc, *j))));
                    }
                    out.push(format!("      default: {}", label(target(*pc, t.default))));
                }
                Opcode::Lookupswitch(t) => {
                    out.push("    lookupswitch".into());
                    for (key, j) in &t.match_offsets {
                        out.push(format!("      {key}: {}", label(target(*pc, *j))));
                    }
                    out.push(format!("      default: {}", label(target(*pc, t.default))));
                }
                op => out.push(format!("    {}", self.instruction(op, *pc, &label, &local))),
            }
        }
        if let Some(l) = labels.get(&code.code.len()) {
            out.push(format!("  {l}:"));
        }
        for e in &code.exception_table {
            let ty = e.catch_type.as_ref().map_or_else(|| "any".to_string(), |t| n.class_name(t));
            out.push(format!(
                "  catch {ty} in {}..{} -> {}",
                label(e.start_pc.into()),
                label(e.end_pc.into()),
                label(e.handler_pc.into())
            ));
        }
        out
    }

    fn instruction(
        &self,
        op: &Opcode,
        pc: usize,
        label: &dyn Fn(usize) -> String,
        local: &dyn Fn(u16, bool) -> String,
    ) -> String {
        let n = &self.names;
        let at = |j: i32| label((pc as i64 + i64::from(j)) as usize);
        match op {
            Opcode::Aload(s) => format!("aload {}", local(*s, false)),
            Opcode::Dload(s) => format!("dload {}", local(*s, false)),
            Opcode::Fload(s) => format!("fload {}", local(*s, false)),
            Opcode::Iload(s) => format!("iload {}", local(*s, false)),
            Opcode::Lload(s) => format!("lload {}", local(*s, false)),
            Opcode::Astore(s) => format!("astore {}", local(*s, true)),
            Opcode::Dstore(s) => format!("dstore {}", local(*s, true)),
            Opcode::Fstore(s) => format!("fstore {}", local(*s, true)),
            Opcode::Istore(s) => format!("istore {}", local(*s, true)),
            Opcode::Lstore(s) => format!("lstore {}", local(*s, true)),
            Opcode::Ret(s) => format!("ret {}", local(*s, false)),
            Opcode::Iinc(s, by) => format!("iinc {} {by}", local(*s, false)),
            Opcode::Bipush(v) => format!("bipush {v}"),
            Opcode::Sipush(v) => format!("sipush {v}"),
            Opcode::Anewarray(t) => format!("anewarray {}", object_type(n, t)),
            Opcode::Checkcast(t) => format!("checkcast {}", object_type(n, t)),
            Opcode::Instanceof(t) => format!("instanceof {}", object_type(n, t)),
            Opcode::Multianewarray(t, dims) => format!("multianewarray {} {dims}", object_type(n, t)),
            Opcode::New(t) => format!("new {}", n.class_name(t)),
            Opcode::Newarray(t) => format!("newarray {}", primitive(*t)),
            Opcode::Getfield(m) => format!("getfield {}", self.field_ref(m)),
            Opcode::Getstatic(m) => format!("getstatic {}", self.field_ref(m)),
            Opcode::Putfield(m) => format!("putfield {}", self.field_ref(m)),
            Opcode::Putstatic(m) => format!("putstatic {}", self.field_ref(m)),
            Opcode::Invokevirtual(m) => format!("invokevirtual {}", self.method_ref(m)),
            Opcode::Invokespecial(m) => format!("invokespecial {}", self.method_ref(m)),
            Opcode::Invokestatic(m) => format!("invokestatic {}", self.method_ref(m)),
            Opcode::Invokeinterface(m, _) => format!("invokeinterface {}", self.method_ref(m)),
            Opcode::Invokedynamic(d) => {
                format!(
                    "invokedynamic {}",
                    self.dynamic_call(d.attr_index, &d.name_and_type.name, &d.name_and_type.descriptor)
                )
            }
            // ldc, ldc_w and ldc2_w differ only in the width of the pool index.
            Opcode::Ldc(v) | Opcode::LdcW(v) | Opcode::Ldc2W(v) => format!("ldc {}", self.loadable(v)),
            Opcode::Goto(j) => format!("goto {}", at(*j)),
            Opcode::Jsr(j) => format!("jsr {}", at(*j)),
            Opcode::IfAcmpeq(j) => format!("if_acmpeq {}", at(*j)),
            Opcode::IfAcmpne(j) => format!("if_acmpne {}", at(*j)),
            Opcode::IfIcmpeq(j) => format!("if_icmpeq {}", at(*j)),
            Opcode::IfIcmpge(j) => format!("if_icmpge {}", at(*j)),
            Opcode::IfIcmpgt(j) => format!("if_icmpgt {}", at(*j)),
            Opcode::IfIcmple(j) => format!("if_icmple {}", at(*j)),
            Opcode::IfIcmplt(j) => format!("if_icmplt {}", at(*j)),
            Opcode::IfIcmpne(j) => format!("if_icmpne {}", at(*j)),
            Opcode::Ifeq(j) => format!("ifeq {}", at(*j)),
            Opcode::Ifge(j) => format!("ifge {}", at(*j)),
            Opcode::Ifgt(j) => format!("ifgt {}", at(*j)),
            Opcode::Ifle(j) => format!("ifle {}", at(*j)),
            Opcode::Iflt(j) => format!("iflt {}", at(*j)),
            Opcode::Ifne(j) => format!("ifne {}", at(*j)),
            Opcode::Ifnonnull(j) => format!("ifnonnull {}", at(*j)),
            Opcode::Ifnull(j) => format!("ifnull {}", at(*j)),
            op => simple(op).map_or_else(|| format!("{op:?}").to_lowercase(), str::to_string),
        }
    }

    fn field_ref(&self, m: &MemberRef) -> String {
        let n = &self.names;
        format!(
            "{}.{}: {}",
            n.class_name(&m.class_name),
            m.name_and_type.name,
            n.descriptor_type(&m.name_and_type.descriptor)
        )
    }

    fn method_ref(&self, m: &MemberRef) -> String {
        self.method_text(&m.class_name, &m.name_and_type.name, &m.name_and_type.descriptor)
    }

    fn method_text(&self, owner: &str, name: &str, desc: &str) -> String {
        let n = &self.names;
        let name = self.member_name(owner, name);
        match n.method_types(desc) {
            Some((params, _)) if name == "<init>" => format!("{}.<init>({})", n.class_name(owner), params.join(", ")),
            Some((params, ret)) => format!("{}.{name}({}): {ret}", n.class_name(owner), params.join(", ")),
            None => format!("{}.{name}{desc}", n.class_name(owner)),
        }
    }

    fn handle(&self, h: &MethodHandle) -> String {
        let n = &self.names;
        let kind = match h.kind {
            ReferenceKind::GetField => "getfield",
            ReferenceKind::GetStatic => "getstatic",
            ReferenceKind::PutField => "putfield",
            ReferenceKind::PutStatic => "putstatic",
            ReferenceKind::InvokeVirtual => "virtual",
            ReferenceKind::InvokeStatic => "static",
            ReferenceKind::InvokeSpecial => "special",
            ReferenceKind::NewInvokeSpecial => "new",
            ReferenceKind::InvokeInterface => "interface",
        };
        let target = if h.member_ref.descriptor.starts_with('(') {
            self.method_text(&h.class_name, &h.member_ref.name, &h.member_ref.descriptor)
        } else {
            format!(
                "{}.{}: {}",
                n.class_name(&h.class_name),
                h.member_ref.name,
                n.descriptor_type(&h.member_ref.descriptor)
            )
        };
        format!("{kind} {target}")
    }

    fn method_type(&self, desc: &str) -> String {
        let n = &self.names;
        n.method_types(desc).map_or_else(|| desc.to_string(), |(p, r)| format!("({}): {r}", p.join(", ")))
    }

    fn loadable(&self, v: &Loadable) -> String {
        let n = &self.names;
        match v {
            Loadable::LiteralConstant(c) => literal(c),
            Loadable::ClassInfo(c) => format!("{}.class", n.class_name(c)),
            Loadable::MethodHandle(h) => format!("handle {}", self.handle(h)),
            Loadable::MethodType(d) => format!("methodtype {}", self.method_type(d)),
            Loadable::Dynamic(d) => {
                format!(
                    "dynamic {}",
                    self.dynamic_call(d.attr_index, &d.name_and_type.name, &d.name_and_type.descriptor)
                )
            }
        }
    }

    fn bootstrap_argument(&self, a: &BootstrapArgument) -> String {
        let n = &self.names;
        match a {
            BootstrapArgument::LiteralConstant(c) => literal(c),
            BootstrapArgument::ClassInfo(c) => format!("{}.class", n.class_name(c)),
            BootstrapArgument::MethodHandle(h) => self.handle(h),
            BootstrapArgument::MethodType(d) => self.method_type(d),
        }
    }

    /// An invokedynamic call (or dynamic constant): lambdas and string
    /// concatenation get a short form, others name their bootstrap method.
    fn dynamic_call(&self, index: u16, name: &str, desc: &str) -> String {
        let n = &self.names;
        let Some(bsm) = self.bootstrap_methods().get(usize::from(index)) else {
            return format!("{name}{desc}");
        };
        let owner = bsm.method.class_name.as_ref();
        let args = &bsm.arguments;
        match (owner, bsm.method.member_ref.name.as_ref()) {
            ("java/lang/invoke/LambdaMetafactory", _) if args.len() >= 2 => {
                let interface = n.method_types(desc).map_or_else(String::new, |(_, r)| r);
                format!("lambda {interface}.{name} -> {}", self.bootstrap_argument(&args[1]))
            }
            ("java/lang/invoke/StringConcatFactory", "makeConcatWithConstants") => {
                let parts: Vec<String> = args.iter().map(|a| self.bootstrap_argument(a)).collect();
                format!("concat {}", parts.join(" "))
            }
            _ => {
                let parts: Vec<String> = args.iter().map(|a| self.bootstrap_argument(a)).collect();
                let ty = if desc.starts_with('(') { self.method_type(desc) } else { n.descriptor_type(desc) };
                format!("{name} {ty} bootstrap {} [{}]", self.handle(&bsm.method), parts.join(", "))
            }
        }
    }
}

fn code_of<'b, 'a>(m: &'b MethodInfo<'a>) -> Option<&'b CodeData<'a>> {
    m.attributes.iter().find_map(|a| match &a.data {
        AttributeData::Code(c) => Some(c),
        _ => None,
    })
}

fn local_table<'b, 'a>(code: &'b CodeData<'a>) -> Vec<&'b LocalVariableEntry<'a>> {
    code.attributes
        .iter()
        .flat_map(|a| match &a.data {
            AttributeData::LocalVariableTable(list) => list.iter().collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn object_type(n: &Names, t: &ObjectArrayType) -> String {
    match t {
        ObjectArrayType::ArrayType(d) => n.descriptor_type(&d.to_string()),
        ObjectArrayType::BinaryName(name) => n.class_name(name),
    }
}

fn primitive(t: PrimitiveArrayType) -> &'static str {
    match t {
        PrimitiveArrayType::Boolean => "boolean",
        PrimitiveArrayType::Char => "char",
        PrimitiveArrayType::Float => "float",
        PrimitiveArrayType::Double => "double",
        PrimitiveArrayType::Byte => "byte",
        PrimitiveArrayType::Short => "short",
        PrimitiveArrayType::Int => "int",
        PrimitiveArrayType::Long => "long",
    }
}

fn jump(op: &Opcode) -> Option<i32> {
    match op {
        Opcode::Goto(j)
        | Opcode::Jsr(j)
        | Opcode::IfAcmpeq(j)
        | Opcode::IfAcmpne(j)
        | Opcode::IfIcmpeq(j)
        | Opcode::IfIcmpge(j)
        | Opcode::IfIcmpgt(j)
        | Opcode::IfIcmple(j)
        | Opcode::IfIcmplt(j)
        | Opcode::IfIcmpne(j)
        | Opcode::Ifeq(j)
        | Opcode::Ifge(j)
        | Opcode::Ifgt(j)
        | Opcode::Ifle(j)
        | Opcode::Iflt(j)
        | Opcode::Ifne(j)
        | Opcode::Ifnonnull(j)
        | Opcode::Ifnull(j) => Some(*j),
        _ => None,
    }
}

/// Mnemonics of the instructions without operands.
fn simple(op: &Opcode) -> Option<&'static str> {
    use Opcode as O;
    Some(match op {
        O::Aaload => "aaload",
        O::Aastore => "aastore",
        O::AconstNull => "aconst_null",
        O::Areturn => "areturn",
        O::Arraylength => "arraylength",
        O::Athrow => "athrow",
        O::Baload => "baload",
        O::Bastore => "bastore",
        O::Breakpoint => "breakpoint",
        O::Caload => "caload",
        O::Castore => "castore",
        O::D2f => "d2f",
        O::D2i => "d2i",
        O::D2l => "d2l",
        O::Dadd => "dadd",
        O::Daload => "daload",
        O::Dastore => "dastore",
        O::Dcmpg => "dcmpg",
        O::Dcmpl => "dcmpl",
        O::Dconst0 => "dconst_0",
        O::Dconst1 => "dconst_1",
        O::Ddiv => "ddiv",
        O::Dmul => "dmul",
        O::Dneg => "dneg",
        O::Drem => "drem",
        O::Dreturn => "dreturn",
        O::Dsub => "dsub",
        O::Dup => "dup",
        O::DupX1 => "dup_x1",
        O::DupX2 => "dup_x2",
        O::Dup2 => "dup2",
        O::Dup2X1 => "dup2_x1",
        O::Dup2X2 => "dup2_x2",
        O::F2d => "f2d",
        O::F2i => "f2i",
        O::F2l => "f2l",
        O::Fadd => "fadd",
        O::Faload => "faload",
        O::Fastore => "fastore",
        O::Fcmpg => "fcmpg",
        O::Fcmpl => "fcmpl",
        O::Fconst0 => "fconst_0",
        O::Fconst1 => "fconst_1",
        O::Fconst2 => "fconst_2",
        O::Fdiv => "fdiv",
        O::Fmul => "fmul",
        O::Fneg => "fneg",
        O::Frem => "frem",
        O::Freturn => "freturn",
        O::Fsub => "fsub",
        O::I2b => "i2b",
        O::I2c => "i2c",
        O::I2d => "i2d",
        O::I2f => "i2f",
        O::I2l => "i2l",
        O::I2s => "i2s",
        O::Iadd => "iadd",
        O::Iaload => "iaload",
        O::Iand => "iand",
        O::Iastore => "iastore",
        O::IconstM1 => "iconst_m1",
        O::Iconst0 => "iconst_0",
        O::Iconst1 => "iconst_1",
        O::Iconst2 => "iconst_2",
        O::Iconst3 => "iconst_3",
        O::Iconst4 => "iconst_4",
        O::Iconst5 => "iconst_5",
        O::Idiv => "idiv",
        O::Impdep1 => "impdep1",
        O::Impdep2 => "impdep2",
        O::Imul => "imul",
        O::Ineg => "ineg",
        O::Ior => "ior",
        O::Irem => "irem",
        O::Ireturn => "ireturn",
        O::Ishl => "ishl",
        O::Ishr => "ishr",
        O::Isub => "isub",
        O::Iushr => "iushr",
        O::Ixor => "ixor",
        O::L2d => "l2d",
        O::L2f => "l2f",
        O::L2i => "l2i",
        O::Ladd => "ladd",
        O::Laload => "laload",
        O::Land => "land",
        O::Lastore => "lastore",
        O::Lcmp => "lcmp",
        O::Lconst0 => "lconst_0",
        O::Lconst1 => "lconst_1",
        O::Ldiv => "ldiv",
        O::Lmul => "lmul",
        O::Lneg => "lneg",
        O::Lor => "lor",
        O::Lrem => "lrem",
        O::Lreturn => "lreturn",
        O::Lshl => "lshl",
        O::Lshr => "lshr",
        O::Lsub => "lsub",
        O::Lushr => "lushr",
        O::Lxor => "lxor",
        O::Monitorenter => "monitorenter",
        O::Monitorexit => "monitorexit",
        O::Nop => "nop",
        O::Pop => "pop",
        O::Pop2 => "pop2",
        O::Return => "return",
        O::Saload => "saload",
        O::Sastore => "sastore",
        O::Swap => "swap",
        _ => return None,
    })
}

fn module_outline(n: &Names, m: &ModuleData, out: &mut String) {
    let version = m.version.as_ref().map(|v| format!("@{v}")).unwrap_or_default();
    let _ = writeln!(out, "module {}{version}", m.name);
    let mut requires: Vec<String> = m
        .requires
        .iter()
        .map(|r| {
            let mut flags = String::new();
            if r.flags.bits() & 0x0020 != 0 {
                flags.push_str("transitive ");
            }
            if r.flags.bits() & 0x0040 != 0 {
                flags.push_str("static ");
            }
            format!("requires {flags}{}", r.name)
        })
        .collect();
    requires.sort();
    let mut lines = requires;
    let to = |list: &[std::borrow::Cow<str>]| {
        if list.is_empty() { String::new() } else { format!(" to {}", list.join(", ")) }
    };
    let mut exports: Vec<String> = m
        .exports
        .iter()
        .map(|e| format!("exports {}{}", e.package_name.replace('/', "."), to(&e.exports_to)))
        .collect();
    exports.sort();
    lines.extend(exports);
    let mut opens: Vec<String> =
        m.opens.iter().map(|e| format!("opens {}{}", e.package_name.replace('/', "."), to(&e.opens_to))).collect();
    opens.sort();
    lines.extend(opens);
    let mut uses: Vec<String> = m.uses.iter().map(|u| format!("uses {}", n.class_name(u))).collect();
    uses.sort();
    lines.extend(uses);
    for p in &m.provides {
        let with: Vec<String> = p.provides_with.iter().map(|w| n.class_name(w)).collect();
        lines.push(format!("provides {} with {}", n.class_name(&p.service_interface_name), with.join(", ")));
    }
    for l in lines {
        let _ = writeln!(out, "  {l}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/java/", $name))
        };
    }
    const V1: &[u8] = fixture!("OrderService-v1.class");
    const V1_SHIFTED: &[u8] = fixture!("OrderService-v1-shifted.class");
    const V1_NODEBUG: &[u8] = fixture!("OrderService-v1-nodebug.class");
    const V1_JAVA17: &[u8] = fixture!("OrderService-v1-java17.class");
    const V2: &[u8] = fixture!("OrderService-v2.class");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn outline_of_a_compiled_class() {
        let o = outline(V1).unwrap();
        for expected in [
            "Version:           65.0 (Java 21)",
            "@Deprecated(since = \"2.0\")",
            "public final class OrderService<T extends Comparable<T>> implements Runnable, AutoCloseable",
            "Inner class:       public static final OrderService.Item",
            "public static final char SEP = ';'",
            "public static final long VERSION = 3L",
            "private final java.util.List<OrderService.Item> items",
            "@SafeVarargs\npublic final <R> java.util.List<R> convert(java.util.function.Function<OrderService.Item, R> f, OrderService.Item... extra)  // code ",
            "public java.math.BigDecimal total(java.util.List<OrderService.Item> items) throws java.io.IOException  // code ",
            "public synchronized void run()",
        ] {
            assert!(o.contains(expected), "{expected}\n---\n{o}");
        }
    }

    #[test]
    fn nested_record_and_enum() {
        let item = outline(fixture!("OrderService-Item.class")).unwrap();
        assert!(item.contains("public record OrderService.Item\n"), "{item}");
        assert!(item.contains("Components:        String name, java.math.BigDecimal price, int quantity"), "{item}");
        let status = outline(fixture!("OrderService-Status.class")).unwrap();
        assert!(status.contains("public static final OrderService.Status PAID  // enum constant"), "{status}");
        // MethodParameters without a name: the name comes from the debug info.
        assert!(status.contains("public static OrderService.Status valueOf(String name)"), "{status}");
    }

    #[test]
    fn bytecode_is_symbolic() {
        let b = bytecode(V1, false).unwrap();
        for expected in [
            "=== public int bucket(int code) ===\n    iload code\n    lookupswitch\n      1: L1\n      2: L2\n      3: L3\n      1000: L4\n      default: L5\n  L1:\n    bipush 10\n    ireturn",
            "    invokedynamic concat \"Order \\u0001 with \\u0001 items\"",
            "    invokedynamic lambda Runnable.run -> static OrderService.lambda$convert$0(OrderService.Item[]): void",
            "    aload this\n    getfield OrderService.items: java.util.List\n    invokeinterface java.util.List.stream(): java.util.stream.Stream",
            "    getstatic java.math.BigDecimal.ZERO: java.math.BigDecimal\n    astore sum",
            "  catch Exception in L1..L2 -> L3",
        ] {
            assert!(b.contains(expected), "{expected}\n---\n{b}");
        }
        assert!(!b.contains("// line"), "line numbers are off by default");
        assert!(!b.contains('#') && !b.contains('@'), "no pool indexes or raw offsets:\n{b}");
        assert!(bytecode(V1, true).unwrap().contains("=== public void close() throws Exception ===\n    // line 75\n"));
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let (o1, o2) = (outline(V1).unwrap(), outline(V2).unwrap());
        let strip = |l: &str| l.split("  // code").next().unwrap().to_string();
        let changed: Vec<String> = lines_only_in(&o1, &o2).into_iter().map(strip).collect();
        assert_eq!(
            changed,
            [
                "public static final long VERSION = 3L",
                "=== Methods (10) ===",
                "private static synthetic String lambda$names$0(OrderService.Item i)",
                "public java.util.List<String> names()",
                "public java.math.BigDecimal total(java.util.List<OrderService.Item> items) throws java.io.IOException",
            ]
        );
        let added: Vec<String> = lines_only_in(&o2, &o1).into_iter().map(strip).collect();
        assert!(added.contains(&"public boolean isOpen()".to_string()), "{added:?}");
        // javac numbers lambdas per class: the lambda added in names() would rename
        // the one in convert(), which is numbered per method here.
        assert!(o2.lines().any(|l| l.starts_with("private static synthetic void lambda$convert$0(")));

        // Moving the code down three lines changes no line of the bytecode view.
        assert_eq!(bytecode(V1, false).unwrap(), bytecode(V1_SHIFTED, false).unwrap());
        assert_ne!(bytecode(V1, true).unwrap(), bytecode(V1_SHIFTED, true).unwrap());
    }

    #[test]
    fn same_code_ignores_debug_info_only() {
        assert_eq!(same_code(V1, V1_SHIFTED), Some(true));
        assert_eq!(same_code(V1, V1_NODEBUG), Some(true));
        assert_eq!(same_code(V1, V1_JAVA17), Some(false), "the class version is part of the API");
        assert_eq!(same_code(V1, V2), Some(false));
        assert_eq!(same_code(V1, b"\xCA\xFE\xBA\xBE\x00\x00\x00\x41"), None);
    }

    #[test]
    fn detects_class_files() {
        assert!(is_class(V1));
        // A Mach-O universal binary with two architectures.
        assert!(!is_class(b"\xCA\xFE\xBA\xBE\x00\x00\x00\x02\x01\x00\x00\x07"));
    }

    #[test]
    fn generic_signatures() {
        let n = Names { package: "a".into(), nested: HashMap::from([("a/Outer$In".into(), "a/Outer.In".into())]) };
        let m = method_signature(
            &n,
            "<K:Ljava/lang/Object;V::Ljava/lang/Comparable<-TV;>;>(Ljava/util/Map<TK;+Ljava/util/List<*>;>;[TV;La/Outer$In;)Ljava/util/Map$Entry<TK;TV;>;^Ljava/io/IOException;^TK;",
        )
        .unwrap();
        assert_eq!(m.type_params, "<K, V extends Comparable<? super V>>");
        assert_eq!(m.params, ["java.util.Map<K, ? extends java.util.List<?>>", "V[]", "Outer.In"]);
        assert_eq!(m.ret, "java.util.Map$Entry<K, V>");
        assert_eq!(m.throws, ["java.io.IOException", "K"]);
        let (params, superclass, interfaces) =
            class_signature(&n, "<T:Ljava/lang/Number;>Ljava/util/AbstractList<TT;>;Ljava/util/RandomAccess;").unwrap();
        assert_eq!((params.as_str(), superclass.as_str()), ("<T extends Number>", "java.util.AbstractList<T>"));
        assert_eq!(interfaces, ["java.util.RandomAccess"]);
        assert!(method_signature(&n, "(Ljava/lang/String").is_none());
    }
}
