use crate::memory::{IndexingWorker, MemoryType};
use anyhow::{Result, anyhow};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

pub struct Explorer {
    indexing_worker: Arc<IndexingWorker>,
    working_dir: PathBuf,
}

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @function)
(impl_item (function_item name: (identifier) @function))
(struct_item name: (type_identifier) @struct)
(enum_item name: (type_identifier) @enum)
(trait_item name: (type_identifier) @trait)
(type_item name: (type_identifier) @type)
(mod_item name: (identifier) @module)
"#;

const TS_QUERY: &str = r#"
(function_declaration name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(method_definition name: (property_identifier) @method)
(class_declaration name: (type_identifier) @class)
(interface_declaration name: (type_identifier) @interface)
(type_alias_declaration name: (type_identifier) @type)
(enum_declaration name: (identifier) @enum)
(module name: (identifier) @module)
"#;

const PY_QUERY: &str = r#"
(function_definition name: (identifier) @function)
(class_definition name: (identifier) @class)
"#;

impl Explorer {
    pub fn new(indexing_worker: Arc<IndexingWorker>, working_dir: PathBuf) -> Self {
        Self {
            indexing_worker,
            working_dir,
        }
    }

    /// Index a single file (lazy indexing).
    pub async fn index_file(&self, path: &Path) -> Result<()> {
        let relative_path = path
            .strip_prefix(&self.working_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        info!("Explorer: Lazy indexing {}", relative_path);

        // Index the file existence
        self.index_item(
            &format!("File: {}", relative_path),
            MemoryType::Semantic,
            serde_json::json!({"type": "file", "path": relative_path}),
        )
        .await?;

        // Extract symbols
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let lang_and_query = match ext {
                "rs" => Some((tree_sitter_rust::LANGUAGE.into(), RUST_QUERY)),
                "ts" | "tsx" => {
                    Some((tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), TS_QUERY))
                }
                "js" | "jsx" => Some((tree_sitter_javascript::LANGUAGE.into(), TS_QUERY)), // Reuse TS query for JS
                "py" => Some((tree_sitter_python::LANGUAGE.into(), PY_QUERY)),
                _ => None,
            };

            if let Some((lang, query_str)) = lang_and_query {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Err(e) = self
                        .extract_and_index_symbols_ts(&relative_path, &content, lang, query_str)
                        .await
                    {
                        warn!(
                            "Explorer: Failed to extract symbols from {}: {}",
                            relative_path, e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Crawl a specific path and index file structures.
    pub async fn index_path(&self, sub_path: &Path) -> Result<()> {
        let absolute_path = if sub_path.is_absolute() {
            sub_path.to_path_buf()
        } else {
            self.working_dir.join(sub_path)
        };

        let walker = WalkBuilder::new(&absolute_path)
            .hidden(false)
            .git_ignore(true)
            .build();

        info!(
            "Explorer: Starting targeted indexing in {:?}",
            absolute_path
        );

        for result in walker {
            if let Ok(entry) = result {
                if entry.file_type().map_or(false, |ft| ft.is_file()) {
                    let _ = self.index_file(entry.path()).await;
                }
            }
        }

        Ok(())
    }

    async fn extract_and_index_symbols_ts(
        &self,
        path: &str,
        content: &str,
        lang: Language,
        query_str: &str,
    ) -> Result<()> {
        let lang_name = self.get_lang_name_from_path(path);
        let symbols = {
            let mut parser = Parser::new();
            parser.set_language(&lang)?;

            let tree = parser
                .parse(content, None)
                .ok_or_else(|| anyhow!("Failed to parse file: {}", path))?;
            let query = Query::new(&lang, query_str)?;
            let mut cursor = QueryCursor::new();

            let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
            let mut captured_symbols = Vec::new();

            while let Some(m) = matches.next() {
                for capture in m.captures {
                    let name = capture.node.utf8_text(content.as_bytes())?.to_string();
                    let symbol_type = query.capture_names()[capture.index as usize].to_string();
                    captured_symbols.push((name, symbol_type));
                }
            }
            captured_symbols
        };

        for (name, symbol_type) in symbols {
            self.index_item(
                &format!("{} {} in {}", symbol_type, name, path),
                MemoryType::Semantic,
                serde_json::json!({
                    "type": symbol_type,
                    "name": name,
                    "path": path,
                    "lang": lang_name
                }),
            )
            .await?;
        }

        Ok(())
    }

    fn get_lang_name_from_path(&self, path: &str) -> &'static str {
        let path = Path::new(path);
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust",
            Some("ts") | Some("tsx") => "typescript",
            Some("py") => "python",
            Some("js") | Some("jsx") => "javascript",
            _ => "unknown",
        }
    }

    async fn index_item(
        &self,
        text: &str,
        r#type: MemoryType,
        metadata: serde_json::Value,
    ) -> Result<()> {
        self.indexing_worker
            .index(text.to_string(), r#type, metadata)
            .await?;
        Ok(())
    }
}
