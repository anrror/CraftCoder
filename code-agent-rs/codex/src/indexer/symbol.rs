use std::ops::Range;

/// 符号条目（实体）
///
/// 【领域含义】从源代码中提取的代码符号实体，是"代码解析与索引"限界上下文的核心实体。
/// 包含符号名称、类型、文件位置（行/列范围）、文档注释、签名等完整信息。
/// 每个 SymbolEntry 唯一标识源代码中的一个具名声明（函数、类、结构体等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolEntry {
    /// The name of the symbol (function name, class name, etc.)
    pub symbol_name: String,
    /// The kind of symbol (function, class, etc.)
    pub symbol_kind: SymbolKind,
    /// Path to the source file containing this symbol
    pub file_path: String,
    /// 1-based line range (start..end)
    pub line_range: Range<usize>,
    /// 1-based column range (start..end)
    pub column_range: Range<usize>,
    /// Documentation comment associated with the symbol, if any
    pub doc_comment: Option<String>,
    /// Programming language of the source file
    pub language: String,
    /// Function/method signature including parameters and return type
    pub signature: Option<String>,
}

/// 符号类型（值对象）
///
/// 【领域含义】代码符号的分类枚举，涵盖函数、方法、类、结构体、枚举、接口、
/// Trait、模块、变量、常量、导入、类型别名和宏。用于区分不同种类的代码声明，
/// 影响检索权重和图谱构建策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Module,
    Variable,
    Constant,
    Import,
    TypeAlias,
    Macro,
}

impl SymbolKind {
    /// 转换为字符串表示
    ///
    /// 【领域含义】将符号类型枚举值转换为持久化友好的字符串形式，用于 SQLite 存储和序列化。
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Module => "module",
            SymbolKind::Variable => "variable",
            SymbolKind::Constant => "constant",
            SymbolKind::Import => "import",
            SymbolKind::TypeAlias => "type_alias",
            SymbolKind::Macro => "macro",
        }
    }

    /// 从字符串解析符号类型
    ///
    /// 【领域含义】将持久化存储的字符串形式反序列化为 SymbolKind 枚举值。
    /// 用于从 SQLite 读取符号条目时的类型还原。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(SymbolKind::Function),
            "method" => Some(SymbolKind::Method),
            "class" => Some(SymbolKind::Class),
            "struct" => Some(SymbolKind::Struct),
            "enum" => Some(SymbolKind::Enum),
            "interface" => Some(SymbolKind::Interface),
            "trait" => Some(SymbolKind::Trait),
            "module" => Some(SymbolKind::Module),
            "variable" => Some(SymbolKind::Variable),
            "constant" => Some(SymbolKind::Constant),
            "import" => Some(SymbolKind::Import),
            "type_alias" => Some(SymbolKind::TypeAlias),
            "macro" => Some(SymbolKind::Macro),
            _ => None,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
