use std::fmt::Write as _;

use ion_ai::ImageContent;

use super::Document;

pub(super) struct Text(String);

impl Text {
    pub(super) fn new() -> Self {
        Self("Ion transcript\n".into())
    }

    fn indented(&mut self, value: &str, prefix: &str) {
        for line in value.lines() {
            let _ = writeln!(self.0, "{prefix}{line}");
        }
    }
}

impl Document for Text {
    fn metadata(&mut self, label: &str, value: &str) {
        let _ = writeln!(self.0, "{label}: {value}");
    }
    fn heading(&mut self, value: &str) {
        let _ = writeln!(self.0, "\n{value}");
    }
    fn label(&mut self, value: &str) {
        let _ = writeln!(self.0, "  {value}");
    }
    fn literal(&mut self, value: &str) {
        self.indented(value, "  ");
    }
    fn markdown(&mut self, value: &str) {
        self.literal(value);
    }
    fn thinking(&mut self, value: &str) {
        self.label("Thinking:");
        self.indented(value, "    ");
    }
    fn image(&mut self, image: &ImageContent) {
        self.label(&format!("[image: {}]", image.mime_type().as_str()));
    }
    fn finish(self) -> String {
        self.0
            .chars()
            .map(|ch| {
                if ch == '\n' || ch == '\t' || !ch.is_control() {
                    ch
                } else {
                    '�'
                }
            })
            .collect()
    }
}
