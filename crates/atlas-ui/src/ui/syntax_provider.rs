use anyhow::Result;
use gpui::{App, Task, Window};
use gpui_component::input::{DocumentRangeSemanticTokensProvider, Rope, RopeExt};
use lsp_types::{SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend};
use std::ops::Range;

pub struct AtlasSyntaxTokensProvider {
    ext: String,
}

impl AtlasSyntaxTokensProvider {
    pub fn new(ext: impl Into<String>) -> Self {
        Self { ext: ext.into() }
    }
}

impl DocumentRangeSemanticTokensProvider for AtlasSyntaxTokensProvider {
    fn legend(&self) -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types: vec![
                SemanticTokenType::KEYWORD,        // 0: keyword
                SemanticTokenType::TYPE,           // 1: type
                SemanticTokenType::STRING,         // 2: string
                SemanticTokenType::NUMBER,         // 3: number
                SemanticTokenType::COMMENT,        // 4: comment
                SemanticTokenType::new("preproc"), // 5: preproc
                SemanticTokenType::OPERATOR,       // 6: operator
                SemanticTokenType::VARIABLE,       // 7: variable
            ],
            token_modifiers: vec![],
        }
    }

    fn semantic_tokens(
        &self,
        text: &Rope,
        _range: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<SemanticTokens>> {
        let mut data = Vec::new();
        let mut in_block_comment = false;
        let mut prev_line = 0u32;
        let mut prev_col = 0u32;
        let mut first = true;

        for (line_idx, line) in text.iter_lines().enumerate() {
            let line_u32 = line_idx as u32;
            let line_str = line.to_string();
            let clean_line = line_str.trim_end_matches(&['\r', '\n'][..]);
            let tokens = atlas_core::fs::preview::syntax::tokenize_line(
                clean_line,
                &self.ext,
                &mut in_block_comment,
            );

            for tok in tokens {
                // Plain identifiers keep the editor foreground; the theme's
                // `variable` color is nearly invisible on dark surfaces.
                if matches!(
                    tok.kind,
                    atlas_core::fs::preview::syntax::TokenKind::Whitespace
                        | atlas_core::fs::preview::syntax::TokenKind::Identifier
                ) {
                    continue;
                }

                let token_type = match tok.kind {
                    atlas_core::fs::preview::syntax::TokenKind::Keyword
                    | atlas_core::fs::preview::syntax::TokenKind::DiffHeader => 0,
                    atlas_core::fs::preview::syntax::TokenKind::Type => 1,
                    atlas_core::fs::preview::syntax::TokenKind::String
                    | atlas_core::fs::preview::syntax::TokenKind::DiffAdd => 2,
                    atlas_core::fs::preview::syntax::TokenKind::Number
                    | atlas_core::fs::preview::syntax::TokenKind::LogWarn => 3,
                    atlas_core::fs::preview::syntax::TokenKind::Comment
                    | atlas_core::fs::preview::syntax::TokenKind::LogInfo => 4,
                    atlas_core::fs::preview::syntax::TokenKind::Preprocessor
                    | atlas_core::fs::preview::syntax::TokenKind::DiffRemove
                    | atlas_core::fs::preview::syntax::TokenKind::LogError => 5,
                    atlas_core::fs::preview::syntax::TokenKind::Symbol => 6,
                    atlas_core::fs::preview::syntax::TokenKind::Identifier => 7,
                    _ => 7,
                };

                let col_start = tok.range.start as u32;
                let length = tok.range.len() as u32;
                if length == 0 {
                    continue;
                }

                let (delta_line, delta_start) = if first {
                    first = false;
                    (line_u32, col_start)
                } else if line_u32 > prev_line {
                    (line_u32 - prev_line, col_start)
                } else {
                    (0, col_start.saturating_sub(prev_col))
                };

                prev_line = line_u32;
                prev_col = col_start;

                data.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length,
                    token_type,
                    token_modifiers_bitset: 0,
                });
            }
        }

        Task::ready(Ok(SemanticTokens {
            result_id: None,
            data,
        }))
    }
}
