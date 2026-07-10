use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Node, Parser};

use super::symbol::{SymbolEntry, SymbolKind};

/// 语言映射表
///
/// 【领域含义】将人类可读的语言名称（如 "python"、"rust"）映射到 tree-sitter Language 实例。
/// 是 CodeParser 内部维护的语言支持注册表，决定了哪些语言可以被解析。
pub type LanguageMap = HashMap<String, tree_sitter::Language>;

/// 代码解析器（领域服务）
///
/// 【领域含义】基于 tree-sitter 的多语言 AST 解析器，是"代码解析与索引"限界上下文的
/// 领域服务。负责将源代码解析为抽象语法树，并从中提取符号声明（函数、类、结构体等）。
/// 当前支持 Python、TypeScript、Rust、Go、Java 五种语言。
pub struct CodeParser {
    /// Maps file extensions to language names (e.g., "py" → "python")
    extension_map: HashMap<String, String>,
    /// Maps language names to tree-sitter Language instances
    language_map: LanguageMap,
}

impl CodeParser {
    /// 创建代码解析器
    ///
    /// 【领域含义】工厂方法，创建预配置了 Python、TypeScript、Rust、Go、Java
    /// 五种语言支持的 CodeParser 实例。内部初始化语言映射表和文件扩展名映射表。
    pub fn new() -> Self {
        let mut language_map: LanguageMap = HashMap::new();
        language_map.insert("python".to_string(), tree_sitter_python::LANGUAGE.into());
        language_map.insert(
            "typescript".to_string(),
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        );
        language_map.insert(
            "tsx".to_string(),
            tree_sitter_typescript::LANGUAGE_TSX.into(),
        );
        language_map.insert("rust".to_string(), tree_sitter_rust::LANGUAGE.into());
        language_map.insert("go".to_string(), tree_sitter_go::LANGUAGE.into());
        language_map.insert("java".to_string(), tree_sitter_java::LANGUAGE.into());

        let mut extension_map = HashMap::new();
        extension_map.insert("py".to_string(), "python".to_string());
        extension_map.insert("pyi".to_string(), "python".to_string());
        extension_map.insert("ts".to_string(), "typescript".to_string());
        extension_map.insert("tsx".to_string(), "tsx".to_string());
        extension_map.insert("rs".to_string(), "rust".to_string());
        extension_map.insert("go".to_string(), "go".to_string());
        extension_map.insert("java".to_string(), "java".to_string());

        Self {
            extension_map,
            language_map,
        }
    }

    /// 检测文件语言
    ///
    /// 【领域含义】根据文件扩展名检测编程语言。返回语言名称（如 "python"、"rust"），
    /// 如果不支持该扩展名则返回 None。用于索引器判断是否需要处理该文件。
    pub fn detect_language(&self, path: &Path) -> Option<&str> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| self.extension_map.get(ext))
            .map(|s| s.as_str())
    }

    /// 获取 tree-sitter Language 实例
    ///
    /// 【领域含义】根据语言名称获取对应的 tree-sitter Language 实例，
    /// 用于后续的 AST 解析操作。
    pub fn get_language(&self, name: &str) -> Option<&tree_sitter::Language> {
        self.language_map.get(name)
    }

    /// 解析源文件并提取符号
    ///
    /// 【领域含义】从文件系统读取源文件，通过 tree-sitter 解析 AST 并提取所有符号声明。
    /// 如果语言不支持或文件无法读取则返回错误。成功时返回符号条目列表。
    pub fn parse_file(
        &self,
        path: &Path,
    ) -> Result<Vec<SymbolEntry>, crate::indexer::IndexerError> {
        let language_name = self
            .detect_language(path)
            .ok_or_else(|| crate::indexer::IndexerError::UnsupportedLanguage {
                path: path.display().to_string(),
            })?;

        let ts_language = self
            .get_language(language_name)
            .ok_or_else(|| crate::indexer::IndexerError::UnsupportedLanguage {
                path: path.display().to_string(),
            })?;

        let source = std::fs::read_to_string(path).map_err(|e| {
            crate::indexer::IndexerError::FileRead {
                path: path.display().to_string(),
                source: e,
            }
        })?;

        self.parse_source(&source, language_name, ts_language, &path.display().to_string())
    }

    /// 从字符串解析源代码并提取符号
    ///
    /// 【领域含义】直接从字符串解析源代码（无需文件系统访问），通过 tree-sitter 解析 AST
    /// 并提取所有符号声明。用于测试和内存中代码分析场景。
    pub fn parse_source(
        &self,
        source: &str,
        language_name: &str,
        ts_language: &tree_sitter::Language,
        file_path: &str,
    ) -> Result<Vec<SymbolEntry>, crate::indexer::IndexerError> {
        let mut parser = Parser::new();
        parser
            .set_language(ts_language)
            .map_err(|e| crate::indexer::IndexerError::Parse {
                path: file_path.to_string(),
                message: format!("Failed to set language: {}", e),
            })?;

        let tree = parser.parse(source, None).ok_or_else(|| {
            crate::indexer::IndexerError::Parse {
                path: file_path.to_string(),
                message: "Parse returned None".to_string(),
            }
        })?;

        let mut symbols = Vec::new();
        let source_bytes = source.as_bytes();
        extract_symbols_from_node(
            tree.root_node(),
            source_bytes,
            language_name,
            file_path,
            None,
            &mut symbols,
        );
        Ok(symbols)
    }
}

impl Default for CodeParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Recursively walk the AST and extract symbols.
#[allow(clippy::too_many_arguments)]
fn extract_symbols_from_node(
    node: Node,
    source: &[u8],
    language: &str,
    file_path: &str,
    parent_kind: Option<&str>,
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();

    if let Some(sym) = try_extract_symbol(node, source, language, file_path, parent_kind) {
        symbols.push(sym);
        // Don't recurse into symbol nodes (avoid nested symbols like
        // class methods appearing as top-level functions)
        // However, for containers like class/module, we DO recurse into children.
        if !is_container_kind(kind) {
            return;
        }
    }

    // Recurse into children, passing current kind as parent context
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_symbols_from_node(child, source, language, file_path, Some(kind), symbols);
    }
}

/// Check if a node kind is a container that may contain child symbols.
fn is_container_kind(kind: &str) -> bool {
    matches!(
        kind,
        "program"
            | "source_file"
            | "module"
            | "class_definition"
            | "class_declaration"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "interface_declaration"
            | "type_declaration"
            | "block"
            | "declaration_list"
            | "body"
            | "class_body"
            | "enum_body"
            | "interface_body"
    )
}

/// Try to extract a symbol from a tree-sitter node.
/// Returns `None` if the node kind is not a recognized symbol type.
fn try_extract_symbol(
    node: Node,
    source: &[u8],
    language: &str,
    file_path: &str,
    parent_kind: Option<&str>,
) -> Option<SymbolEntry> {
    let kind = node.kind();
    let (sym_kind, name) = extract_name_and_kind(node, source, language, kind, parent_kind)?;

    let start = node.start_position();
    let end = node.end_position();

    // tree-sitter uses 0-based rows, we convert to 1-based for line_range
    let line_range = (start.row + 1)..(end.row + 1);
    let column_range = start.column..end.column;

    let doc_comment = extract_doc_comment(node, source, language);
    let signature = extract_signature(node, source, kind);

    Some(SymbolEntry {
        symbol_name: name,
        symbol_kind: sym_kind,
        file_path: file_path.to_string(),
        line_range,
        column_range,
        doc_comment,
        language: language.to_string(),
        signature,
    })
}

/// Extract the symbol name and kind from a node.
fn extract_name_and_kind(
    node: Node,
    source: &[u8],
    language: &str,
    kind: &str,
    parent_kind: Option<&str>,
) -> Option<(SymbolKind, String)> {
    match (language, kind) {
        // ── Python ──────────────────────────────────────────────
        ("python", "function_definition") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            let is_method = parent_kind
                .map(|pk| pk == "class_definition" || pk == "block")
                .unwrap_or(false);
            if is_method {
                Some((SymbolKind::Method, name))
            } else {
                Some((SymbolKind::Function, name))
            }
        }
        ("python", "class_definition") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Class, name))
        }
        ("python", "import_statement") => {
            let name = import_path_text(node, source);
            Some((SymbolKind::Import, name))
        }
        ("python", "import_from_statement") => {
            let name = import_path_text(node, source);
            Some((SymbolKind::Import, name))
        }

        // ── TypeScript / TSX ────────────────────────────────────
        ("typescript" | "tsx", "function_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Function, name))
        }
        ("typescript" | "tsx", "method_definition") => {
            let name = child_text_by_field(node, source, "name")?;
            Some((SymbolKind::Method, name))
        }
        ("typescript" | "tsx", "class_declaration") => {
            let name = child_text_by_kind(node, source, "type_identifier")
                .or_else(|| child_text_by_kind(node, source, "identifier"))?;
            Some((SymbolKind::Class, name))
        }
        ("typescript" | "tsx", "interface_declaration") => {
            let name = child_text_by_kind(node, source, "type_identifier")
                .or_else(|| child_text_by_kind(node, source, "identifier"))?;
            Some((SymbolKind::Interface, name))
        }
        ("typescript" | "tsx", "type_alias_declaration") => {
            let name = child_text_by_kind(node, source, "type_identifier")
                .or_else(|| child_text_by_kind(node, source, "identifier"))?;
            Some((SymbolKind::TypeAlias, name))
        }
        ("typescript" | "tsx", "enum_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Enum, name))
        }
        ("typescript" | "tsx", "import_statement") => {
            let name = child_text_by_kind(node, source, "string")
                .unwrap_or_else(|| "unknown".to_string());
            Some((SymbolKind::Import, name))
        }
        ("typescript" | "tsx", "export_statement") => {
            // Export is a wrapper; the actual symbol is extracted from children
            None
        }

        // ── Rust ────────────────────────────────────────────────
        ("rust", "function_item") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Function, name))
        }
        ("rust", "struct_item") => {
            let name = child_text_by_kind(node, source, "type_identifier")?;
            Some((SymbolKind::Struct, name))
        }
        ("rust", "enum_item") => {
            let name = child_text_by_kind(node, source, "type_identifier")?;
            Some((SymbolKind::Enum, name))
        }
        ("rust", "trait_item") => {
            let name = child_text_by_kind(node, source, "type_identifier")?;
            Some((SymbolKind::Trait, name))
        }
        ("rust", "impl_item") => {
            // impl block: extract type name, but recurse to get methods
            let name = child_text_by_kind(node, source, "type_identifier")
                .unwrap_or_else(|| "impl".to_string());
            Some((SymbolKind::Module, name))
        }
        ("rust", "use_declaration") => {
            let name = node_text(node, source).unwrap_or_else(|| "use".to_string());
            Some((SymbolKind::Import, name))
        }
        ("rust", "macro_definition") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Macro, name))
        }
        ("rust", "mod_item") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Module, name))
        }

        // ── Go ──────────────────────────────────────────────────
        ("go", "function_declaration") => {
            let name = child_text_by_field(node, source, "name")?;
            Some((SymbolKind::Function, name))
        }
        ("go", "method_declaration") => {
            let name = child_text_by_field(node, source, "name")?;
            Some((SymbolKind::Method, name))
        }
        ("go", "type_declaration") => {
            // Type declaration may contain struct/interface specs
            let name = child_text_by_kind(node, source, "type_identifier")
                .or_else(|| child_text_by_kind(node, source, "identifier"))?;

            // Determine if it's a struct or interface
            let body_kind = node
                .child_by_field_name("body")
                .or_else(|| node.child_by_field_name("type"))
                .map(|n| n.kind().to_string());

            let sym_kind = match body_kind.as_deref() {
                Some("struct_type") | Some("struct_spec") => SymbolKind::Struct,
                Some("interface_type") | Some("interface_spec") => SymbolKind::Interface,
                _ => SymbolKind::TypeAlias,
            };
            Some((sym_kind, name))
        }
        ("go", "import_declaration") => {
            let name = child_text_by_kind(node, source, "interpreted_string_literal")
                .unwrap_or_else(|| "import".to_string());
            Some((SymbolKind::Import, name))
        }
        ("go", "const_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")
                .unwrap_or_else(|| "const".to_string());
            Some((SymbolKind::Constant, name))
        }
        ("go", "var_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")
                .unwrap_or_else(|| "var".to_string());
            Some((SymbolKind::Variable, name))
        }

        // ── Java ────────────────────────────────────────────────
        ("java", "method_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Method, name))
        }
        ("java", "class_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Class, name))
        }
        ("java", "interface_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Interface, name))
        }
        ("java", "enum_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")?;
            Some((SymbolKind::Enum, name))
        }
        ("java", "import_declaration") => {
            let name = child_text_by_kind(node, source, "identifier")
                .or_else(|| child_text_by_kind(node, source, "scoped_identifier"))
                .unwrap_or_else(|| "import".to_string());
            Some((SymbolKind::Import, name))
        }

        _ => None,
    }
}

/// Get the text content of a child node with the given kind.
fn child_text_by_kind(node: Node, source: &[u8], kind: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return node_text(child, source);
        }
    }
    None
}

/// Get the text content of a child node with the given field name.
fn child_text_by_field(node: Node, source: &[u8], field: &str) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    node_text(child, source)
}

/// Get the source text for a node.
fn node_text(node: Node, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(|s| s.to_string())
}

/// Build a readable import path from import-related nodes.
fn import_path_text(node: Node, source: &[u8]) -> String {
    // Try to get the module path from dotted_name or similar
    if let Some(dn) = child_text_by_kind(node, source, "dotted_name") {
        return dn;
    }
    // Try aliased_import, import_spec, etc.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let text = node_text(child, source).unwrap_or_default();
        if !text.is_empty() && !text.starts_with("import") && !text.starts_with("from") {
            return text;
        }
    }
    "import".to_string()
}

/// Extract a documentation comment preceding the node.
fn extract_doc_comment(node: Node, source: &[u8], language: &str) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();

    // Find this node's position among siblings and check preceding siblings for comments
    let mut prev_node: Option<Node> = None;
    for child in parent.children(&mut cursor) {
        if child.id() == node.id() {
            break;
        }
        prev_node = Some(child);
    }

    let comment = prev_node?;

    match language {
        "python" => {
            // Python docstrings are string nodes, not comments
            // For function/class definitions, the first statement in body is the docstring
            if let Some(body) = node.child_by_field_name("body") {
                let first_stmt = body.child(0);
                if let Some(stmt) = first_stmt {
                    if stmt.kind() == "expression_statement" {
                        if let Some(expr) = stmt.child(0) {
                            if expr.kind() == "string" {
                                return node_text(expr, source);
                            }
                        }
                    }
                }
            }
            None
        }
        "rust" => {
            if comment.kind() == "line_comment" || comment.kind() == "block_comment" {
                let text = node_text(comment, source)?;
                if text.starts_with("///") || text.starts_with("/**") {
                    return Some(clean_doc_text(&text));
                }
            }
            None
        }
        "typescript" | "tsx" | "go" | "java" => {
            if comment.kind() == "comment" || comment.kind() == "line_comment" {
                let text = node_text(comment, source)?;
                if text.starts_with("//") || text.starts_with("/*") || text.starts_with("/**") {
                    return Some(clean_doc_text(&text));
                }
            }
            None
        }
        _ => None,
    }
}

/// Clean up doc comment text (strip comment markers).
fn clean_doc_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim();
            if let Some(stripped) = trimmed.strip_prefix("///") {
                stripped.trim()
            } else if let Some(stripped) = trimmed.strip_prefix("/**") {
                stripped.trim()
            } else if let Some(stripped) = trimmed.strip_prefix("**") {
                stripped.trim()
            } else if let Some(stripped) = trimmed.strip_prefix("//") {
                stripped.trim()
            } else if let Some(stripped) = trimmed.strip_prefix("/*") {
                stripped.trim()
            } else if let Some(stripped) = trimmed.strip_prefix('*') {
                stripped.trim()
            } else {
                trimmed
            }
        })
        .collect::<Vec<&str>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Extract a function/method signature from the node.
fn extract_signature(node: Node, source: &[u8], kind: &str) -> Option<String> {
    match kind {
        "function_definition" | "function_declaration" | "function_item"
        | "method_definition" | "method_declaration" => {
            // Extract from name start to body/block start
            let name_node = node.child_by_field_name("name")
                .or_else(|| {
                    let mut cursor = node.walk();
                    let found = node.children(&mut cursor)
                        .find(|&child| child.kind() == "identifier" || child.kind() == "property_identifier");
                    found
                })?;

            let params_node = node.child_by_field_name("parameters");

            if let Some(params) = params_node {
                let param_text = node_text(params, source)?;
                let name = node_text(name_node, source)?;
                Some(format!("{}{}", name, param_text))
            } else {
                let name = node_text(name_node, source)?;
                Some(name)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_functions_and_classes() {
        let parser = CodeParser::new();
        let lang = parser.get_language("python").unwrap();
        let source = r#"
"""Module docstring."""

def hello():
    """Say hello."""
    pass

class Greeter:
    """A greeter class."""
    
    def greet(self, name: str) -> str:
        """Greet someone."""
        return f"Hello, {name}"

import os
from collections import defaultdict
"#;
        let symbols = parser
            .parse_source(source, "python", lang, "test.py")
            .unwrap();

        let names: Vec<String> = symbols.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(names.contains(&"hello".to_string()), "Expected 'hello' function");
        assert!(names.contains(&"Greeter".to_string()), "Expected 'Greeter' class");
        assert!(names.contains(&"greet".to_string()), "Expected 'greet' method");

        // Check imports
        let import_count = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Import)
            .count();
        assert!(import_count >= 2, "Expected at least 2 imports, got {}", import_count);

        // Verify doc comments
        let hello_sym = symbols.iter().find(|s| s.symbol_name == "hello").unwrap();
        assert!(hello_sym.doc_comment.is_some());
        let doc = hello_sym.doc_comment.as_ref().unwrap();
        assert!(doc.contains("Say hello"), "Doc comment: {}", doc);

        let greet_sym = symbols.iter().find(|s| s.symbol_name == "greet").unwrap();
        assert!(greet_sym.signature.is_some());
        assert!(greet_sym.symbol_kind == SymbolKind::Method);
    }

    #[test]
    fn test_parse_rust_structs_and_traits() {
        let parser = CodeParser::new();
        let lang = parser.get_language("rust").unwrap();
        let source = r#"
/// A point in 2D space.
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A drawable trait.
pub trait Drawable {
    fn draw(&self) -> String;
}

impl Point {
    fn new() -> Self {
        Point { x: 0.0, y: 0.0 }
    }
}

pub enum Color {
    Red,
    Green,
    Blue,
}

use std::collections::HashMap;
"#;
        let symbols = parser
            .parse_source(source, "rust", lang, "test.rs")
            .unwrap();

        let names: Vec<String> = symbols.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(
            names.contains(&"Point".to_string()),
            "Expected 'Point' struct, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Drawable".to_string()),
            "Expected 'Drawable' trait"
        );
        assert!(
            names.contains(&"Color".to_string()),
            "Expected 'Color' enum"
        );

        // Check struct doc comment
        let point_sym = symbols.iter().find(|s| s.symbol_name == "Point").unwrap();
        assert_eq!(point_sym.symbol_kind, SymbolKind::Struct);
        assert!(point_sym.doc_comment.is_some());

        // Check use (import)
        let import_count = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Import)
            .count();
        assert!(import_count >= 1, "Expected at least 1 import");

        // Check new method is extracted from impl block
        assert!(
            names.contains(&"new".to_string()),
            "Expected 'new' function from impl"
        );
    }

    #[test]
    fn test_parse_typescript_interfaces() {
        let parser = CodeParser::new();
        let lang = parser.get_language("typescript").unwrap();
        let source = r#"
interface User {
    id: number;
    name: string;
}

type Status = "active" | "inactive";

class UserService {
    private users: User[] = [];

    getUser(id: number): User | undefined {
        return this.users.find(u => u.id === id);
    }
}

export function createUser(name: string): User {
    return { id: 1, name };
}

import { Injectable } from '@angular/core';
"#;
        let symbols = parser
            .parse_source(source, "typescript", lang, "test.ts")
            .unwrap();

        let names: Vec<String> = symbols.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(names.contains(&"User".to_string()), "Expected 'User' interface");
        assert!(names.contains(&"Status".to_string()), "Expected 'Status' type alias");
        assert!(
            names.contains(&"UserService".to_string()),
            "Expected 'UserService' class"
        );
        assert!(
            names.contains(&"getUser".to_string()),
            "Expected 'getUser' method"
        );
        assert!(
            names.contains(&"createUser".to_string()),
            "Expected 'createUser' function"
        );

        // Check kinds
        let user_sym = symbols.iter().find(|s| s.symbol_name == "User").unwrap();
        assert_eq!(user_sym.symbol_kind, SymbolKind::Interface);

        let status_sym = symbols.iter().find(|s| s.symbol_name == "Status").unwrap();
        assert_eq!(status_sym.symbol_kind, SymbolKind::TypeAlias);

        let import_count = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Import)
            .count();
        assert!(import_count >= 1);
    }

    #[test]
    fn test_parse_with_syntax_errors_graceful() {
        let parser = CodeParser::new();
        let lang = parser.get_language("python").unwrap();

        // Valid syntax at top, syntax error later
        let source = r#"
def valid_func():
    pass

def broken_func(
    # Missing closing paren and body - syntax error
"#;
        let symbols = parser
            .parse_source(source, "python", lang, "broken.py")
            .unwrap();

        // Should still extract valid_func even though broken_func causes partial parse
        assert!(
            !symbols.is_empty(),
            "Should extract at least valid_func symbol"
        );
        let names: Vec<String> = symbols.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(
            names.contains(&"valid_func".to_string()),
            "Should find valid_func"
        );
    }

    #[test]
    fn test_detect_language() {
        let parser = CodeParser::new();
        assert_eq!(
            parser.detect_language(Path::new("main.py")),
            Some("python")
        );
        assert_eq!(
            parser.detect_language(Path::new("lib.rs")),
            Some("rust")
        );
        assert_eq!(
            parser.detect_language(Path::new("app.ts")),
            Some("typescript")
        );
        assert_eq!(
            parser.detect_language(Path::new("Component.tsx")),
            Some("tsx")
        );
        assert_eq!(
            parser.detect_language(Path::new("main.go")),
            Some("go")
        );
        assert_eq!(
            parser.detect_language(Path::new("Main.java")),
            Some("java")
        );
        assert_eq!(parser.detect_language(Path::new("README.md")), None);
    }
}
