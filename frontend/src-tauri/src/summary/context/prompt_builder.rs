//! Renders attachments into the `<attachments>` XML block injected into the
//! LLM user prompt.

use crate::summary::context::types::AttachmentContent;

/// Escape the five XML special chars in attribute/text content.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Returns the `\n\n<attachments>...</attachments>` block, or an empty string
/// when there are no attachments.
pub fn render_attachments_block(attachments: &[AttachmentContent]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n<attachments>\n");
    for a in attachments {
        let truncated_attr = if a.truncated { " truncated=\"true\"" } else { "" };
        out.push_str(&format!(
            "<file name=\"{}\"{}>\n{}\n</file>\n",
            xml_escape(&a.display_name),
            truncated_attr,
            a.content,
        ));
    }
    out.push_str("</attachments>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_attachments_returns_empty_string() {
        assert_eq!(render_attachments_block(&[]), "");
    }

    #[test]
    fn renders_single_file() {
        let block = render_attachments_block(&[AttachmentContent {
            display_name: "brief.md".to_string(),
            content: "hello".to_string(),
            truncated: false,
        }]);
        assert!(block.contains("<file name=\"brief.md\">\nhello\n</file>"));
        assert!(block.starts_with("\n\n<attachments>\n"));
        assert!(block.ends_with("</attachments>"));
    }

    #[test]
    fn renders_truncated_attribute_when_flag_set() {
        let block = render_attachments_block(&[AttachmentContent {
            display_name: "big.txt".to_string(),
            content: "...".to_string(),
            truncated: true,
        }]);
        assert!(block.contains("<file name=\"big.txt\" truncated=\"true\">"));
    }

    #[test]
    fn escapes_xml_specials_in_display_name() {
        let block = render_attachments_block(&[AttachmentContent {
            display_name: "a<b>&c\"d.txt".to_string(),
            content: "x".to_string(),
            truncated: false,
        }]);
        assert!(block.contains("name=\"a&lt;b&gt;&amp;c&quot;d.txt\""));
    }

    #[test]
    fn renders_multiple_files_in_order() {
        let block = render_attachments_block(&[
            AttachmentContent {
                display_name: "first.md".to_string(),
                content: "1".to_string(),
                truncated: false,
            },
            AttachmentContent {
                display_name: "second.md".to_string(),
                content: "2".to_string(),
                truncated: false,
            },
        ]);
        let first_at = block.find("first.md").unwrap();
        let second_at = block.find("second.md").unwrap();
        assert!(first_at < second_at);
    }
}
