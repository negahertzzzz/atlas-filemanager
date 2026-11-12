use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    Keyword,
    Type,
    Identifier,
    Number,
    String,
    Comment,
    Preprocessor,
    Symbol,
    DiffAdd,
    DiffRemove,
    DiffHeader,
    LogError,
    LogWarn,
    LogInfo,
}

impl TokenKind {
    /// Maps our token kind to a canonical theme token name compatible with HighlightTheme.
    pub fn theme_name(&self) -> &'static str {
        match self {
            TokenKind::Whitespace => "whitespace",
            TokenKind::Keyword => "keyword",
            TokenKind::Type => "type",
            TokenKind::Identifier => "variable",
            TokenKind::Number => "number",
            TokenKind::String => "string",
            TokenKind::Comment => "comment",
            TokenKind::Preprocessor => "preproc",
            TokenKind::Symbol => "operator",
            TokenKind::DiffAdd => "string",     // Green
            TokenKind::DiffRemove => "preproc", // Red
            TokenKind::DiffHeader => "keyword", // Cyan/Blue
            TokenKind::LogError => "preproc",   // Red
            TokenKind::LogWarn => "number",     // Yellow
            TokenKind::LogInfo => "comment",    // Dim
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxToken {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

static C_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "asm",
    "auto",
    "break",
    "case",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "final",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "nullptr",
    "operator",
    "override",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];

static C_TYPES: &[&str] = &[
    "bool",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "double",
    "float",
    "int",
    "long",
    "short",
    "signed",
    "unsigned",
    "wchar_t",
    "size_t",
    "ptrdiff_t",
    "intptr_t",
    "uintptr_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "U8",
    "U16",
    "U32",
    "U64",
    "S8",
    "S16",
    "S32",
    "S64",
    "F32",
    "F64",
    "B32",
    "String8",
    "Vec2F32",
    "Vec4F32",
    "string",
    "string_view",
    "vector",
    "map",
    "unordered_map",
    "set",
    "unordered_set",
    "pair",
    "tuple",
    "optional",
    "variant",
    "unique_ptr",
    "shared_ptr",
    "weak_ptr",
    "span",
];

static JS_KEYWORDS: &[&str] = &[
    "abstract",
    "arguments",
    "as",
    "asserts",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constructor",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "module",
    "namespace",
    "new",
    "null",
    "of",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

static TS_TYPES: &[&str] = &[
    "any",
    "bigint",
    "boolean",
    "never",
    "number",
    "object",
    "string",
    "symbol",
    "undefined",
    "unknown",
    "void",
    "Array",
    "Boolean",
    "Date",
    "Error",
    "Function",
    "Map",
    "Number",
    "Object",
    "Promise",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "WeakMap",
    "WeakSet",
    "Record",
    "Partial",
    "Required",
    "Readonly",
    "Pick",
    "Omit",
    "Exclude",
    "Extract",
    "NonNullable",
    "Parameters",
    "ReturnType",
    "InstanceType",
    "Awaited",
    "Capitalize",
    "Uncapitalize",
    "Uppercase",
    "Lowercase",
    "React",
    "JSX",
    "FC",
    "ReactNode",
    "ReactElement",
    "CSSProperties",
    "PropsWithChildren",
    "HTMLElement",
    "HTMLDivElement",
    "HTMLInputElement",
    "HTMLButtonElement",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "Window",
    "Document",
];

static HTML_TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "area",
    "article",
    "aside",
    "audio",
    "b",
    "base",
    "bdi",
    "bdo",
    "blockquote",
    "body",
    "br",
    "button",
    "canvas",
    "caption",
    "cite",
    "code",
    "col",
    "colgroup",
    "data",
    "datalist",
    "dd",
    "del",
    "details",
    "dfn",
    "dialog",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "i",
    "iframe",
    "img",
    "input",
    "ins",
    "kbd",
    "label",
    "legend",
    "li",
    "link",
    "main",
    "map",
    "mark",
    "menu",
    "meta",
    "meter",
    "nav",
    "noscript",
    "object",
    "ol",
    "optgroup",
    "option",
    "output",
    "p",
    "picture",
    "pre",
    "progress",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "script",
    "section",
    "select",
    "slot",
    "small",
    "source",
    "span",
    "strong",
    "style",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "time",
    "title",
    "tr",
    "track",
    "u",
    "ul",
    "var",
    "video",
    "wbr",
    "svg",
    "path",
    "circle",
    "rect",
    "g",
    "line",
    "polyline",
    "polygon",
    "text",
];

static CSS_KEYWORDS: &[&str] = &[
    "important",
    "media",
    "keyframes",
    "import",
    "font-face",
    "supports",
    "charset",
    "layer",
    "container",
    "page",
    "counter-style",
    "property",
    "namespace",
    "auto",
    "inherit",
    "initial",
    "unset",
    "revert",
    "none",
    "block",
    "inline",
    "flex",
    "grid",
    "absolute",
    "relative",
    "fixed",
    "sticky",
    "hidden",
    "visible",
    "collapse",
    "center",
    "left",
    "right",
    "top",
    "bottom",
    "bold",
    "italic",
    "underline",
    "solid",
    "dashed",
    "dotted",
    "transparent",
    "currentcolor",
];

static CSS_PROPERTIES: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "animation",
    "backdrop-filter",
    "background",
    "background-color",
    "background-image",
    "background-position",
    "background-repeat",
    "background-size",
    "border",
    "border-bottom",
    "border-color",
    "border-left",
    "border-radius",
    "border-right",
    "border-style",
    "border-top",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "color",
    "column-gap",
    "columns",
    "content",
    "cursor",
    "display",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-flow",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "gap",
    "grid",
    "grid-area",
    "grid-column",
    "grid-row",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "inset",
    "justify-content",
    "justify-items",
    "justify-self",
    "left",
    "letter-spacing",
    "line-height",
    "list-style",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "order",
    "outline",
    "overflow",
    "overflow-x",
    "overflow-y",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "pointer-events",
    "position",
    "resize",
    "right",
    "row-gap",
    "scroll-behavior",
    "stroke",
    "fill",
    "text-align",
    "text-decoration",
    "text-overflow",
    "text-transform",
    "top",
    "transform",
    "transform-origin",
    "transition",
    "user-select",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "word-break",
    "word-wrap",
    "z-index",
];

static PY_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del",
    "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in",
    "is", "lambda", "match", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True",
    "try", "type", "while", "with", "yield",
];

static PY_TYPES: &[&str] = &[
    "int",
    "float",
    "str",
    "bool",
    "list",
    "dict",
    "set",
    "tuple",
    "bytes",
    "bytearray",
    "object",
    "type",
    "self",
    "cls",
    "Any",
    "Union",
    "Optional",
    "List",
    "Dict",
    "Set",
    "Tuple",
    "Callable",
    "Iterable",
    "Iterator",
    "Generator",
    "Sequence",
    "Mapping",
];

static RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

static RUST_TYPES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64", "String", "Vec", "Option", "Result", "Some", "None", "Ok",
    "Err", "Box", "Rc", "Arc", "RefCell", "Mutex", "RwLock", "Cell", "Path", "PathBuf", "HashMap",
    "HashSet", "BTreeMap", "BTreeSet",
];

static SQL_KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "create",
    "table",
    "index",
    "view",
    "trigger",
    "drop",
    "alter",
    "add",
    "column",
    "primary",
    "key",
    "foreign",
    "references",
    "not",
    "null",
    "unique",
    "default",
    "autoincrement",
    "check",
    "constraint",
    "and",
    "or",
    "in",
    "is",
    "like",
    "order",
    "by",
    "group",
    "having",
    "limit",
    "offset",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "full",
    "cross",
    "on",
    "as",
    "distinct",
    "union",
    "all",
    "case",
    "when",
    "then",
    "else",
    "end",
    "exists",
    "between",
    "cast",
    "coalesce",
    "count",
    "sum",
    "avg",
    "min",
    "max",
];

static SQL_TYPES: &[&str] = &[
    "integer",
    "int",
    "smallint",
    "bigint",
    "text",
    "varchar",
    "char",
    "real",
    "blob",
    "boolean",
    "numeric",
    "decimal",
    "float",
    "double",
    "date",
    "time",
    "datetime",
    "timestamp",
    "json",
    "jsonb",
    "uuid",
];

static ZIG_KEYWORDS: &[&str] = &[
    "fn",
    "pub",
    "const",
    "var",
    "struct",
    "enum",
    "union",
    "error",
    "inline",
    "comptime",
    "test",
    "try",
    "catch",
    "defer",
    "errdefer",
    "while",
    "for",
    "if",
    "else",
    "switch",
    "return",
    "break",
    "continue",
    "asm",
    "usingnamespace",
    "align",
    "linksection",
    "threadlocal",
    "allowzero",
    "volatile",
    "export",
    "extern",
    "naked",
    "anytype",
    "unreachable",
    "and",
    "or",
    "orelse",
    "null",
    "undefined",
    "true",
    "false",
];

static ZIG_TYPES: &[&str] = &[
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "f16",
    "f32",
    "f64",
    "f80",
    "f128",
    "bool",
    "void",
    "noreturn",
    "type",
    "anyerror",
    "anyopaque",
    "c_int",
    "c_uint",
    "c_long",
    "c_ulong",
    "c_void",
];

static GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
    "true",
    "false",
    "nil",
    "iota",
];

static GO_TYPES: &[&str] = &[
    "bool",
    "byte",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "any",
    "comparable",
    "append",
    "cap",
    "close",
    "copy",
    "delete",
    "len",
    "make",
    "new",
    "panic",
    "print",
    "println",
    "recover",
];

static SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "case", "esac", "while",
    "until", "function", "return", "exit", "echo", "export", "source", "local", "set", "unset",
    "alias", "read",
];

static JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
    "record",
    "sealed",
    "permits",
    "var",
    "yield",
    // Kotlin additions
    "fun",
    "val",
    "when",
    "is",
    "in",
    "object",
    "companion",
    "override",
    "open",
    "data",
    "suspend",
    "inline",
    "it",
];

static JAVA_TYPES: &[&str] = &[
    "String",
    "Integer",
    "Long",
    "Double",
    "Float",
    "Boolean",
    "Byte",
    "Short",
    "Character",
    "Object",
    "Class",
    "System",
    "Thread",
    "Throwable",
    "Exception",
    "List",
    "ArrayList",
    "Map",
    "HashMap",
    "Set",
    "HashSet",
    "Optional",
    "Stream",
    "Int",
    "Any",
    "Unit",
    "Nothing",
];

static CSHARP_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "record",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "var",
    "virtual",
    "void",
    "volatile",
    "while",
    "yield",
    "get",
    "set",
    "init",
    "value",
];

static CSHARP_TYPES: &[&str] = &[
    "Task",
    "List",
    "Dictionary",
    "IEnumerable",
    "Action",
    "Func",
    "String",
    "Int32",
    "Int64",
    "Boolean",
    "Object",
    "DateTime",
    "Guid",
    "Span",
    "Memory",
    "Console",
    "Exception",
];

static PHP_KEYWORDS: &[&str] = &[
    "echo",
    "print",
    "function",
    "class",
    "public",
    "private",
    "protected",
    "static",
    "namespace",
    "use",
    "return",
    "if",
    "else",
    "elseif",
    "while",
    "for",
    "foreach",
    "as",
    "new",
    "try",
    "catch",
    "finally",
    "throw",
    "extends",
    "implements",
    "interface",
    "trait",
    "abstract",
    "final",
    "const",
    "switch",
    "case",
    "break",
    "continue",
    "default",
    "include",
    "require",
    "include_once",
    "require_once",
    "fn",
    "match",
    "readonly",
    "null",
    "true",
    "false",
];

static RUBY_KEYWORDS: &[&str] = &[
    "def",
    "end",
    "class",
    "module",
    "if",
    "elsif",
    "else",
    "unless",
    "while",
    "until",
    "for",
    "in",
    "do",
    "return",
    "yield",
    "self",
    "nil",
    "true",
    "false",
    "require",
    "require_relative",
    "include",
    "extend",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "begin",
    "rescue",
    "ensure",
    "raise",
    "then",
    "when",
    "case",
    "super",
    "alias",
];

static SWIFT_KEYWORDS: &[&str] = &[
    "func",
    "var",
    "let",
    "guard",
    "if",
    "else",
    "switch",
    "case",
    "default",
    "for",
    "in",
    "while",
    "repeat",
    "return",
    "struct",
    "class",
    "enum",
    "protocol",
    "extension",
    "import",
    "init",
    "deinit",
    "self",
    "Self",
    "nil",
    "true",
    "false",
    "public",
    "private",
    "fileprivate",
    "internal",
    "open",
    "override",
    "mutating",
    "nonmutating",
    "throws",
    "rethrows",
    "try",
    "catch",
    "async",
    "await",
    "some",
    "any",
    "actor",
];

static SWIFT_TYPES: &[&str] = &[
    "String",
    "Int",
    "Double",
    "Float",
    "Bool",
    "Array",
    "Dictionary",
    "Set",
    "Optional",
    "View",
    "Text",
];

static LUA_KEYWORDS: &[&str] = &[
    "local", "function", "end", "if", "then", "elseif", "else", "while", "do", "for", "in",
    "repeat", "until", "return", "break", "nil", "true", "false", "not", "and", "or",
];

static DART_KEYWORDS: &[&str] = &[
    "void",
    "class",
    "extends",
    "with",
    "implements",
    "final",
    "const",
    "var",
    "dynamic",
    "async",
    "await",
    "return",
    "import",
    "export",
    "library",
    "part",
    "of",
    "if",
    "else",
    "for",
    "in",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "try",
    "catch",
    "finally",
    "throw",
    "rethrow",
    "new",
    "this",
    "super",
    "true",
    "false",
    "null",
    "static",
    "late",
    "required",
    "factory",
    "get",
    "set",
];

static DART_TYPES: &[&str] = &[
    "int",
    "double",
    "String",
    "bool",
    "List",
    "Map",
    "Set",
    "Future",
    "Stream",
    "Widget",
    "BuildContext",
    "State",
];

static DOCKER_KEYWORDS: &[&str] = &[
    "FROM",
    "RUN",
    "CMD",
    "LABEL",
    "EXPOSE",
    "ENV",
    "ADD",
    "COPY",
    "ENTRYPOINT",
    "VOLUME",
    "USER",
    "WORKDIR",
    "ARG",
    "ONBUILD",
    "STOPSIGNAL",
    "HEALTHCHECK",
    "SHELL",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LangKind {
    Cpp,
    Js,
    Ts,
    Html,
    Css,
    Json,
    Python,
    Rust,
    Zig,
    Go,
    Java,
    CSharp,
    Php,
    Ruby,
    Swift,
    Lua,
    Dart,
    Sql,
    Shell,
    Config,
    Docker,
    Markdown,
    Diff,
    Log,
    Plain,
}

#[inline]
fn is_pascal_case(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_uppercase() {
        return false;
    }
    // Must contain at least one lowercase ASCII character to avoid all-caps constants like MAX_SIZE
    bytes.iter().skip(1).any(|b| b.is_ascii_lowercase())
}

pub fn detect_lang(ext: &str) -> LangKind {
    let lower = ext.trim_start_matches('.').to_ascii_lowercase();
    match lower.as_str() {
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "inl" | "hlsl" | "glsl" | "frag" | "vert"
        | "wgsl" => LangKind::Cpp,
        "js" | "mjs" | "cjs" | "jsx" => LangKind::Js,
        "ts" | "mts" | "cts" | "tsx" => LangKind::Ts,
        "html" | "htm" | "xhtml" | "xml" | "svg" | "rss" | "atom" | "xaml" | "plist" | "vue"
        | "svelte" | "astro" => LangKind::Html,
        "css" | "scss" | "sass" | "less" | "pcss" => LangKind::Css,
        "json" | "jsonc" | "json5" => LangKind::Json,
        "py" | "pyw" | "ipynb" => LangKind::Python,
        "rs" => LangKind::Rust,
        "zig" => LangKind::Zig,
        "go" => LangKind::Go,
        "java" | "kt" | "kts" | "groovy" | "scala" => LangKind::Java,
        "cs" | "csx" | "fs" => LangKind::CSharp,
        "php" | "phtml" => LangKind::Php,
        "rb" | "rake" | "gemspec" | "erb" => LangKind::Ruby,
        "swift" => LangKind::Swift,
        "lua" => LangKind::Lua,
        "dart" => LangKind::Dart,
        "sql" | "sqlite" | "db" => LangKind::Sql,
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat" | "cmd" => LangKind::Shell,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "gitignore"
        | "gitattributes" | "properties" => LangKind::Config,
        "dockerfile" | "containerfile" | "makefile" | "mk" | "justfile" | "cmake" => {
            LangKind::Docker
        }
        "md" | "markdown" | "mdx" => LangKind::Markdown,
        "diff" | "patch" => LangKind::Diff,
        "log" | "err" | "out" => LangKind::Log,
        _ => LangKind::Plain,
    }
}

/// Tokenizes a single line of text with stateful multi-line block comment tracking.
pub fn tokenize_line(text: &str, ext: &str, in_block_comment: &mut bool) -> Vec<SyntaxToken> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return tokens;
    }

    let lang = detect_lang(ext);

    // Fast path: Diff / Patch
    if lang == LangKind::Diff {
        if text.starts_with("--- ")
            || text.starts_with("+++ ")
            || text.starts_with("@@")
            || text.starts_with("diff ")
            || text.starts_with("index ")
        {
            tokens.push(SyntaxToken {
                kind: TokenKind::DiffHeader,
                range: 0..len,
            });
            return tokens;
        } else if bytes[0] == b'+' {
            tokens.push(SyntaxToken {
                kind: TokenKind::DiffAdd,
                range: 0..len,
            });
            return tokens;
        } else if bytes[0] == b'-' {
            tokens.push(SyntaxToken {
                kind: TokenKind::DiffRemove,
                range: 0..len,
            });
            return tokens;
        }
    }

    // Fast path: Markdown line features
    if lang == LangKind::Markdown {
        let trimmed_start = text.trim_start();
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            tokens.push(SyntaxToken {
                kind: TokenKind::Preprocessor,
                range: 0..len,
            });
            return tokens;
        } else if trimmed_start.starts_with('#') {
            tokens.push(SyntaxToken {
                kind: TokenKind::Keyword,
                range: 0..len,
            });
            return tokens;
        } else if trimmed_start.starts_with('>') {
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: 0..len,
            });
            return tokens;
        }
    }

    let mut i = 0;

    // Check if continuing inside a multi-line block comment
    if *in_block_comment {
        let (close_pat, close_len) = match lang {
            LangKind::Html => ("-->", 3),
            LangKind::Lua => ("]]", 2),
            _ => ("*/", 2),
        };
        if let Some(close_idx) = text.find(close_pat) {
            let end = close_idx + close_len;
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: 0..end,
            });
            *in_block_comment = false;
            i = end;
        } else {
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: 0..len,
            });
            return tokens;
        }
    }

    while i < len {
        let b = bytes[i];

        // 1. Whitespace
        if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
            let start = i;
            while i < len
                && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r' || bytes[i] == b'\n')
            {
                i += 1;
            }
            tokens.push(SyntaxToken {
                kind: TokenKind::Whitespace,
                range: start..i,
            });
            continue;
        }

        // 2. Block Comments: HTML <!-- ... -->
        if lang == LangKind::Html && text[i..].starts_with("<!--") {
            let start = i;
            if let Some(close_idx) = text[i + 4..].find("-->") {
                let end = i + 4 + close_idx + 3;
                tokens.push(SyntaxToken {
                    kind: TokenKind::Comment,
                    range: start..end,
                });
                i = end;
            } else {
                tokens.push(SyntaxToken {
                    kind: TokenKind::Comment,
                    range: start..len,
                });
                *in_block_comment = true;
                break;
            }
            continue;
        }

        // 2b. Block Comments: Lua --[[ ... ]]
        if lang == LangKind::Lua && text[i..].starts_with("--[[") {
            let start = i;
            if let Some(close_idx) = text[i + 4..].find("]]") {
                let end = i + 4 + close_idx + 2;
                tokens.push(SyntaxToken {
                    kind: TokenKind::Comment,
                    range: start..end,
                });
                i = end;
            } else {
                tokens.push(SyntaxToken {
                    kind: TokenKind::Comment,
                    range: start..len,
                });
                *in_block_comment = true;
                break;
            }
            continue;
        }

        // 2c. Multi-line Block Comments /* ... */
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            if let Some(close_idx) = text[i + 2..].find("*/") {
                let end = i + 2 + close_idx + 2;
                tokens.push(SyntaxToken {
                    kind: TokenKind::Comment,
                    range: start..end,
                });
                i = end;
            } else {
                tokens.push(SyntaxToken {
                    kind: TokenKind::Comment,
                    range: start..len,
                });
                *in_block_comment = true;
                break;
            }
            continue;
        }

        // 3. Line Comments
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: i..len,
            });
            break;
        }
        if matches!(
            lang,
            LangKind::Python
                | LangKind::Shell
                | LangKind::Config
                | LangKind::Docker
                | LangKind::Ruby
                | LangKind::Php
        ) && b == b'#'
        {
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: i..len,
            });
            break;
        }
        if (lang == LangKind::Sql || lang == LangKind::Lua)
            && b == b'-'
            && i + 1 < len
            && bytes[i + 1] == b'-'
        {
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: i..len,
            });
            break;
        }
        if lang == LangKind::Config && b == b';' {
            tokens.push(SyntaxToken {
                kind: TokenKind::Comment,
                range: i..len,
            });
            break;
        }

        // 4. HTML / XML doctype, tags, entities
        if lang == LangKind::Html {
            if text[i..].starts_with("<!") {
                let start = i;
                while i < len && bytes[i] != b'>' {
                    i += 1;
                }
                if i < len && bytes[i] == b'>' {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Preprocessor,
                    range: start..i,
                });
                continue;
            }
            if text[i..].starts_with("</") {
                let start = i;
                i += 2;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'-'
                        || bytes[i] == b'_'
                        || bytes[i] == b':')
                {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Keyword,
                    range: start..i,
                });
                continue;
            }
            if b == b'<'
                && i + 1 < len
                && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
            {
                let start = i;
                i += 1;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'-'
                        || bytes[i] == b'_'
                        || bytes[i] == b':')
                {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Keyword,
                    range: start..i,
                });
                continue;
            }
            if text[i..].starts_with("/>") {
                tokens.push(SyntaxToken {
                    kind: TokenKind::Keyword,
                    range: i..i + 2,
                });
                i += 2;
                continue;
            }
            if b == b'>' {
                tokens.push(SyntaxToken {
                    kind: TokenKind::Keyword,
                    range: i..i + 1,
                });
                i += 1;
                continue;
            }
            if b == b'&' {
                let start = i;
                i += 1;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'#') {
                    i += 1;
                }
                if i < len && bytes[i] == b';' {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Preprocessor,
                    range: start..i,
                });
                continue;
            }
        }

        // 5. JSX / TSX Tag Recognition (<Component, </Component>, <div, etc.)
        if (lang == LangKind::Js || lang == LangKind::Ts) && b == b'<' && i + 1 < len {
            if text[i..].starts_with("</") {
                let start = i;
                i += 2;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'-')
                {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Type,
                    range: start..i,
                });
                continue;
            } else if text[i..].starts_with("<>") {
                tokens.push(SyntaxToken {
                    kind: TokenKind::Type,
                    range: i..i + 2,
                });
                i += 2;
                continue;
            } else if bytes[i + 1].is_ascii_uppercase() {
                let start = i;
                i += 1;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'-')
                {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Type,
                    range: start..i,
                });
                continue;
            } else if bytes[i + 1].is_ascii_lowercase() {
                let tag_start = i + 1;
                let mut tag_end = tag_start;
                while tag_end < len && bytes[tag_end].is_ascii_alphabetic() {
                    tag_end += 1;
                }
                let tag = &text[tag_start..tag_end];
                if HTML_TAGS.contains(&tag) {
                    let start = i;
                    i = tag_end;
                    tokens.push(SyntaxToken {
                        kind: TokenKind::Keyword,
                        range: start..i,
                    });
                    continue;
                }
            }
        }
        if (lang == LangKind::Js || lang == LangKind::Ts) && text[i..].starts_with("/>") {
            tokens.push(SyntaxToken {
                kind: TokenKind::Type,
                range: i..i + 2,
            });
            i += 2;
            continue;
        }

        // 6. CSS Selectors and At-rules
        if lang == LangKind::Css {
            if b == b'@' {
                let start = i;
                i += 1;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Preprocessor,
                    range: start..i,
                });
                continue;
            }
            if b == b'.'
                && i + 1 < len
                && (bytes[i + 1].is_ascii_alphabetic()
                    || bytes[i + 1] == b'-'
                    || bytes[i + 1] == b'_')
            {
                let start = i;
                i += 1;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
                {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Type,
                    range: start..i,
                });
                continue;
            }
            if b == b'#' {
                let start = i;
                i += 1;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
                {
                    i += 1;
                }
                let hex = &text[start + 1..i];
                let is_hex = (hex.len() == 3 || hex.len() == 4 || hex.len() == 6 || hex.len() == 8)
                    && hex.bytes().all(|c| c.is_ascii_hexdigit());
                tokens.push(SyntaxToken {
                    kind: if is_hex {
                        TokenKind::Number
                    } else {
                        TokenKind::Type
                    },
                    range: start..i,
                });
                continue;
            }
            if b == b':'
                && i + 1 < len
                && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b':')
            {
                let start = i;
                while i < len && bytes[i] == b':' {
                    i += 1;
                }
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                    i += 1;
                }
                tokens.push(SyntaxToken {
                    kind: TokenKind::Keyword,
                    range: start..i,
                });
                continue;
            }
        }

        // 7. Preprocessors & Attributes: C/C++/C# directives or Rust attributes
        if (lang == LangKind::Cpp || lang == LangKind::CSharp) && b == b'#' {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' || bytes[i] == b'#')
            {
                i += 1;
            }
            tokens.push(SyntaxToken {
                kind: TokenKind::Preprocessor,
                range: start..i,
            });
            continue;
        }
        if lang == LangKind::Rust
            && b == b'#'
            && i + 1 < len
            && (bytes[i + 1] == b'[' || bytes[i + 1] == b'!')
        {
            let start = i;
            while i < len && bytes[i] != b']' {
                i += 1;
            }
            if i < len && bytes[i] == b']' {
                i += 1;
            }
            tokens.push(SyntaxToken {
                kind: TokenKind::Preprocessor,
                range: start..i,
            });
            continue;
        }

        // 8. Decorators / Annotations / Zig builtins (@name)
        if matches!(
            lang,
            LangKind::Python
                | LangKind::Java
                | LangKind::Dart
                | LangKind::Ts
                | LangKind::Js
                | LangKind::Zig
        ) && b == b'@'
            && i + 1 < len
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
        {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            tokens.push(SyntaxToken {
                kind: if lang == LangKind::Zig {
                    TokenKind::Keyword
                } else {
                    TokenKind::Preprocessor
                },
                range: start..i,
            });
            continue;
        }

        // 9. Variables: PHP, Shell, Makefile ($var or ${var})
        if matches!(lang, LangKind::Php | LangKind::Shell | LangKind::Docker)
            && b == b'$'
            && i + 1 < len
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_' || bytes[i + 1] == b'{')
        {
            let start = i;
            i += 1;
            if i < len && bytes[i] == b'{' {
                while i < len && bytes[i] != b'}' {
                    i += 1;
                }
                if i < len && bytes[i] == b'}' {
                    i += 1;
                }
            } else {
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            tokens.push(SyntaxToken {
                kind: TokenKind::Identifier,
                range: start..i,
            });
            continue;
        }

        // 10. Strings: "...", '...', and `...`
        let is_backtick_lang = matches!(
            lang,
            LangKind::Js | LangKind::Ts | LangKind::Go | LangKind::Markdown | LangKind::Sql
        );
        if b == b'"' || b == b'\'' || (is_backtick_lang && b == b'`') {
            let quote = b;
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            let kind = if lang == LangKind::Json {
                let mut peek = i;
                while peek < len
                    && (bytes[peek] == b' '
                        || bytes[peek] == b'\t'
                        || bytes[peek] == b'\r'
                        || bytes[peek] == b'\n')
                {
                    peek += 1;
                }
                if peek < len && bytes[peek] == b':' {
                    TokenKind::Type
                } else {
                    TokenKind::String
                }
            } else {
                TokenKind::String
            };
            tokens.push(SyntaxToken {
                kind,
                range: start..i,
            });
            continue;
        }

        // 11. Numbers
        if b.is_ascii_digit() || (b == b'.' && i + 1 < len && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            if b == b'0' && i + 1 < len && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
                i += 2;
                while i < len && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                    i += 1;
                }
            } else if b == b'0' && i + 1 < len && (bytes[i + 1] == b'b' || bytes[i + 1] == b'B') {
                i += 2;
                while i < len && (bytes[i] == b'0' || bytes[i] == b'1' || bytes[i] == b'_') {
                    i += 1;
                }
            } else if b == b'0' && i + 1 < len && (bytes[i + 1] == b'o' || bytes[i + 1] == b'O') {
                i += 2;
                while i < len && ((bytes[i] >= b'0' && bytes[i] <= b'7') || bytes[i] == b'_') {
                    i += 1;
                }
            } else {
                while i < len
                    && (bytes[i].is_ascii_digit()
                        || bytes[i] == b'.'
                        || bytes[i] == b'e'
                        || bytes[i] == b'E'
                        || bytes[i] == b'f'
                        || bytes[i] == b'u'
                        || bytes[i] == b'l'
                        || bytes[i] == b'L'
                        || bytes[i] == b'_'
                        || (lang == LangKind::Css && bytes[i] == b'%'))
                {
                    i += 1;
                }
                if lang == LangKind::Css {
                    while i < len && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                }
            }
            tokens.push(SyntaxToken {
                kind: TokenKind::Number,
                range: start..i,
            });
            continue;
        }

        // 12. Identifiers / Keywords / Types
        if b.is_ascii_alphabetic() || b == b'_' || (lang == LangKind::Css && b == b'-') {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || (lang == LangKind::Css && bytes[i] == b'-'))
            {
                i += 1;
            }
            let word = &text[start..i];

            // Rust macros (println!, vec!, format!)
            if lang == LangKind::Rust
                && i < len
                && bytes[i] == b'!'
                && (i + 1 >= len || bytes[i + 1] != b'=')
            {
                i += 1;
                tokens.push(SyntaxToken {
                    kind: TokenKind::Preprocessor,
                    range: start..i,
                });
                continue;
            }

            let mut kind = TokenKind::Identifier;

            if lang == LangKind::Html {
                let mut peek = i;
                while peek < len && (bytes[peek] == b' ' || bytes[peek] == b'\t') {
                    peek += 1;
                }
                if peek < len && bytes[peek] == b'=' {
                    kind = TokenKind::Type;
                }
            } else if lang == LangKind::Css {
                let lower_word = word.to_ascii_lowercase();
                let mut peek = i;
                while peek < len && (bytes[peek] == b' ' || bytes[peek] == b'\t') {
                    peek += 1;
                }
                let is_property = peek < len && bytes[peek] == b':';
                if word.starts_with("--") {
                    kind = TokenKind::Type;
                } else if is_property
                    || (CSS_PROPERTIES.contains(&lower_word.as_str())
                        && !CSS_KEYWORDS.contains(&lower_word.as_str()))
                {
                    kind = TokenKind::Keyword;
                } else if CSS_KEYWORDS.contains(&lower_word.as_str()) {
                    kind = TokenKind::Type;
                }
            } else if lang == LangKind::Config {
                let mut peek = i;
                while peek < len && (bytes[peek] == b' ' || bytes[peek] == b'\t') {
                    peek += 1;
                }
                if peek < len && (bytes[peek] == b':' || bytes[peek] == b'=') {
                    kind = TokenKind::Keyword;
                } else if word == "true"
                    || word == "false"
                    || word == "yes"
                    || word == "no"
                    || word == "null"
                {
                    kind = TokenKind::Keyword;
                }
            } else {
                // PascalCase custom types / components heuristic
                if is_pascal_case(word)
                    && matches!(
                        lang,
                        LangKind::Js
                            | LangKind::Ts
                            | LangKind::Rust
                            | LangKind::Go
                            | LangKind::Java
                            | LangKind::CSharp
                            | LangKind::Swift
                            | LangKind::Dart
                            | LangKind::Python
                            | LangKind::Cpp
                    )
                {
                    kind = TokenKind::Type;
                }

                match lang {
                    LangKind::Cpp => {
                        if C_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if C_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Js => {
                        if JS_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if TS_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Ts => {
                        if JS_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if TS_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Python => {
                        if PY_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if PY_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Rust => {
                        if RUST_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if RUST_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Zig => {
                        if ZIG_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if ZIG_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Go => {
                        if GO_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if GO_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Java => {
                        if JAVA_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if JAVA_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::CSharp => {
                        if CSHARP_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if CSHARP_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Php => {
                        if PHP_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        }
                    }
                    LangKind::Ruby => {
                        if RUBY_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        }
                    }
                    LangKind::Swift => {
                        if SWIFT_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if SWIFT_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Lua => {
                        if LUA_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        }
                    }
                    LangKind::Dart => {
                        if DART_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        } else if DART_TYPES.contains(&word) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Sql => {
                        let lower_word = word.to_ascii_lowercase();
                        if SQL_KEYWORDS.contains(&lower_word.as_str()) {
                            kind = TokenKind::Keyword;
                        } else if SQL_TYPES.contains(&lower_word.as_str()) {
                            kind = TokenKind::Type;
                        }
                    }
                    LangKind::Shell => {
                        if SHELL_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        }
                    }
                    LangKind::Docker => {
                        if DOCKER_KEYWORDS.contains(&word) {
                            kind = TokenKind::Keyword;
                        }
                    }
                    LangKind::Json => {
                        if word == "true" || word == "false" || word == "null" {
                            kind = TokenKind::Keyword;
                        }
                    }
                    LangKind::Log => {
                        if word == "ERROR" || word == "FATAL" || word == "FAIL" {
                            kind = TokenKind::LogError;
                        } else if word == "WARN" || word == "WARNING" {
                            kind = TokenKind::LogWarn;
                        } else if word == "INFO" || word == "DEBUG" || word == "TRACE" {
                            kind = TokenKind::LogInfo;
                        }
                    }
                    _ => {}
                }
            }

            tokens.push(SyntaxToken {
                kind,
                range: start..i,
            });
            continue;
        }

        // 13. Symbols / Operators
        // Non-ASCII text also reaches this fallback. A token must consume a
        // whole UTF-8 character, since callers slice the original string.
        let start = i;
        i += 1;
        while i < len && !text.is_char_boundary(i) {
            i += 1;
        }
        tokens.push(SyntaxToken {
            kind: TokenKind::Symbol,
            range: start..i,
        });
    }

    tokens
}

/// Tokenize an entire document into lines of (TokenKind, text slice)
pub fn tokenize_document<'a>(text: &'a str, ext: &str) -> Vec<Vec<(TokenKind, &'a str)>> {
    let mut in_block_comment = false;
    text.lines()
        .map(|line| {
            let tokens = tokenize_line(line, ext, &mut in_block_comment);
            tokens
                .into_iter()
                .map(|t| (t.kind, &line[t.range]))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_tokens_preserve_utf8_boundaries_and_text() {
        let source = "café 日本語 e\u{301} 🧑🏽‍💻 → Ω\nplain ñ";
        for ext in [
            "txt", "html", "xml", "tsx", "js", "rs", "py", "css", "json", "lua", "diff", "log",
            "md",
        ] {
            let lines = tokenize_document(source, ext);
            let reconstructed = lines
                .iter()
                .map(|line| line.iter().map(|(_, value)| *value).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(reconstructed, source, "{ext}");
        }
    }

    #[test]
    fn unicode_near_delimiters_and_escapes_preserves_ranges() {
        for source in [
            "é<!-- ñ -->🙂",
            "<div>ñ</div>",
            "'\\é' Ω",
            "/* ñ */好",
            "${ñ}é",
            "<Componente>💻</Componente>",
            "# título",
            "é\"日本\"ñ",
        ] {
            for ext in ["txt", "html", "tsx", "css", "sh", "lua", "rs"] {
                let mut comment = false;
                let tokens = tokenize_line(source, ext, &mut comment);
                let mut end = 0;
                for token in tokens {
                    assert_eq!(token.range.start, end, "{source:?}, {ext}");
                    assert!(
                        source.get(token.range.clone()).is_some(),
                        "invalid boundary: {token:?}, {source:?}, {ext}"
                    );
                    end = token.range.end;
                }
                assert_eq!(end, source.len());
            }
        }
    }

    #[test]
    fn test_rust_tokens() {
        let code = "fn main() {\n    let x: u32 = 42;\n    println!(\"hello\"); // comment\n}";
        let lines = tokenize_document(code, "rs");
        assert_eq!(lines.len(), 4);

        let kinds_0: Vec<TokenKind> = lines[0].iter().map(|(k, _)| *k).collect();
        assert!(kinds_0.contains(&TokenKind::Keyword)); // "fn"
        assert!(kinds_0.contains(&TokenKind::Identifier)); // "main"

        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::Keyword)); // "let"
        assert!(kinds_1.contains(&TokenKind::Type)); // "u32"
        assert!(kinds_1.contains(&TokenKind::Number)); // "42"

        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Preprocessor)); // "println!"
        assert!(kinds_2.contains(&TokenKind::String)); // "\"hello\""
        assert!(kinds_2.contains(&TokenKind::Comment)); // "// comment"
    }

    #[test]
    fn test_typescript_tokens() {
        let code = "interface UserProfile {\n    id: string;\n    count: number;\n    active: boolean;\n}\nconst user: UserProfile = { id: \"1\", count: 42, active: true };";
        let lines = tokenize_document(code, "ts");
        assert_eq!(lines.len(), 6);

        // Line 0: interface UserProfile {
        let kinds_0: Vec<TokenKind> = lines[0].iter().map(|(k, _)| *k).collect();
        assert!(kinds_0.contains(&TokenKind::Keyword)); // "interface"
        assert!(kinds_0.contains(&TokenKind::Type)); // "UserProfile" (PascalCase)

        // Line 1: id: string;
        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::Identifier)); // "id"
        assert!(kinds_1.contains(&TokenKind::Type)); // "string"

        // Line 5: const user: UserProfile ...
        let kinds_5: Vec<TokenKind> = lines[5].iter().map(|(k, _)| *k).collect();
        assert!(kinds_5.contains(&TokenKind::Keyword)); // "const"
        assert!(kinds_5.contains(&TokenKind::Type)); // "UserProfile"
        assert!(kinds_5.contains(&TokenKind::String)); // "\"1\""
        assert!(kinds_5.contains(&TokenKind::Number)); // "42"
        assert!(kinds_5.contains(&TokenKind::Keyword)); // "true"
    }

    #[test]
    fn test_tsx_tokens() {
        let code = "export const Header = () => {\n    return (\n        <div className=\"header\">\n            <Button onClick={handleClick}>Click</Button>\n        </div>\n    );\n};";
        let lines = tokenize_document(code, "tsx");
        assert_eq!(lines.len(), 7);

        // Line 2: <div className="header">
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Keyword)); // "<div" (HTML tag)
        assert!(kinds_2.contains(&TokenKind::String)); // "\"header\""

        // Line 3: <Button onClick={handleClick}>Click</Button>
        let kinds_3: Vec<TokenKind> = lines[3].iter().map(|(k, _)| *k).collect();
        assert!(kinds_3.contains(&TokenKind::Type)); // "<Button" (JSX Component)
        assert!(kinds_3.contains(&TokenKind::Type)); // "</Button>" (closing tag)
    }

    #[test]
    fn test_html_tokens() {
        let code = "<!DOCTYPE html>\n<html lang=\"en\">\n<!-- Title comment -->\n<body class=\"main\">\n    <h1>Hello &amp; welcome</h1>\n</body>\n</html>";
        let lines = tokenize_document(code, "html");
        assert_eq!(lines.len(), 7);

        // Line 0: <!DOCTYPE html>
        let kinds_0: Vec<TokenKind> = lines[0].iter().map(|(k, _)| *k).collect();
        assert!(kinds_0.contains(&TokenKind::Preprocessor));

        // Line 1: <html lang="en">
        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::Keyword)); // "<html"
        assert!(kinds_1.contains(&TokenKind::Type)); // "lang" (attr before '=')
        assert!(kinds_1.contains(&TokenKind::String)); // "\"en\""

        // Line 2: <!-- Title comment -->
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Comment));

        // Line 4: <h1>Hello &amp; welcome</h1>
        let kinds_4: Vec<TokenKind> = lines[4].iter().map(|(k, _)| *k).collect();
        assert!(kinds_4.contains(&TokenKind::Keyword)); // "<h1"
        assert!(kinds_4.contains(&TokenKind::Preprocessor)); // "&amp;"
        assert!(kinds_4.contains(&TokenKind::Keyword)); // "</h1>"
    }

    #[test]
    fn test_html_multiline_comment() {
        let code = "<!--\n  Multi line\n-->\n<div />";
        let lines = tokenize_document(code, "html");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0][0].0, TokenKind::Comment);
        assert_eq!(lines[1][0].0, TokenKind::Comment);
        assert_eq!(lines[2][0].0, TokenKind::Comment);
        let kinds_3: Vec<TokenKind> = lines[3].iter().map(|(k, _)| *k).collect();
        assert!(kinds_3.contains(&TokenKind::Keyword)); // "<div"
    }

    #[test]
    fn test_css_tokens() {
        let code = "/* Reset */\n:root {\n    --main-bg: #1e1e1e;\n}\n.container {\n    display: flex;\n    max-width: 1200px;\n}";
        let lines = tokenize_document(code, "css");
        assert_eq!(lines.len(), 8);

        // Line 0: /* Reset */
        assert_eq!(lines[0][0].0, TokenKind::Comment);

        // Line 1: :root
        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::Keyword)); // ":root"

        // Line 2: --main-bg: #1e1e1e;
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Type)); // "--main-bg"
        assert!(kinds_2.contains(&TokenKind::Number)); // "#1e1e1e"

        // Line 4: .container {
        let kinds_4: Vec<TokenKind> = lines[4].iter().map(|(k, _)| *k).collect();
        assert!(kinds_4.contains(&TokenKind::Type)); // ".container"

        // Line 5: display: flex;
        let kinds_5: Vec<TokenKind> = lines[5].iter().map(|(k, _)| *k).collect();
        assert!(kinds_5.contains(&TokenKind::Keyword)); // "display"
        assert!(kinds_5.contains(&TokenKind::Type)); // "flex"

        // Line 6: max-width: 1200px;
        let kinds_6: Vec<TokenKind> = lines[6].iter().map(|(k, _)| *k).collect();
        assert!(kinds_6.contains(&TokenKind::Keyword)); // "max-width"
        assert!(kinds_6.contains(&TokenKind::Number)); // "1200px"
    }

    #[test]
    fn test_json_tokens() {
        let code = "{\n    \"name\": \"atlas\",\n    \"version\": 10,\n    \"active\": true,\n    \"data\": null\n}";
        let lines = tokenize_document(code, "json");
        assert_eq!(lines.len(), 6);

        // Line 1: "name": "atlas"
        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::Type)); // key "name"
        assert!(kinds_1.contains(&TokenKind::String)); // value "atlas"

        // Line 2: "version": 10
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Type)); // key "version"
        assert!(kinds_2.contains(&TokenKind::Number)); // 10

        // Line 3: "active": true
        let kinds_3: Vec<TokenKind> = lines[3].iter().map(|(k, _)| *k).collect();
        assert!(kinds_3.contains(&TokenKind::Type)); // key "active"
        assert!(kinds_3.contains(&TokenKind::Keyword)); // true
    }

    #[test]
    fn test_c_multiline_comment() {
        let code = "/* start\n   middle\n   end */ int x = 1;";
        let lines = tokenize_document(code, "c");
        assert_eq!(lines.len(), 3);

        assert_eq!(lines[0][0].0, TokenKind::Comment);
        assert_eq!(lines[1][0].0, TokenKind::Comment);
        assert_eq!(lines[2][0].0, TokenKind::Comment);
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Type)); // "int"
        assert!(kinds_2.contains(&TokenKind::Number)); // "1"
    }

    #[test]
    fn test_python_tokens() {
        let code = "def hello():\n    # greeting\n    return 'world'";
        let lines = tokenize_document(code, "py");
        assert_eq!(lines.len(), 3);
        let kinds_0: Vec<TokenKind> = lines[0].iter().map(|(k, _)| *k).collect();
        assert!(kinds_0.contains(&TokenKind::Keyword)); // "def"
        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::Comment)); // "# greeting"
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::Keyword)); // "return"
        assert!(kinds_2.contains(&TokenKind::String)); // "'world'"
    }

    #[test]
    fn test_docker_tokens() {
        let code = "FROM rust:alpine\nWORKDIR /app\nCOPY . .\nRUN cargo build";
        let lines = tokenize_document(code, "dockerfile");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0][0].0, TokenKind::Keyword); // FROM
        assert_eq!(lines[1][0].0, TokenKind::Keyword); // WORKDIR
        assert_eq!(lines[2][0].0, TokenKind::Keyword); // COPY
        assert_eq!(lines[3][0].0, TokenKind::Keyword); // RUN
    }

    #[test]
    fn test_markdown_tokens() {
        let code = "# Title\n```rust\nfn main() {}\n```\n> Blockquote\nRegular text";
        let lines = tokenize_document(code, "md");
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0][0].0, TokenKind::Keyword); // Header
        assert_eq!(lines[1][0].0, TokenKind::Preprocessor); // Code fence start
        assert_eq!(lines[3][0].0, TokenKind::Preprocessor); // Code fence end
        assert_eq!(lines[4][0].0, TokenKind::Comment); // Blockquote
    }

    #[test]
    fn test_diff_tokens() {
        let diff = "--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n-old\n+new";
        let lines = tokenize_document(diff, "diff");
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0][0].0, TokenKind::DiffHeader);
        assert_eq!(lines[1][0].0, TokenKind::DiffHeader);
        assert_eq!(lines[2][0].0, TokenKind::DiffHeader);
        assert_eq!(lines[3][0].0, TokenKind::DiffRemove);
        assert_eq!(lines[4][0].0, TokenKind::DiffAdd);
    }

    #[test]
    fn test_log_tokens() {
        let log = "[ERROR] Connection failed\n[WARN] High memory\n[INFO] Starting up";
        let lines = tokenize_document(log, "log");
        assert_eq!(lines.len(), 3);
        let kinds_0: Vec<TokenKind> = lines[0].iter().map(|(k, _)| *k).collect();
        assert!(kinds_0.contains(&TokenKind::LogError));
        let kinds_1: Vec<TokenKind> = lines[1].iter().map(|(k, _)| *k).collect();
        assert!(kinds_1.contains(&TokenKind::LogWarn));
        let kinds_2: Vec<TokenKind> = lines[2].iter().map(|(k, _)| *k).collect();
        assert!(kinds_2.contains(&TokenKind::LogInfo));
    }

    /// Every short mix of syntax-significant ASCII and multi-byte chars must
    /// tokenize without panicking and yield char-boundary ranges.
    #[test]
    fn exhaustive_short_inputs_never_split_utf8() {
        const ALPHABET: &[&str] = &[
            "<", ">", "!", "-", "/", "*", "#", "[", "]", "\\", "\"", "'", "`", "$", "{", "@", "&",
            ";", "0", "x", "a", " ", "é", "日", "🧑", "\u{301}",
        ];
        const EXTS: &[&str] = &[
            "c", "js", "tsx", "html", "css", "json", "py", "rs", "zig", "go", "java", "cs", "php",
            "rb", "swift", "lua", "dart", "sql", "sh", "toml", "md", "diff", "log",
        ];
        let n = ALPHABET.len();
        let mut line = String::new();
        for ext in EXTS {
            for a in 0..n {
                for b in 0..n {
                    for c in 0..n {
                        line.clear();
                        line.push_str(ALPHABET[a]);
                        line.push_str(ALPHABET[b]);
                        line.push_str(ALPHABET[c]);
                        line.push_str(ALPHABET[(a + b + c) % n]);
                        for open in [false, true] {
                            let mut in_block = open;
                            for token in tokenize_line(&line, ext, &mut in_block) {
                                assert!(
                                    line.get(token.range.clone()).is_some(),
                                    "{ext}: range {:?} splits {line:?}",
                                    token.range
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
