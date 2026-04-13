use crate::ProviderError;

const SENTINEL_BEGIN: &str = "<<<SWITCHYARD_JSON_BEGIN>>>";
const SENTINEL_END: &str = "<<<SWITCHYARD_JSON_END>>>";

/// Extract all JSON blocks delimited by sentinel markers from provider text output.
pub fn extract_sentinel_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut search_from = 0;

    while let Some(begin) = text[search_from..].find(SENTINEL_BEGIN) {
        let json_start = search_from + begin + SENTINEL_BEGIN.len();
        if let Some(end) = text[json_start..].find(SENTINEL_END) {
            let json_end = json_start + end;
            let block = text[json_start..json_end].trim();
            if !block.is_empty() {
                blocks.push(block);
            }
            search_from = json_end + SENTINEL_END.len();
        } else {
            break;
        }
    }

    blocks
}

/// Strip all sentinel blocks from text, returning the surrounding prose.
pub fn strip_sentinel_blocks(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut search_from = 0;

    while let Some(begin) = text[search_from..].find(SENTINEL_BEGIN) {
        let abs_begin = search_from + begin;
        result.push_str(&text[search_from..abs_begin]);
        let json_start = abs_begin + SENTINEL_BEGIN.len();
        if let Some(end) = text[json_start..].find(SENTINEL_END) {
            search_from = json_start + end + SENTINEL_END.len();
        } else {
            // Unclosed block — keep the rest as-is
            search_from = abs_begin + SENTINEL_BEGIN.len();
        }
    }
    result.push_str(&text[search_from..]);

    // Collapse multiple blank lines left by removal
    let trimmed = result.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// Parse the first sentinel-delimited JSON block into a typed value.
pub fn parse_sentinel_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, ProviderError> {
    let blocks = extract_sentinel_blocks(text);
    let block = blocks
        .first()
        .ok_or_else(|| ProviderError::InvalidOutput("no sentinel JSON block found".to_string()))?;

    serde_json::from_str(block)
        .map_err(|e| ProviderError::InvalidOutput(format!("sentinel JSON parse error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_single_block() {
        let text = r#"some text before
<<<SWITCHYARD_JSON_BEGIN>>>
{"key": "value"}
<<<SWITCHYARD_JSON_END>>>
some text after"#;
        let blocks = extract_sentinel_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_multiple_blocks() {
        let text = r#"header
<<<SWITCHYARD_JSON_BEGIN>>>
{"a": 1}
<<<SWITCHYARD_JSON_END>>>
middle
<<<SWITCHYARD_JSON_BEGIN>>>
{"b": 2}
<<<SWITCHYARD_JSON_END>>>
footer"#;
        let blocks = extract_sentinel_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], r#"{"a": 1}"#);
        assert_eq!(blocks[1], r#"{"b": 2}"#);
    }

    #[test]
    fn no_blocks_returns_empty() {
        let blocks = extract_sentinel_blocks("just plain text");
        assert!(blocks.is_empty());
    }

    #[test]
    fn unclosed_block_is_ignored() {
        let text = "<<<SWITCHYARD_JSON_BEGIN>>>\n{\"key\": 1}\nno end marker";
        let blocks = extract_sentinel_blocks(text);
        assert!(blocks.is_empty());
    }

    #[test]
    fn parse_typed_json() {
        #[derive(Debug, serde::Deserialize, PartialEq)]
        struct Simple {
            key: String,
        }
        let text = r#"blah
<<<SWITCHYARD_JSON_BEGIN>>>
{"key": "hello"}
<<<SWITCHYARD_JSON_END>>>
blah"#;
        let parsed: Simple = parse_sentinel_json(text).unwrap();
        assert_eq!(
            parsed,
            Simple {
                key: "hello".to_string()
            }
        );
    }

    #[test]
    fn parse_missing_block_returns_error() {
        let result: Result<serde_json::Value, _> = parse_sentinel_json("no blocks here");
        assert!(result.is_err());
    }

    #[test]
    fn strip_removes_sentinel_blocks() {
        let text = r#"I need help reviewing this.

<<<SWITCHYARD_JSON_BEGIN>>>
{"type":"delegate","requests":[{"id":"t1","provider":"claude"}]}
<<<SWITCHYARD_JSON_END>>>

Some trailing text."#;
        let stripped = strip_sentinel_blocks(text);
        assert!(!stripped.contains("SWITCHYARD_JSON"));
        assert!(!stripped.contains("delegate"));
        assert!(stripped.contains("I need help reviewing this."));
        assert!(stripped.contains("Some trailing text."));
    }

    #[test]
    fn strip_only_sentinel_returns_empty() {
        let text = "<<<SWITCHYARD_JSON_BEGIN>>>\n{}\n<<<SWITCHYARD_JSON_END>>>";
        let stripped = strip_sentinel_blocks(text);
        assert!(stripped.is_empty());
    }

    #[test]
    fn strip_no_sentinel_returns_original() {
        let text = "just plain text";
        assert_eq!(strip_sentinel_blocks(text), text);
    }
}
