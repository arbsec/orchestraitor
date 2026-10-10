//! Tree-sitter grammar feature dispatch.

use std::path::Path;

use tree_sitter::Language;

use crate::{ContextError, LanguageKind};

/// Tree-sitter grammar and query pair selected for one source language.
pub struct LanguageSpec {
    /// Language classification.
    pub kind: LanguageKind,
    /// Human-readable language name.
    pub name: &'static str,
    /// Tree-sitter language handle.
    pub language: Language,
    /// Symbol extraction query.
    pub query: &'static str,
}

/// Returns the enabled language spec for a repository path.
pub fn spec_for_path(path: &Path) -> Option<Result<LanguageSpec, ContextError>> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rs") => Some({
            #[cfg(feature = "grammar-rust")]
            {
                Ok(rust())
            }
            #[cfg(not(feature = "grammar-rust"))]
            {
                Err(ContextError::LanguageSetup { language: "rust" })
            }
        }),
        Some("ts") => Some(typescript(LanguageKind::TypeScript)),
        Some("tsx") => Some(typescript(LanguageKind::Tsx)),
        Some("py" | "pyi") => Some({
            #[cfg(feature = "grammar-python")]
            {
                Ok(python())
            }
            #[cfg(not(feature = "grammar-python"))]
            {
                Err(ContextError::LanguageSetup { language: "python" })
            }
        }),
        Some("js" | "mjs" | "cjs") => Some(javascript(LanguageKind::JavaScript)),
        Some("jsx") => Some(javascript(LanguageKind::Jsx)),
        Some("go") => Some({
            #[cfg(feature = "grammar-go")]
            {
                Ok(go())
            }
            #[cfg(not(feature = "grammar-go"))]
            {
                Err(ContextError::LanguageSetup { language: "go" })
            }
        }),
        Some("sh" | "bash") => Some({
            #[cfg(feature = "grammar-bash")]
            {
                Ok(bash())
            }
            #[cfg(not(feature = "grammar-bash"))]
            {
                Err(ContextError::LanguageSetup { language: "bash" })
            }
        }),
        _ => None,
    }
}

#[cfg(feature = "grammar-rust")]
fn rust() -> LanguageSpec {
    LanguageSpec {
        kind: LanguageKind::Rust,
        name: "rust",
        language: tree_sitter_rust::LANGUAGE.into(),
        query: r"
            (function_item name: (identifier) @name) @definition.function
            (struct_item name: (type_identifier) @name) @definition.type
            (enum_item name: (type_identifier) @name) @definition.type
            (trait_item name: (type_identifier) @name) @definition.type
            (type_item name: (type_identifier) @name) @definition.type
            (mod_item name: (identifier) @name) @definition.module
            (const_item name: (identifier) @name) @definition.variable
            (static_item name: (identifier) @name) @definition.variable
        ",
    }
}

#[cfg(feature = "grammar-typescript")]
fn typescript(kind: LanguageKind) -> Result<LanguageSpec, ContextError> {
    let language = match kind {
        LanguageKind::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        LanguageKind::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        LanguageKind::Rust
        | LanguageKind::Python
        | LanguageKind::JavaScript
        | LanguageKind::Jsx
        | LanguageKind::Go
        | LanguageKind::Bash => {
            return Err(ContextError::LanguageSetup {
                language: "typescript",
            });
        }
    };
    Ok(LanguageSpec {
        kind,
        name: "typescript",
        language,
        query: r"
            (function_declaration name: (identifier) @name) @definition.function
            (method_definition name: (property_identifier) @name) @definition.method
            (class_declaration name: (type_identifier) @name) @definition.type
            (interface_declaration name: (type_identifier) @name) @definition.type
            (type_alias_declaration name: (type_identifier) @name) @definition.type
            (lexical_declaration (variable_declarator name: (identifier) @name)) @definition.variable
        ",
    })
}

#[cfg(not(feature = "grammar-typescript"))]
fn typescript(_: LanguageKind) -> Result<LanguageSpec, ContextError> {
    Err(ContextError::LanguageSetup {
        language: "typescript",
    })
}

#[cfg(feature = "grammar-python")]
fn python() -> LanguageSpec {
    LanguageSpec {
        kind: LanguageKind::Python,
        name: "python",
        language: tree_sitter_python::LANGUAGE.into(),
        query: r"
            (function_definition name: (identifier) @name) @definition.function
            (class_definition name: (identifier) @name) @definition.type
        ",
    }
}

#[cfg(feature = "grammar-javascript")]
fn javascript(kind: LanguageKind) -> Result<LanguageSpec, ContextError> {
    let language = match kind {
        LanguageKind::JavaScript | LanguageKind::Jsx => tree_sitter_javascript::LANGUAGE.into(),
        LanguageKind::Rust
        | LanguageKind::TypeScript
        | LanguageKind::Tsx
        | LanguageKind::Python
        | LanguageKind::Go
        | LanguageKind::Bash => {
            return Err(ContextError::LanguageSetup {
                language: "javascript",
            });
        }
    };
    Ok(LanguageSpec {
        kind,
        name: "javascript",
        language,
        query: r"
            (function_declaration name: (identifier) @name) @definition.function
            (method_definition name: (property_identifier) @name) @definition.method
            (class_declaration name: (identifier) @name) @definition.type
            (lexical_declaration (variable_declarator name: (identifier) @name)) @definition.variable
        ",
    })
}

#[cfg(not(feature = "grammar-javascript"))]
fn javascript(_: LanguageKind) -> Result<LanguageSpec, ContextError> {
    Err(ContextError::LanguageSetup {
        language: "javascript",
    })
}

#[cfg(feature = "grammar-go")]
fn go() -> LanguageSpec {
    LanguageSpec {
        kind: LanguageKind::Go,
        name: "go",
        language: tree_sitter_go::LANGUAGE.into(),
        query: r"
            (function_declaration name: (identifier) @name) @definition.function
            (method_declaration name: (field_identifier) @name) @definition.method
            (type_declaration (type_spec name: (type_identifier) @name)) @definition.type
            (const_declaration (const_spec name: (identifier) @name)) @definition.variable
        ",
    }
}

#[cfg(feature = "grammar-bash")]
fn bash() -> LanguageSpec {
    LanguageSpec {
        kind: LanguageKind::Bash,
        name: "bash",
        language: tree_sitter_bash::LANGUAGE.into(),
        query: r"(function_definition name: (word) @name) @definition.function",
    }
}
